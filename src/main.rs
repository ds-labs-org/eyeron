use eyeron::error::{EyeronError, Result};
use eyeron::printing::{document_debug, rdf_result_to_string, result_to_string};
use eyeron::proof_writer::proof_to_n3;
use eyeron::reasoner::{reason, PreparedReasoner, ReasonerOptions};
use eyeron::Document;
use eyeron::{
    fuse_report, is_rdf_message_log, parse_n3, parse_n3_with_source, parse_rdf12, parse_rdf_message_log,
    RdfFormat,
};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use ureq::ResponseExt;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Default)]
struct CliOptions {
    ast: bool,
    proof: bool,
    rdf: bool,
    stream: bool,
    stream_messages: bool,
    base_iri: Option<String>,
    max_backward_depth: Option<usize>,
    files: Vec<String>,
    /// `--check-proof FILE`: check that proof document against the program
    /// given as the positional arguments (`docs/proof-checking.md`).
    check_proof: Option<String>,
}

/// sysexits.h EX_DATAERR: the input and the rules together made a forbidden
/// situation provable, rather than anything going wrong with the run.
const INFERENCE_FUSE_EXIT_CODE: i32 = 65;

fn main() {
    if let Err(err) = run() {
        eprintln!("eyeron: {}", err);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        print_help();
        return Ok(());
    }
    let opt = parse_args(args)?;

    if opt.stream {
        eprintln!(
            "warning: --stream is accepted; Eyeron currently emits after the fixpoint is reached"
        );
    }
    if opt.stream_messages {
        return run_stream_messages(&opt);
    }
    let sources = read_sources(&opt.files)?;

    if let Some(path) = &opt.check_proof {
        return run_check_proof(path, &sources);
    }

    let mut merged = Document::new();
    for (label, text) in &sources {
        let path_base = if label == "<stdin>" {
            None
        } else if is_http_url(label) {
            Some(label.clone())
        } else {
            path_to_file_iri(label).ok()
        };
        let base = opt.base_iri.as_deref().or(path_base.as_deref());
        let parsed = if is_rdf_message_log(text) {
            parse_rdf_message_log(text, base)
        } else if let Some(format) = rdf_format_for_source(label, opt.rdf)? {
            parse_rdf12(text, base, format)
        } else if opt.proof {
            parse_n3_with_source(text, base, Some(label))
        } else {
            parse_n3(text, base)
        };
        match parsed {
            Ok(doc) => merged.merge(doc),
            Err(err) => return Err(EyeronError::new(err.with_source_location(text, label))),
        }
    }

    if opt.ast {
        print!("{}", document_debug(&merged));
        return Ok(());
    }

    let reasoner_options = cli_reasoner_options(&opt, opt.proof);
    let result = reason(&merged, &reasoner_options);
    // A fired fuse is the answer: the rules forbade a situation and the data
    // made it provable. Report which rule, and leave with sysexits' EX_DATAERR
    // (65) so a caller can tell it from an ordinary failure.
    if let Some(fuse) = &result.fuse {
        print!("{}", fuse_report(&merged.prefixes, fuse));
        std::process::exit(INFERENCE_FUSE_EXIT_CODE);
    }
    if let Some(summary) = result.incomplete_summary() {
        return Err(EyeronError::new(summary));
    }
    if opt.proof {
        print!("{}", proof_to_n3(&merged.prefixes, &result));
    } else if opt.rdf {
        print!(
            "{}",
            rdf_result_to_string(&merged.prefixes, &result.derived)
        );
    } else {
        print!("{}", result_to_string(&merged.prefixes, &result.derived));
    }
    Ok(())
}

