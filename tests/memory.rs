//! How much memory one fact costs.
//!
//! Its own test target, so the peak resident size it reads back is this run's
//! and not some other test's (`VmHWM` is per process, and cargo runs the
//! tests in one target concurrently).

use eyeron::{parse_n3, reason_document, ReasonerOptions};

/// Peak resident set size of this process so far, in bytes.
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
fn a_hundred_thousand_facts_do_not_cost_two_kilobytes_each() {
    // Reported as issue #20: 100,000 facts of the simplest possible shape
    // cost hundreds of megabytes, because each one was cloned into the
    // closure, a `seen` set, an `explicit_seen` set and the three fact-index
    // maps, whose keys were further copies of the terms -- six to eight
    // copies of every triple. Measured on that version: 262 MB resident,
    // about 2.6 kB for a 21-byte line of Turtle. With the closure the only
    // owner, and membership and the index buckets holding positions into it,
    // the same run takes 149 MB.
    //
    // The bound is 200 MB: far enough above the current figure to survive an
    // allocator that trims less eagerly, and far enough below the old one to
    // fail if the duplicate storage comes back.
    let Some(before) = peak_resident_bytes() else { return };

    let facts = 100_000;
    let mut source = String::from("@prefix : <http://example.org/> .\n");
    for i in 0..facts {
        source.push_str(&format!(":s{i} :a :o{i} .\n"));
    }

    let doc = parse_n3(&source, None).unwrap();
    let result = reason_document(&doc, &ReasonerOptions::default());
    assert_eq!(result.closure.len(), facts);

    let peak = peak_resident_bytes().unwrap();
    assert!(
        peak < 200 * 1024 * 1024,
        "100,000 facts reached {} MB resident (was {} MB before parsing): \
         every triple is being stored several times over again",
        peak / (1024 * 1024),
        before / (1024 * 1024),
    );
}
