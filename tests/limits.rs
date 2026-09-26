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

// ===========================================================================
// Resource budgets: ReasonerOptions::{max_facts, max_duration, max_term_bytes,
// max_term_depth}. The red versions of these tests use bounded programs that
// finish when unlimited, so they fail safely instead of eating memory.
// ===========================================================================

use eyeron::{reason_document, ReasonerLimit, ReasonerOptions, DEFAULT_MAX_TERM_BYTES, DEFAULT_MAX_TERM_DEPTH};
use std::time::Duration;

/// `:n0 :next :n1 ...` and a transitive `:reach`: about n*n/2 derived facts.
fn reach_chain(n: usize) -> String {
    let mut source = String::from("@prefix : <http://e/> .\n");
    for i in 0..n {
        source.push_str(&format!(":n{i} :next :n{} .\n", i + 1));
    }
    source.push_str("{ ?a :next ?b } => { ?a :reach ?b } .\n");
    source.push_str("{ ?a :reach ?b . ?b :next ?c } => { ?a :reach ?c } .\n");
    source
}

fn run(source: &str, options: &ReasonerOptions) -> eyeron::ReasonerResult {
    let doc = parse_n3(source, None).expect("fixture parses");
    reason_document(&doc, options)
}

#[test]
fn defaults_leave_facts_and_time_unbounded_and_bound_term_size_and_depth() {
    let d = ReasonerOptions::default();
    assert_eq!(d.max_facts, None, "unbounded facts is the historical behaviour");
    assert_eq!(d.max_duration, None);
    assert_eq!(d.max_term_bytes, DEFAULT_MAX_TERM_BYTES);
    assert_eq!(d.max_term_depth, DEFAULT_MAX_TERM_DEPTH);
    assert_eq!(DEFAULT_MAX_TERM_DEPTH, ParserOptions::DEFAULT_MAX_NESTING_DEPTH);
}

#[test]
fn a_reasoning_run_within_its_budgets_is_complete_and_unchanged() {
    let options = ReasonerOptions { max_facts: Some(100_000), max_duration: Some(Duration::from_secs(60)), ..Default::default() };
    let unlimited = run(&reach_chain(30), &ReasonerOptions::default());
    let budgeted = run(&reach_chain(30), &options);
    assert!(budgeted.is_complete(), "{:?}", budgeted.limits_reached);
    assert_eq!(budgeted.closure.len(), unlimited.closure.len());
}

#[test]
fn max_facts_stops_the_closure_and_says_so() {
    let options = ReasonerOptions { max_facts: Some(500), ..Default::default() };
    let result = run(&reach_chain(200), &options);
    assert!(!result.is_complete(), "20,000 derivable facts cannot fit a budget of 500");
    assert!(result.limits_reached.contains(&ReasonerLimit::Facts), "{:?}", result.limits_reached);
    assert!(result.closure.len() <= 500, "closure grew to {}", result.closure.len());
    let summary = result.incomplete_summary().expect("incomplete");
    assert!(summary.contains("fact limit"), "{summary}");
}

#[test]
fn max_duration_stops_a_long_run_and_says_so() {
    let options = ReasonerOptions { max_duration: Some(Duration::from_millis(1)), ..Default::default() };
    let started = std::time::Instant::now();
    let result = run(&reach_chain(400), &options);
    assert!(!result.is_complete(), "a 1 ms budget cannot finish an 80,000-fact closure");
    assert!(result.limits_reached.contains(&ReasonerLimit::Time), "{:?}", result.limits_reached);
    assert!(started.elapsed() < Duration::from_secs(20), "the deadline was not honoured promptly");
}

