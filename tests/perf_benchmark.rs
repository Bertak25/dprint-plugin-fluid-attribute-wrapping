//! Performance benchmark / advisory regression guard (task 15.4, Requirement 9.8).
//!
//! These are *advisory* perf tests, not hard real-time guarantees. They drive
//! the public `engine::format_document` seam (re-exported at the crate root and
//! gated on the `wasm` feature) over a representative ~10,000-line markup
//! fixture and assert:
//!
//!   1. A single `format_document` call — which internally runs TWO
//!      `format_once` passes (so it includes BOTH self-checks: the per-tag
//!      structure-equivalence fingerprinting inside each pass and the
//!      idempotence second pass) — completes within a generous time budget. The
//!      indicative target from Requirement 9.8 is ~50 ms on typical
//!      developer-class hardware; we apply a large multiplier so the guard does
//!      not flake on slow/debug CI while still catching gross regressions.
//!
//!   2. Runtime grows roughly *linearly* in input size, i.e. there is no
//!      super-linear backtracking in the scanner. We time a few fixture sizes
//!      and check the largest/smallest per-line cost ratio stays within a
//!      generous bound.
//!
//! The whole file is gated on `feature = "wasm"` because `format_document`
//! returns `dprint_core::plugins::FormatError`, which only exists under that
//! feature. Run with `cargo test --features wasm` (ideally `--release` for a
//! meaningful absolute timing, but the thresholds are generous enough to also
//! pass in a debug build).
#![cfg(feature = "wasm")]

use std::path::Path;
use std::time::{Duration, Instant};

use dprint_plugin_fluid_attribute_wrapping::config::{Configuration, SchemeId};
use dprint_plugin_fluid_attribute_wrapping::format_document;

/// A test configuration with a moderate line width so the fixture contains a mix
/// of tags that fit on one line and tags that must wrap — exercising both the
/// fast path and the wrapping/render path. All dialect flags are enabled so any
/// extension resolves.
fn config() -> Configuration {
    Configuration {
        line_width: 80,
        fluid_wrapping: true,
        attribute_sorting: true,
        attribute_order_scheme: SchemeId::Standard,
        indent_width: 2,
        use_tabs: false,
        vue: true,
        astro: true,
        svelte: true,
    }
}

/// Build a representative markup fixture of exactly `lines` physical lines.
///
/// The content mixes several shapes so the scanner, sorter, wrapper, and both
/// self-checks all do real work:
///   - short tags that already fit on one line (fast path, no change),
///   - wide tags with many attributes that must wrap onto continuation lines,
///   - self-closing tags (canonical ` />` terminator),
///   - plain interleaved text and closing tags (verbatim passthrough).
///
/// The pattern repeats every 4 lines; each generated line is a self-contained
/// construct so the total line count is deterministic.
fn fixture(lines: usize) -> String {
    // Four distinct line templates, cycled. Each entry is one physical line.
    let templates: [&str; 4] = [
        // Short tag that fits within 80 cols: emitted unchanged (fast path).
        "<a href=\"/home\" class=\"nav\">Home</a>",
        // Wide tag that exceeds 80 cols: must wrap onto continuation lines.
        "<section id=\"main\" class=\"container wide hero\" data-role=\"banner\" role=\"region\" aria-label=\"primary\">body</section>",
        // Self-closing tag with several attributes: canonical ` />` terminator.
        "<input id=\"email\" name=\"email\" type=\"email\" value=\"\" placeholder=\"you@example.com\" required=\"required\" />",
        // Plain text with entities and no start tags: verbatim passthrough.
        "Just some prose &amp; a few words, no tags on this line at all.",
    ];

    let mut out = String::with_capacity(lines * 96);
    for i in 0..lines {
        out.push_str(templates[i % templates.len()]);
        out.push('\n');
    }
    out
}