fn run_stream_messages(opt: &CliOptions) -> Result<()> {
    if !opt.rdf {
        return Err(EyeronError::new("--stream-messages requires -r/--rdf"));
    }
    if opt.ast || opt.proof || opt.stream {
        return Err(EyeronError::new(
            "--stream-messages cannot be combined with --ast, --proof, or --stream",
        ));
    }

    let mut program = Document::new();
    let mut message_sources = Vec::new();
    for source in &opt.files {
        if is_http_url(source) {
            message_sources.push(source.clone());
            continue;
        }
        let text = if source == "-" {
            let mut text = String::new();
            io::stdin().read_to_string(&mut text)?;
            text
        } else {
            fs::read_to_string(source)?
        };
        if is_rdf_message_log(&text) {
            message_sources.push(source.clone());
        } else {
            let base = opt.base_iri.as_deref().or(None);
            let parsed = parse_n3_with_source(&text, base, Some(source))
                .map_err(|err| EyeronError::new(err.with_source_location(&text, source)))?;
            program.merge(parsed);
        }
    }
    if message_sources.is_empty() {
        return Err(EyeronError::new(
            "--stream-messages did not find any RDF Message Log input",
        ));
    }

    let reasoner_options = cli_reasoner_options(opt, false);
    // Built once for the whole stream: PreparedReasoner keeps the agenda index
    // it builds from `program`'s rules and reuses it for every message,
    // instead of `reason()` recomputing that index from scratch per message.
    //
    // Measured against the real alma-rdf-messages.n3 rule set padded out to
    // 1,615 static rules and run over 1,000 synthetic messages: plain
    // `reason()` (rebuilding the agenda index from `doc.rules` on every
    // message) took ~8.4s. Switching to `PreparedReasoner` alone, while it
    // still cloned the whole static rule set and agenda index into every
    // call, made this *worse* (~9.9s) -- pure per-call cloning overhead that
    // the rebuild-from-scratch path didn't pay. `PreparedReasoner::reason`
    // was then changed to hold its rule set and agenda index as `Cow`, so a
    // call only clones them if a rule reifies itself as data mid-run and has
    // to be appended (rare); the common case borrows them for the whole
    // call. That brings this same benchmark back to ~8.3-8.9s -- essentially
    // recovering the regression, not a net win over plain `reason()`, because
    // this workload's real cost is the backward search through the marc:
    // helper rules per message, not the rule-set clone. With a small rule
    // set (examples/alma-rdf-messages.n3's own ~400 rules, 100 messages)
    // neither path is measurably different (~0.5-0.7s either way).
    let prepared = PreparedReasoner::new(program);
    for source in message_sources {
        let base = opt.base_iri.clone().or_else(|| {
            if is_http_url(&source) {
                Some(source.clone())
            } else {
                path_to_file_iri(&source).ok()
            }
        });
        if is_http_url(&source) {
            let response = ureq::get(&source)
                .call()
                .map_err(|err| EyeronError::new(format!("failed to fetch {source}: {err}")))?;
            let final_url = response.get_uri().to_string();
            stream_message_reader(
                BufReader::new(response.into_body().into_reader()),
                &final_url,
                base.as_deref(),
                &prepared,
                &reasoner_options,
            )?;
        } else if source == "-" {
            return Err(EyeronError::new(
                "stdin RDF Message Logs cannot follow another stdin read",
            ));
        } else {
            let file = fs::File::open(&source)?;
            stream_message_reader(
                BufReader::new(file),
                &source,
                base.as_deref(),
                &prepared,
                &reasoner_options,
            )?;
        }
    }
    Ok(())
}

fn stream_message_reader<R: BufRead>(
    mut reader: R,
    label: &str,
    base: Option<&str>,
    prepared: &PreparedReasoner,
    reasoner_options: &ReasonerOptions,
) -> Result<()> {
    let mut directives = String::new();
    let mut message = String::new();
    let mut line = String::new();
    let mut saw_version = false;
    let mut saw_delimiter = false;
    let mut message_index = 1usize;

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line).map_err(|err| {
            EyeronError::new(format!("failed to read response from {label}: {err}"))
        })?;
        if bytes == 0 {
            break;
        }
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if (lower.starts_with("version ") || lower.starts_with("@version "))
            && lower.contains("-messages")
        {
            saw_version = true;
        } else if trimmed.eq_ignore_ascii_case("MESSAGE")
            || trimmed.eq_ignore_ascii_case("@message .")
        {
            run_one_message(
                prepared,
                &directives,
                &message,
                label,
                message_index,
                base,
                reasoner_options,
            )?;
            message.clear();
            message_index += 1;
            saw_delimiter = true;
        } else if trimmed.starts_with("PREFIX ")
            || trimmed.starts_with("prefix ")
            || trimmed.starts_with("BASE ")
            || trimmed.starts_with("base ")
        {
            if !directives.contains(&line) {
                directives.push_str(&line);
            }
        } else {
            message.push_str(&line);
        }
    }
    if !saw_version {
        return Err(EyeronError::new(format!(
            "not an RDF Message Log: missing VERSION \"*-messages\" directive in {label}"
        )));
    }
    if saw_delimiter || !message.trim().is_empty() {
        run_one_message(
            prepared,
            &directives,
            &message,
            label,
            message_index,
            base,
            reasoner_options,
        )?;
    }
    Ok(())
}