#[test]
fn max_term_bytes_rejects_an_oversized_builtin_result() {
    // 30 MB of padding requested from string:format; the cap is 1 KB.
    let source = r#"
        @prefix : <http://e/> .
        @prefix string: <http://www.w3.org/2000/10/swap/string#> .
        :a :fmt "%30000000s" .
        { :a :fmt ?f . (?f "x") string:format ?s } => { :a :out ?s } .
    "#;
    let options = ReasonerOptions { max_term_bytes: 1000, ..Default::default() };
    let result = run(source, &options);
    assert!(result.limits_reached.contains(&ReasonerLimit::TermBytes), "{:?}", result.limits_reached);
    assert!(
        result.derived.iter().all(|t| !format!("{t:?}").contains("http://e/out")),
        "the oversized string must not become a fact"
    );
}

#[test]
fn max_term_bytes_also_bounds_concatenation() {
    let long = "a".repeat(600);
    let source = format!(
        r#"@prefix : <http://e/> .
           @prefix string: <http://www.w3.org/2000/10/swap/string#> .
           :a :s "{long}" .
           {{ :a :s ?x . (?x ?x) string:concatenation ?y }} => {{ :a :doubled ?y }} ."#
    );
    let options = ReasonerOptions { max_term_bytes: 1000, ..Default::default() };
    let result = run(&source, &options);
    assert!(result.limits_reached.contains(&ReasonerLimit::TermBytes), "{:?}", result.limits_reached);
    assert!(result.derived.iter().all(|t| !format!("{t:?}").contains("doubled")));
    // a builtin result within the cap is unaffected
    let roomy = run(&source, &ReasonerOptions { max_term_bytes: 2000, ..Default::default() });
    assert!(roomy.is_complete());
    assert!(roomy.derived.iter().any(|t| format!("{t:?}").contains("doubled")));
}

#[test]
fn max_term_depth_rejects_a_derived_fact_nested_too_deeply() {
    let source = format!(
        "@prefix : <http://e/> .\n:a :p {}:z{} .\n{{ :a :p ?x }} => {{ :b :p ?x }} .\n",
        "{ :x :y ".repeat(20),
        " }".repeat(20)
    );
    let strict = run(&source, &ReasonerOptions { max_term_depth: 10, ..Default::default() });
    assert!(strict.limits_reached.contains(&ReasonerLimit::TermDepth), "{:?}", strict.limits_reached);
    assert!(strict.derived.is_empty(), "the 20-deep copy must be rejected");

    let default = run(&source, &ReasonerOptions::default());
    assert!(default.is_complete(), "20 levels is within the default of {DEFAULT_MAX_TERM_DEPTH}");
    assert_eq!(default.derived.len(), 1);
}

// --- the command line ---

#[test]
fn cli_budget_flags_stop_a_run_and_name_the_limit() {
    let chain = reach_chain(200);
    let ok = run_cli(&[], &chain, "n3");
    assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
    for (flag, value, label) in [("--max-facts", "500", "fact limit"), ("--timeout-ms", "1", "time limit")] {
        let source = if flag == "--timeout-ms" { reach_chain(400) } else { chain.clone() };
        let out = run_cli(&[flag, value], &source, "n3");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("unknown option"), "{flag} must exist: {stderr}");
        assert_eq!(out.status.code(), Some(1), "{flag}: {stderr}");
        assert!(stderr.contains(label), "{flag}: {stderr}");
    }
}

#[test]
fn cli_term_flags_reject_oversized_and_too_deep_results() {
    let format = "@prefix : <http://e/> . @prefix string: <http://www.w3.org/2000/10/swap/string#> .\n\
                  :a :fmt \"%30000000s\" .\n{ :a :fmt ?f . (?f \"x\") string:format ?s } => { :a :out ?s } .\n";
    let out = run_cli(&["--max-term-bytes", "1000"], format, "n3");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("unknown option"), "{stderr}");
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("term-size limit"), "{stderr}");

    let deep = format!(
        "@prefix : <http://e/> .\n:a :p {}:z{} .\n{{ :a :p ?x }} => {{ :b :p ?x }} .\n",
        "{ :x :y ".repeat(20),
        " }".repeat(20)
    );
    let out = run_cli(&["--max-term-depth", "10"], &deep, "n3");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("unknown option"), "{stderr}");
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("term-depth limit"), "{stderr}");
}
