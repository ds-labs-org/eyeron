//! How much memory `ReasonerOptions::include_explicit: false` actually
//! saves, on the same 100,000-fact shape `tests/memory.rs` measures.
//!
//! Its own test target, for the same reason `tests/memory.rs` is: `VmHWM`
//! is a per-process high-water mark that never goes down, and cargo can run
//! several test binaries from this crate concurrently, so this number has
//! to come from a process that never also ran the `include_explicit: true`
//! (default) case.

use eyeron::{parse_n3, reason_document, ReasonerOptions};

#[cfg(target_os = "linux")]
fn peak_resident_bytes() -> Option<usize> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kilobytes: usize = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kilobytes * 1024)
}

#[cfg(not(target_os = "linux"))]
fn peak_resident_bytes() -> Option<usize> {
    None
}

#[test]
fn skipping_explicit_saves_a_real_fraction_of_the_hundred_thousand_fact_baseline() {
    // `tests/memory.rs` measures the current (include_explicit: true,
    // the default) shape at under 200 MB, itself already down from an
    // unfixed 262 MB (issue #20's storage-overhead fix, e9e179e). This test
    // is not a race against that one -- they are separate processes -- it
    // just re-asserts the *same* input under the *other* setting, with a
    // bound loose enough to survive an allocator that trims less eagerly
    // but tight enough to fail if the skip stops actually skipping.
    let Some(before) = peak_resident_bytes() else { return };

    let facts = 100_000;
    let mut source = String::from("@prefix : <http://example.org/> .\n");
    for i in 0..facts {
        source.push_str(&format!(":s{i} :a :o{i} .\n"));
    }

    let doc = parse_n3(&source, None).unwrap();
    let options = ReasonerOptions { include_explicit: false, ..ReasonerOptions::default() };
    let result = reason_document(&doc, &options);
    assert_eq!(result.closure.len(), facts);
    assert!(result.explicit.is_empty());
    assert!(result.explicit_sources.is_empty());

    let peak = peak_resident_bytes().unwrap();
    // Measured: 102 MB, down from tests/memory.rs's ~119 MB -- a ~17 MB
    // saving on this shape, matching eyereasoner/eyeron#20's own estimate of
    // saving on this shape. 130 MB leaves
    // headroom for a less eager allocator while still failing if the skip
    // stops actually skipping (back to ~119 MB).
    assert!(
        peak < 130 * 1024 * 1024,
        "100,000 facts with include_explicit: false reached {} MB resident \
         (was {} MB before parsing): skipping .explicit/.explicit_sources \
         should measurably undercut the include_explicit: true baseline \
         (tests/memory.rs's ~119 MB), not just avoid growing past it",
        peak / (1024 * 1024),
        before / (1024 * 1024),
    );
}
