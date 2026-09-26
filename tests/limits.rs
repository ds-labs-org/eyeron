//! Nesting limits for every parser, and (below) resource budgets for the
//! reasoner. Written before the implementation: each test names a failure a
//! hostile or accidental input caused, and states the behaviour we want.
//!
//! Stack overflows abort the whole process and cannot be caught, so every
//! test that feeds deep input runs on a 1 MiB thread (what a small thread or a
//! wasm module gets) and would kill the test binary if a limit were missing.

use eyeron::{
    parse_n3, parse_n3_with_options, parse_rdf12, parse_rdf12_with_options, parse_sparql_rl_with_options,
    ParserOptions, RdfFormat,
};

fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .expect("must not overflow the stack")
}

const P: &str = "@prefix : <http://e/> .\n";

/// `:a :b X .` with X nested `depth` levels deep, for each bracket kind.
fn n3_formula(depth: usize) -> String {
    format!("{P}:a :b {}:z{} .", "{ :x :y ".repeat(depth), " }".repeat(depth))
}
fn n3_list(depth: usize) -> String {
    format!("{P}:a :b {}1{} .", "( ".repeat(depth), " )".repeat(depth))
}
fn n3_bnode(depth: usize) -> String {
    format!("{P}:a :b {}:z{} .", "[ :p ".repeat(depth), " ]".repeat(depth))
}
fn n3_triple_term(depth: usize) -> String {
    format!("{P}{}:s{} :p :o .", "<< ".repeat(depth), " :p :o >>".repeat(depth))
}