fn run_one_message(
    prepared: &PreparedReasoner,
    directives: &str,
    message: &str,
    label: &str,
    index: usize,
    base: Option<&str>,
    reasoner_options: &ReasonerOptions,
) -> Result<()> {
    let replay = format!("{directives}\nVERSION \"1.2-messages\"\n{message}");
    let message_label = format!("{label}#message-{index}");
    let parsed = parse_rdf_message_log(&replay, base)
        .map_err(|err| EyeronError::new(err.with_source_location(&replay, &message_label)))?;
    let result = prepared.reason(&parsed, reasoner_options);
    if let Some(summary) = result.incomplete_summary() {
        return Err(EyeronError::new(summary));
    }
    print!(
        "{}",
        rdf_result_to_string(&BTreeMap::new(), &result.derived)
    );
    io::stdout().flush()?;
    Ok(())
}

fn read_text_source(source: &str) -> Result<String> {
    if source == "-" {
        let mut s = String::new();
        io::stdin().read_to_string(&mut s)?;
        Ok(s)
    } else if is_http_url(source) {
        let response = ureq::get(source).call().map_err(|err| EyeronError::new(format!("failed to fetch {source}: {err}")))?;
        response.into_body().read_to_string().map_err(|err| EyeronError::new(format!("failed to read response from {source}: {err}")))
    } else {
        Ok(fs::read_to_string(source)?)
    }
}

/// `--check-proof`: read the proof document and check it against the
/// program, reporting the verdict the specification's §9 requires.
fn run_check_proof(path: &str, sources: &[(String, String)]) -> Result<()> {
    let proof = read_text_source(path)?;
    let source: String = sources.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>().join("\n");
    let label = sources.first().map(|(label, _)| label.clone()).unwrap_or_else(|| "<input>".to_string());

    let report = eyeron::proof_check_n3::check_proof(&source, &proof, &label)?;

    println!("{}", report.verdict());
    for (kind, count) in &report.counts {
        println!("  {} {}", count, kind);
    }
    for obligation in &report.obligations {
        println!("  trusted ({}): {}", obligation.kind, obligation.conclusion);
    }
    for failure in &report.failures {
        println!("  [{}] {} -- {}", failure.condition, failure.conclusion, failure.detail);
    }
    if !report.valid() {
        return Err(EyeronError::new(format!("{} is not a valid proof for the given program", path)));
    }
    Ok(())
}

fn cli_reasoner_options(opt: &CliOptions, proof: bool) -> ReasonerOptions {
    let mut options = ReasonerOptions {
        proof,
        ..ReasonerOptions::default()
    };
    if let Some(max_backward_depth) = opt.max_backward_depth {
        options.max_backward_depth = max_backward_depth;
    }
    options
}

fn parse_args(args: Vec<String>) -> Result<CliOptions> {
    let mut opt = CliOptions::default();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-v" | "--version" => {
                println!("{}", VERSION);
                std::process::exit(0);
            }
            "-a" | "--ast" => opt.ast = true,
            "-p" | "--proof" | "--proof-comments" => opt.proof = true,
            "-r" | "--rdf" => opt.rdf = true,
            "-s" | "--stream" => opt.stream = true,
            "--max-backward-depth" => {
                let flag = args[i].clone();
                i += 1;
                if i >= args.len() {
                    return Err(EyeronError::new(format!("{} requires a value", flag)));
                }
                opt.max_backward_depth = Some(args[i].parse::<usize>().map_err(|_| {
                    EyeronError::new(format!(
                        "{} requires a non-negative integer, got {}",
                        flag, args[i]
                    ))
                })?);
            }
            "--builtin" | "--store" | "--store-path" => {
                let flag = args[i].clone();
                i += 1;
                if i >= args.len() {
                    return Err(EyeronError::new(format!("{} requires a value", flag)));
                }
                eprintln!(
                    "warning: {} is accepted for CLI compatibility but not implemented in Eyeron",
                    flag
                );
            }
            "--stream-messages" => opt.stream_messages = true,
            "--check-proof" => {
                let flag = args[i].clone();
                i += 1;
                if i >= args.len() {
                    return Err(EyeronError::new(format!("{} requires a value", flag)));
                }
                opt.check_proof = Some(args[i].clone());
            }
            "--base-iri" | "--base" => {
                i += 1;
                if i >= args.len() {
                    return Err(EyeronError::new(format!(
                        "{} requires a value",
                        args[i - 1]
                    )));
                }
                opt.base_iri = Some(args[i].clone());
            }
            "--store-clear" | "--enforce-https" => {
                eprintln!(
                    "warning: {} is accepted for CLI compatibility but not implemented in Eyeron",
                    args[i]
                );
            }
            other if other.starts_with('-') && other != "-" => {
                return Err(EyeronError::new(format!("unknown option {}", other)));
            }
            file => opt.files.push(file.to_string()),
        }
        i += 1;
    }
    Ok(opt)
}

