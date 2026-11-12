//! End-to-end integration test driving the public `format_document` seam
//! (task 15.1).
//!
//! This test exercises the plugin the way the dprint host does: it calls the
//! crate-root re-export `format_document` with representative markup and a
//! resolved [`Configuration`], and asserts the three observable outcomes the
//! requirements promise:
//!
//! * a change produces `Ok(Some(candidate))` (Requirements 2.2, 8.1, 8.2),
//! * an already-fixed-point input produces `Ok(None)` (Requirement 2.4),
//! * the transformation is a pure, deterministic function of its inputs
//!   (Requirement 2.3), and
//! * everything that is **not** a start tag — the text between tags and the end
//!   tags themselves — is preserved byte-for-byte (Requirement 3.6).
//!
//! The scenario is "claiming + additive": the plugin claims `.html`/`.jsx`/
//! `.tsx` files and runs additively over whatever markup it is handed, rewriting
//! only start-tag internals and leaving all surrounding content untouched.
//!
//! `format_document` returns `dprint_core::plugins::FormatError`, which only
//! exists under the `wasm` feature, so both the re-export and this test are
//! gated on that feature. Run with `cargo test --features wasm`.
#![cfg(feature = "wasm")]

use std::path::Path;

use dprint_plugin_fluid_attribute_wrapping::config::{Configuration, SchemeId};
use dprint_plugin_fluid_attribute_wrapping::format_document;

/// Build a resolved configuration with a tunable line width. Dialect flags are
/// all enabled so any extension under test resolves, and `indent_width`/
/// `use_tabs` match the global defaults the host would supply.
fn config(line_width: u32, fluid: bool, sorting: bool) -> Configuration {
    Configuration {
        line_width,
        fluid_wrapping: fluid,
        attribute_sorting: sorting,
        attribute_order_scheme: SchemeId::Standard,
        indent_width: 2,
        use_tabs: false,
        vue: true,
        astro: true,
        svelte: true,
    }
}