/// A named builder of an input `depth` levels deep.
type Shape = (&'static str, fn(usize) -> String);

const N3_SHAPES: [Shape; 4] = [
    ("formula", n3_formula),
    ("list", n3_list),
    ("blank node property list", n3_bnode),
    ("triple term", n3_triple_term),
];

#[test]
fn n3_nesting_at_the_limit_parses_and_one_level_more_is_an_error() {
    let options = ParserOptions { max_nesting_depth: 8 };
    for (name, build) in N3_SHAPES {
        let at_limit = build(8);
        let over = build(9);
        parse_n3_with_options(&at_limit, None, None, &options)
            .unwrap_or_else(|e| panic!("{name} nested 8 deep must parse under a limit of 8: {e}"));
        let err = parse_n3_with_options(&over, None, None, &options)
            .expect_err(&format!("{name} nested 9 deep must be rejected under a limit of 8"));
        let message = err.to_string();
        assert!(message.contains("nesting") && message.contains('8'), "{name}: unhelpful error {message:?}");
    }
}

#[test]
fn the_default_limit_stops_hostile_nesting_before_the_stack_does() {
    // 100,000 levels: an unbounded parser overflows a 1 MiB stack at a few hundred.
    for (name, build) in N3_SHAPES {
        let source = build(100_000);
        let rejected = on_small_stack(move || parse_n3(&source, None).is_err());
        assert!(rejected, "{name} nested 100,000 deep must be rejected, not parsed");
    }
}

#[test]
fn the_default_limit_admits_documents_up_to_it_even_on_a_small_debug_stack() {
    // Real N3 nests a handful of levels; the default is 64. Every construct at
    // exactly the default must parse on a 1 MiB thread, in a debug build too
    // (measured: 77 levels of `{ }` is the most a debug build fits), and one
    // level more must be a clean error, not a crash.
    let max = ParserOptions::DEFAULT_MAX_NESTING_DEPTH;
    for (name, build) in N3_SHAPES {
        let (ok, over) = (build(max), build(max + 1));
        let parsed = on_small_stack(move || parse_n3(&ok, None).is_ok());
        assert!(parsed, "{name} nested {max} deep must parse under the default limit");
        let rejected = on_small_stack(move || parse_n3(&over, None).is_err());
        assert!(rejected, "{name} nested {} deep must be rejected", max + 1);
    }
}

#[test]
fn turtle_and_trig_share_the_limit() {
    let options = ParserOptions { max_nesting_depth: 6 };
    for format in [RdfFormat::Turtle, RdfFormat::Trig] {
        let ok = format!("{P}:a :b {}1{} .", "( ".repeat(6), " )".repeat(6));
        let over = format!("{P}:a :b {}1{} .", "( ".repeat(7), " )".repeat(7));
        parse_rdf12_with_options(&ok, None, format, &options).expect("6 levels parse");
        assert!(parse_rdf12_with_options(&over, None, format, &options).is_err(), "7 levels rejected ({format:?})");
        let deep = format!("{P}:a :b {}1{} .", "( ".repeat(100_000), " )".repeat(100_000));
        assert!(on_small_stack(move || parse_rdf12(&deep, None, format).is_err()), "{format:?} default limit");
    }
}

const SRL_HEAD: &str = "PREFIX : <http://e/>\n";

// FILTER's own parentheses are a parenthesised expression, so `FILTER` followed
// by `depth` parentheses is exactly `depth` levels deep.
fn srl_expr(depth: usize) -> String {
    format!("{SRL_HEAD}RULE {{ ?x :ok true }} WHERE {{ ?x :p ?v . FILTER{}?v > 1{} }}", "(".repeat(depth), ")".repeat(depth))
}
// One level for FILTER's parentheses plus `depth - 1` prefix operators.
fn srl_unary(depth: usize) -> String {
    format!("{SRL_HEAD}RULE {{ ?x :ok true }} WHERE {{ ?x :p ?v . FILTER({}?v) }}", "!".repeat(depth - 1))
}
fn srl_collection(depth: usize) -> String {
    format!("{SRL_HEAD}RULE {{ ?x :p {}1{} }} WHERE {{ ?x :q ?y }}", "( ".repeat(depth), " )".repeat(depth))
}
fn srl_bnode(depth: usize) -> String {
    format!("{SRL_HEAD}RULE {{ ?x :p {}:z{} }} WHERE {{ ?x :q ?y }}", "[ :p ".repeat(depth), " ]".repeat(depth))
}

const SRL_SHAPES: [Shape; 4] = [
    ("parenthesised expression", srl_expr),
    ("unary operator chain", srl_unary),
    ("collection", srl_collection),
    ("blank node property list", srl_bnode),
];

#[test]
fn srl_nesting_at_the_limit_parses_and_one_level_more_is_an_error() {
    let options = ParserOptions { max_nesting_depth: 8 };
    for (name, build) in SRL_SHAPES {
        parse_sparql_rl_with_options(&build(8), None, None, &options)
            .unwrap_or_else(|e| panic!("SRL {name} nested 8 deep must parse under a limit of 8: {e}"));
        let err = parse_sparql_rl_with_options(&build(9), None, None, &options)
            .expect_err(&format!("SRL {name} nested 9 deep must be rejected under a limit of 8"));
        assert!(err.to_string().contains("nesting"), "SRL {name}: unhelpful error {err}");
    }
}

#[test]
fn srl_default_limit_stops_hostile_nesting_before_the_stack_does() {
    for (name, build) in SRL_SHAPES {
        let source = build(100_000);
        let rejected = on_small_stack(move || {
            parse_sparql_rl_with_options(&source, None, None, &ParserOptions::default()).is_err()
        });
        assert!(rejected, "SRL {name} nested 100,000 deep must be rejected");
    }
}

// --- the command line ---

use std::io::Write;
use std::process::Command;

fn run_cli(args: &[&str], file_contents: &str, extension: &str) -> std::process::Output {
    let mut path = std::env::temp_dir();
    path.push(format!("eyeron-limits-{}-{}.{extension}", std::process::id(), file_contents.len()));
    std::fs::File::create(&path).unwrap().write_all(file_contents.as_bytes()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_eyeron")).args(args).arg(&path).output().expect("run eyeron");
    let _ = std::fs::remove_file(&path);
    output
}

#[test]
fn cli_max_nesting_depth_flag_rejects_documents_over_it() {
    let doc = n3_list(10);
    let default = run_cli(&[], &doc, "n3");
    assert!(default.status.success(), "10 levels is fine by default: {}", String::from_utf8_lossy(&default.stderr));
    let limited = run_cli(&["--max-nesting-depth", "4"], &doc, "n3");
    assert!(!limited.status.success());
    let stderr = String::from_utf8_lossy(&limited.stderr);
    assert!(!stderr.contains("unknown"), "the flag must exist: {stderr}");
    assert!(stderr.contains("deeper than the limit of 4"), "{stderr}");

    // a limit that admits the document changes nothing
    let roomy = run_cli(&["--max-nesting-depth", "10"], &doc, "n3");
    assert!(roomy.status.success(), "{}", String::from_utf8_lossy(&roomy.stderr));
}

#[test]
fn cli_survives_a_hostile_file_instead_of_aborting() {
    let doc = n3_formula(200_000);
    let output = run_cli(&[], &doc, "n3");
    // exit 1 (an error message), not 134 (SIGABRT from a stack overflow)
    assert_eq!(output.status.code(), Some(1), "{}", String::from_utf8_lossy(&output.stderr));
}