fn rdf_format_for_source(label: &str, rdf_mode: bool) -> Result<Option<RdfFormat>> {
    if label == "<stdin>" {
        return Ok(if rdf_mode {
            Some(RdfFormat::Turtle)
        } else {
            None
        });
    }

    let source_path = label.split(['?', '#']).next().unwrap_or(label);
    let extension = Path::new(source_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    let format = extension.as_deref().and_then(RdfFormat::parse);

    match (format, extension.as_deref(), rdf_mode) {
        (Some(format), _, _) => Ok(Some(format)),
        (None, Some("n3"), _) => Ok(None),
        (None, _, true) => Err(EyeronError::new(format!(
            "cannot infer RDF format for {}; use .ttl, .nt, .nq, .trig, or .n3",
            label
        ))),
        (None, _, false) => Ok(None),
    }
}

fn read_sources(files: &[String]) -> Result<Vec<(String, String)>> {
    if files.is_empty() {
        let mut s = String::new();
        io::stdin().read_to_string(&mut s)?;
        return Ok(vec![("<stdin>".to_string(), s)]);
    }

    let mut out = Vec::new();
    for f in files {
        if f == "-" {
            let mut s = String::new();
            io::stdin().read_to_string(&mut s)?;
            out.push(("<stdin>".to_string(), s));
        } else if is_http_url(f) {
            let response = ureq::get(f)
                .call()
                .map_err(|err| EyeronError::new(format!("failed to fetch {f}: {err}")))?;
            let final_url = response.get_uri().to_string();
            let text = response.into_body().read_to_string().map_err(|err| {
                EyeronError::new(format!("failed to read response from {f}: {err}"))
            })?;
            out.push((final_url, text));
        } else {
            out.push((f.clone(), fs::read_to_string(f)?));
        }
    }
    Ok(out)
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn path_to_file_iri(path: &str) -> std::result::Result<String, ()> {
    let abs = fs::canonicalize(Path::new(path)).map_err(|_| ())?;
    let s = abs.to_string_lossy().replace('\\', "/");
    Ok(format!(
        "file://{}{}",
        if s.starts_with('/') { "" } else { "/" },
        percent_encode_path(&s)
    ))
}

fn percent_encode_path(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b))
            }
            other => out.push_str(&format!("%{:02X}", other)),
        }
    }
    out
}

fn print_help() {
    println!("eyeron {}", VERSION);
    println!();
    println!("Usage: eyeron [options] [file-or-url|- ...]");
    println!();
    println!("Options:");
    println!("  -a, --ast                     Print parsed AST/debug form and exit");
    println!("  -p, --proof                   Enable N3 proof explanations");
    println!("  -r, --rdf                     Enable RDF/TriG input/output compatibility");
    println!("  -s, --stream                  Output is emitted after fixpoint");
    println!(
        "      --stream-messages         RDF Message Log input with VERSION/MESSAGE delimiters"
    );
    println!(
        "      --base-iri IRI            Base IRI used by parser modes that resolve relative IRIs"
    );
    println!(
        "      --max-backward-depth N    Maximum recursive backward-rule depth (default: {})",
        ReasonerOptions::default().max_backward_depth
    );
    println!("      --check-proof FILE        Check that proof document against the program");
    println!("  -v, --version                 Print version");
    println!("  -h, --help                    Show this help");
}