/// Extract the text content a document carries *outside* of its start tags:
/// everything that is not between a `<` and the matching `>` of an opening tag.
/// For the fixtures used here (no `>` inside attribute values) this is a simple,
/// dependency-free scan that lets the test assert non-tag content survives a
/// rewrite byte-for-byte without depending on how the start tags were wrapped.
///
/// It keeps the bytes of end tags (`</div>`) and the text between tags, and
/// drops only the interior of start tags, so two documents that differ *only*
/// in start-tag whitespace yield the same non-tag content.
fn non_tag_content(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '<' {
            // Is this the start of an *end* tag (`</...>`)? Keep end tags verbatim.
            let is_end_tag = bytes.get(i + 1).copied() == Some(b'/');
            // Find the closing '>'.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'>' {
                j += 1;
            }
            // Include the '>' if present.
            let tag_end = (j + 1).min(bytes.len());
            if is_end_tag {
                out.push_str(&src[i..tag_end]);
            }
            // Start tags contribute nothing to the non-tag content.
            i = tag_end;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// A wide start tag under a narrow line width must wrap, so `format_document`
/// returns `Ok(Some(candidate))`; the candidate differs from the input and the
/// non-tag content (the body text and the end tag) is preserved byte-for-byte
/// (Requirements 2.2, 3.6, 8.1, 8.2).
#[test]
fn changed_input_returns_some_and_preserves_non_tag_content() {
    let src = "<div class=\"container\" id=\"main\" data-role=\"banner\">hello, world</div>";
    let result = format_document(
        Path::new("page.html"),
        src.as_bytes(),
        None,
        &config(24, true, false),
    )
    .expect("format_document should succeed");

    let candidate =
        result.expect("a wide tag under a narrow width must wrap, yielding Ok(Some(..))");
    let out = String::from_utf8(candidate).expect("output is valid UTF-8");

    // It actually changed.
    assert_ne!(out.as_bytes(), src.as_bytes(), "the tag should have wrapped");
    // It wrapped onto continuation lines.
    assert!(out.contains('\n'), "expected continuation lines, got {out:?}");
    // Every attribute value survives verbatim.
    assert!(out.contains("class=\"container\""));
    assert!(out.contains("id=\"main\""));
    assert!(out.contains("data-role=\"banner\""));

    // Non-tag content (body text + end tag) is byte-for-byte identical.
    assert_eq!(
        non_tag_content(&out),
        non_tag_content(src),
        "text between tags and the end tag must be preserved byte-for-byte"
    );
    // Concretely, the body text and end tag are intact.
    assert!(out.contains("hello, world</div>"));
}

/// An already-canonical tag that fits within the line width is a fixed point, so
/// `format_document` reports `Ok(None)` — no change (Requirement 2.4).
#[test]
fn fixed_point_input_returns_none() {
    let src = "<a href=\"x\">click here</a>";
    let result = format_document(
        Path::new("page.html"),
        src.as_bytes(),
        None,
        &config(80, true, false),
    )
    .expect("format_document should succeed");

    assert_eq!(result, None, "a fixed-point input must signal no change");
}

/// The output of one pass is itself a fixed point: feeding the wrapped candidate
/// back through `format_document` yields `Ok(None)` (idempotence as observed
/// through the public seam, Requirements 2.4, 8.1, 8.2).
#[test]
fn wrapped_output_is_a_fixed_point() {
    let src = "<div class=\"container\" id=\"main\" data-role=\"banner\">hello, world</div>";
    let cfg = config(24, true, false);

    let first = format_document(Path::new("page.html"), src.as_bytes(), None, &cfg)
        .expect("first pass should succeed")
        .expect("first pass should change the input");

    let second = format_document(Path::new("page.html"), &first, None, &cfg)
        .expect("second pass should succeed");
    assert_eq!(
        second, None,
        "re-formatting the wrapped output must be a no-op (fixed point)"
    );
}

/// Formatting is a pure, deterministic function of (path, input, range, config):
/// identical inputs produce identical outputs across repeated calls
/// (Requirement 2.3).
#[test]
fn formatting_is_deterministic() {
    let src = "<section id=\"main\" class=\"wrap\" role=\"banner\" data-x=\"1\">body text</section>";
    let cfg = config(20, true, false);

    let a = format_document(Path::new("page.html"), src.as_bytes(), None, &cfg)
        .expect("run A should succeed");
    let b = format_document(Path::new("page.html"), src.as_bytes(), None, &cfg)
        .expect("run B should succeed");
    assert_eq!(a, b, "identical inputs must produce identical outputs");
}

/// A claiming + additive scenario mixing several start tags with body text: the
/// plugin wraps only the start tags that overflow, leaves the ones that fit
/// alone, and preserves every piece of non-tag content byte-for-byte
/// (Requirements 2.2, 3.6).
#[test]
fn claiming_additive_scenario_preserves_all_non_tag_content() {
    let src = concat!(
        "<section id=\"main\" class=\"wrapper\" data-role=\"banner\" aria-label=\"top\">\n",
        "  Intro paragraph with &amp; an entity.\n",
        "  <a href=\"x\">short link</a>\n",
        "  trailing text after the link\n",
        "</section>"
    );
    let result = format_document(
        Path::new("component.jsx"),
        src.as_bytes(),
        None,
        &config(30, true, false),
    )
    .expect("format_document should succeed");

    // The wide <section> start tag overflows width 30, so something changed.
    let candidate = result.expect("the wide section tag should wrap, yielding Ok(Some(..))");
    let out = String::from_utf8(candidate).expect("output is valid UTF-8");

    // Non-tag content — all the body text, the entity, and the end tags — must
    // be preserved byte-for-byte even though a start tag was rewritten.
    assert_eq!(
        non_tag_content(&out),
        non_tag_content(src),
        "text between tags, entities, and end tags must survive verbatim"
    );

    // Spot-check the specific body fragments survive.
    assert!(out.contains("Intro paragraph with &amp; an entity."));
    assert!(out.contains("short link</a>"));
    assert!(out.contains("trailing text after the link"));
    assert!(out.contains("</section>"));
}

// ---------------------------------------------------------------------------
// Multibyte char-boundary regression through the public seam
// (render-multibyte-char-boundary-panic, task 6).
//
// These cases drive the sorting-on / wrapping-off render path end-to-end via
// `format_document`, the path that `leading_breaks_by_attribute` exercises. The
// pre-fix code panicked — "byte index N is not a char boundary" — when a
// multibyte non-whitespace char was the last non-whitespace char immediately
// before an attribute's whitespace run, because `i + 1` landed inside the
// multibyte character. Because the plugin is a sandboxed Wasm formatter, such a
// panic aborts the whole format rather than failing safely, violating main
// plugin Requirement 7. The fix makes the boundary arithmetic char-correct
// (`i + c.len_utf8()`), leaving ASCII output byte-for-byte identical.
//
// The configuration here is `config(small_width, false /*fluid*/, true
// /*sorting*/)`: sorting enabled, fluid wrapping disabled — the only config that
// reaches `leading_breaks_by_attribute`.

/// A small HTML document whose start tag ends an **unquoted** attribute value
/// with a two-byte `¡`, making that `¡` the last non-whitespace char immediately
/// before the next attribute's whitespace run. `data-mark=¡` is a real resolved
/// attribute whose raw slice carries the multibyte char, and the following
/// `class="x"` occurrence resolves past it, so `leading_breaks_by_attribute`
/// computes a boundary one past a multibyte char — exactly the pre-fix panic
/// construction. `format_document` must return a well-formed `Ok(..)` (Some or
/// None) rather than aborting (Requirements bugfix 2.2; main plugin 7), a second
/// pass over the result must be byte-for-byte idempotent (Requirements bugfix
/// 3.3; main plugin 8), and the multibyte char must survive verbatim.
#[test]
fn multibyte_adjacent_attribute_sorting_on_wrapping_off_is_well_formed_and_idempotent() {
    // `¡` (U+00A1, two bytes) is the unquoted value of `data-mark`, so it is the
    // last non-whitespace char directly before the whitespace preceding `class`.
    let src = "<a data-mark=¡ class=\"x\" id=\"y\">body</a>";
    let cfg = config(8, false /* fluid */, true /* sorting */);
    let path = Path::new("page.html");

    // First pass completes as a well-formed result — no panic / no abort.
    let first = format_document(path, src.as_bytes(), None, &cfg)
        .expect("multibyte-adjacent input must format to a well-formed result, not abort");

    // The candidate to feed back in: the changed bytes, or the original when the
    // pass reported no change.
    let candidate: Vec<u8> = match &first {
        Some(bytes) => bytes.clone(),
        None => src.as_bytes().to_vec(),
    };

    // The candidate is valid UTF-8 (the whole pipeline preserves/produces text).
    let candidate_str = String::from_utf8(candidate.clone()).expect("output is valid UTF-8");
    // The multibyte char and both attribute values survive verbatim.
    assert!(
        candidate_str.contains("data-mark=¡"),
        "the ¡-bearing attribute must be preserved: {candidate_str:?}"
    );
    assert!(candidate_str.contains("class=\"x\""));
    assert!(candidate_str.contains("id=\"y\""));
    assert!(candidate_str.contains("body</a>"), "non-tag content must survive");

    // Second pass is a byte-for-byte fixed point.
    let second = format_document(path, &candidate, None, &cfg)
        .expect("second pass must also be well-formed");
    assert_eq!(
        second, None,
        "re-formatting the candidate must be a no-op (fixed point) for multibyte input"
    );
}

/// The ASCII analog under the same sorting-on / wrapping-off config: the `¡` is
/// replaced by an ASCII `x`, keeping the same unquoted-value shape, whitespace
/// run, and attributes. For ASCII, `i + 1 == i + len_utf8`, so the fix changed
/// nothing here — the document must format to a stable fixed point and stay
/// idempotent, which is the runtime-observable stand-in for "unchanged relative
/// to the pre-fix output" (Requirements bugfix 3.1, 3.3; main plugin 8).
#[test]
fn ascii_analog_sorting_on_wrapping_off_is_stable_and_idempotent() {
    // Same shape as the multibyte case, with an ASCII `x` where `¡` stood.
    let src = "<a data-mark=x class=\"x\" id=\"y\">body</a>";
    let cfg = config(8, false /* fluid */, true /* sorting */);
    let path = Path::new("page.html");

    let first = format_document(path, src.as_bytes(), None, &cfg)
        .expect("ASCII analog must format to a well-formed result");

    let candidate: Vec<u8> = match &first {
        Some(bytes) => bytes.clone(),
        None => src.as_bytes().to_vec(),
    };
    let candidate_str = String::from_utf8(candidate.clone()).expect("output is valid UTF-8");
    assert!(candidate_str.contains("data-mark=x"));
    assert!(candidate_str.contains("class=\"x\""));
    assert!(candidate_str.contains("id=\"y\""));
    assert!(candidate_str.contains("body</a>"));

    // Deterministic + idempotent: re-formatting the candidate is a no-op.
    let second = format_document(path, &candidate, None, &cfg)
        .expect("second pass must also be well-formed");
    assert_eq!(
        second, None,
        "the ASCII analog must be a stable fixed point under the same config"
    );
}