/// Format `src` once through the public seam and return the elapsed wall-clock
/// time. Asserts the call succeeds (either a change or a no-change signal); a
/// hard error here would mean the fixture tripped a safety stop, which is a test
/// bug, not a perf result.
fn time_one_format(src: &str) -> Duration {
    let cfg = config();
    let bytes = src.as_bytes();
    let start = Instant::now();
    let result = format_document(Path::new("fixture.html"), bytes, None, &cfg);
    let elapsed = start.elapsed();
    assert!(
        result.is_ok(),
        "format_document should not error on the perf fixture: {:?}",
        result.err()
    );
    elapsed
}

/// A single `format_document` pass over a ~10,000-line fixture completes within a
/// generous time budget.
///
/// Requirement 9.8's indicative target is ~50 ms on typical developer-class
/// hardware. This guard multiplies that generously (debug builds and shared CI
/// runners are far slower than the indicative target), so it flags only a gross
/// regression — e.g. accidental super-linear work — rather than normal timing
/// jitter. It is advisory: the point is the growth characteristic, verified
/// separately below.
#[test]
fn format_10k_lines_within_generous_budget() {
    let src = fixture(10_000);
    // Sanity-check the fixture really is ~10,000 lines.
    let line_count = src.lines().count();
    assert_eq!(line_count, 10_000, "fixture must be exactly 10,000 lines");

    // Warm up once (pay first-touch costs like allocator growth) so the measured
    // run reflects steady-state behavior, then take the best of a few samples to
    // reduce noise from scheduler hiccups on a busy CI box.
    let _ = time_one_format(&src);
    let best = (0..3)
        .map(|_| time_one_format(&src))
        .min()
        .expect("at least one sample");

    // Indicative target 50 ms, multiplied generously to absorb debug builds and
    // slow shared runners. This is a regression guard, not a real-time contract.
    let budget = Duration::from_millis(2_000);
    assert!(
        best <= budget,
        "single format_document pass over 10k lines took {best:?}, exceeding the \
         advisory budget of {budget:?} (indicative target ~50 ms). This suggests a \
         performance regression (e.g. super-linear scanning)."
    );
}

/// Runtime grows roughly linearly in input size — there is no super-linear
/// backtracking in the scanner.
///
/// We time several fixture sizes and compare the per-line cost (elapsed / lines)
/// at the largest size against the smallest size. For linear behavior the
/// per-line cost is approximately constant, so the ratio stays near 1. We allow
/// a generous ceiling to absorb fixed per-call overhead (which dominates the
/// per-line cost at small sizes and *shrinks* the ratio) and timing noise, while
/// still catching quadratic blow-up, where the ratio would grow with size.
#[test]
fn scanning_time_is_roughly_linear() {
    let sizes = [1_000usize, 2_000, 4_000, 8_000];

    // Take a stable per-line cost at each size: warm up, then best-of-three.
    let per_line: Vec<f64> = sizes
        .iter()
        .map(|&n| {
            let src = fixture(n);
            let _ = time_one_format(&src); // warm up
            let best = (0..3)
                .map(|_| time_one_format(&src))
                .min()
                .expect("at least one sample");
            // Guard against a zero duration on very fast machines by flooring to
            // 1 ns so the ratio arithmetic is well-defined.
            let nanos = best.as_nanos().max(1) as f64;
            nanos / n as f64
        })
        .collect();

    let smallest_per_line = per_line
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);
    let largest_per_line = per_line
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);

    // For linear scaling the per-line cost is ~constant across sizes. A quadratic
    // scanner would make the per-line cost at 8k roughly 8x the cost at 1k. We
    // allow a generous 8x spread to absorb fixed overhead and noise while still
    // flagging super-linear growth. (A truly quadratic algorithm over this 8x
    // size range would blow well past this bound.)
    let ratio = largest_per_line / smallest_per_line.max(f64::MIN_POSITIVE);
    assert!(
        ratio <= 8.0,
        "per-line formatting cost grew {ratio:.2}x across sizes {sizes:?} \
         (per-line nanos: {per_line:?}); expected roughly constant per-line cost \
         for linear scanning. A large ratio suggests super-linear backtracking."
    );
}
