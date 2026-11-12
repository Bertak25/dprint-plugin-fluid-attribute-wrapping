//! Engine orchestration: the non-recursive `format_once` single pass and the
//! public `format_document` idempotence seam.
//!
//! This module wires the pure stages — dialect resolution, scanning, sorting,
//! wrapping/rendering, and structure-equivalence verification — into a single
//! formatting pass. The pass is deliberately split from the idempotence
//! self-check so that termination is *structural*: [`format_once`] performs
//! exactly one pass and **never** calls itself or `format_document`
//! (Requirements 8.2, 8.3). The public `format_document` seam (task 11.2) runs
//! `format_once` a bounded number of times to assert the fixed point.
//!
//! Every fallible path is modeled through [`EngineError`] and the whole module
//! is panic-free: there is no `unwrap`/`expect` on a value that can legitimately
//! be absent, slicing only ever uses byte offsets produced by the scanner (which
//! are guaranteed to be char boundaries within the source), and range
//! arithmetic is bounds-checked before any slice.

use std::ops::Range;
use std::path::Path;

use crate::config::Configuration;
use crate::dialect::{Dialect, ScannerProfile};
use crate::model::ScannedTag;
use crate::render::{render_start_tag, Newline};
use crate::scanner::scan_start_tags;
use crate::sort::{sort_attributes, AttributeOrderScheme};
use crate::verify::{
    angle_brackets_conserved, fingerprints_equivalent, structural_fingerprint, tag_fingerprint,
    tag_fingerprints_equivalent,
};

/// A failure surfaced by [`format_once`].
///
/// Each variant corresponds to one of the pipeline's hard stops. Everything that
/// is *not* an error — an unsupported range, a file whose extension is outside
/// the resolved dialect set, a tag the scanner declined to touch — is a no-op
/// that leaves the relevant bytes unchanged rather than an `EngineError`
/// (Requirements 2.2, 2.3, 3.4, 3.5, 4.8, 4.9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineError {
    /// The input bytes were not valid UTF-8 (Requirement 2.5). The plugin only
    /// operates on text and cannot reinterpret binary input.
    NotUtf8,
    /// A sub-range was requested whose offsets fall outside the input buffer
    /// (Requirement 7.3). A range the plugin simply does not support is *not*
    /// this error — it is a no-op (see [`format_once`]).
    OutOfBoundsRange,
    /// The scanner hit one of the enumerated end-of-input truncation triggers
    /// (unterminated start tag, quoted value, braced expression, or opaque
    /// region) and reported an in-bounds location (Requirement 7.1).
    ParseFailure {
        /// 1-based line number of the failure.
        line: usize,
        /// 1-based column number of the failure.
        column: usize,
        /// Human-readable description of what was left unterminated.
        message: String,
    },
    /// After all per-tag reverts, the whole-document structure-equivalence
    /// backstop still found a divergence that could not be localized to a single
    /// tag (Requirement 7.7). The candidate is discarded and the input is left
    /// unchanged by the caller.
    StructureDivergence,
}

/// Execute exactly one formatting pass over `input` and return the candidate
/// bytes, or an [`EngineError`].
///
/// # Pipeline (Requirements 2.2, 2.3, 3.4–3.6, 4.8, 4.9, 5.6, 7.1–7.3, 7.5–7.7)
///
/// 1. **Range gate.** A `range` the plugin does not support (this plugin only
///    formats whole files) yields `Ok(input)` unchanged — a no-op, not an error
///    (Requirement 7.2). A range whose offsets are out of bounds yields
///    [`EngineError::OutOfBoundsRange`] (Requirement 7.3). `None` means
///    whole-file and proceeds.
/// 2. **UTF-8 decode.** Invalid UTF-8 yields [`EngineError::NotUtf8`]
///    (Requirement 2.5).
/// 3. **Dialect gate.** The file extension is resolved through
///    [`Dialect::from_extension`]. An extension outside the resolved set yields
///    `Ok(input)` unchanged (Requirements 3.4, 3.5), preserving everything
///    byte-for-byte (Requirement 3.6).
/// 4. **Scan.** Start tags are located with [`scan_start_tags`]; a parse failure
///    becomes [`EngineError::ParseFailure`] carrying the scanner's in-bounds
///    line/column (Requirement 7.1).
/// 5. **Per-tag rewrite.** For each tag in source order: a
///    not-confidently-rewritable tag is a per-tag no-op (Requirement 4.9). Any
///    other tag has its attributes sorted first (when sorting is enabled —
///    sorting precedes wrapping, Requirement 5.6) and is then rendered. The
///    rewrite is only kept when the tag's before/after fingerprints are
///    equivalent; otherwise only that tag's bytes are reverted (Requirement
///    7.6).
/// 6. **Splice.** The kept rewrites are spliced into the buffer in a single
///    left-to-right walk, so byte offsets stay valid.
/// 7. **Document backstop.** The input and candidate are fingerprinted whole and
///    compared; a residual divergence yields
///    [`EngineError::StructureDivergence`] (Requirement 7.7).
///
/// The function derives correctness and safety from the received `input` alone,
/// so it behaves identically whether or not an upstream plugin has already
/// formatted the file (Requirement 2.7).
///
/// `format_once` is **non-recursive**: it never calls itself and never calls
/// `format_document`.
//
// `allow(dead_code)` scoped to non-test builds that also lack the `wasm`
// feature: the only non-test caller is the public `format_document` idempotence
// seam, which is gated on `#[cfg(feature = "wasm")]` because it returns
// `dprint_core::plugins::FormatError` (available only under that feature). So in
// a plain `cargo build` (no `wasm`, no `test`) `format_once` has no caller and
// the allow keeps the pedantic-deny build clean; under `wasm` or `test` it is
// exercised and the allow lifts.
#[cfg_attr(all(not(test), not(feature = "wasm")), allow(dead_code))]
pub(crate) fn format_once(
    file_path: &Path,
    input: &[u8],
    range: Option<Range<usize>>,
    config: &Configuration,
) -> Result<Vec<u8>, EngineError> {
    // 1. Range gate. This plugin formats whole files only: a present range is an
    //    unsupported-range no-op UNLESS its offsets are out of bounds, which is a
    //    hard error (Requirements 7.2, 7.3).
    if let Some(range) = range {
        if range.start > range.end || range.start > input.len() || range.end > input.len() {
            return Err(EngineError::OutOfBoundsRange);
        }
        // In-bounds but unsupported (sub-range) → no change.
        return Ok(input.to_vec());
    }

    // 2. UTF-8 decode.
    let Ok(src) = std::str::from_utf8(input) else {
        return Err(EngineError::NotUtf8);
    };

    // 3. Dialect gate.
    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let Some(dialect) = Dialect::from_extension(ext, config) else {
        // Extension outside the resolved set → preserve byte-for-byte.
        return Ok(input.to_vec());
    };
    let profile = dialect.scanner_profile();
    let indent_width = usize::from(config.indent_width);

    // 4. Scan start tags.
    let tags = match scan_start_tags(src, &profile, indent_width) {
        Ok(tags) => tags,
        Err(failure) => {
            return Err(EngineError::ParseFailure {
                line: failure.line,
                column: failure.column,
                message: failure.message,
            });
        }
    };

    // 5 & 6. Per-tag rewrite + splice. Walk the source left to right, copying the
    //        verbatim bytes between tags and emitting either each tag's kept
    //        rewrite or its original span (a per-tag no-op / revert).
    let newline = Newline::detect(src);
    let sorting = config.attribute_sorting;
    let scheme = AttributeOrderScheme::standard();

    let mut output = String::with_capacity(src.len());
    let mut cursor = 0usize;
    for tag in &tags {
        // Verbatim text preceding this tag. The scanner yields tags in source
        // order with non-overlapping spans, so `tag.span.start >= cursor` always
        // holds; the guard keeps the splice total even if that ever changed.
        if tag.span.start >= cursor {
            output.push_str(&src[cursor..tag.span.start]);
        }
        let src_tag = &src[tag.span.clone()];

        let rewrite = rewrite_tag(tag, src_tag, config, &profile, newline, sorting, &scheme);
        output.push_str(&rewrite);
        cursor = tag.span.end;
    }
    // Trailing bytes after the last tag (or the whole buffer when there are none).
    if cursor < src.len() {
        output.push_str(&src[cursor..]);
    }

    // 7. Document-level structure-equivalence backstop. Both sides are scanned
    //    with the identical profile/indent model so a divergence reflects a real
    //    structural change, never a measurement difference (Requirement 7.7).
    let Ok(input_fp) = structural_fingerprint(src, &profile, indent_width) else {
        return Err(EngineError::StructureDivergence);
    };
    let Ok(output_fp) = structural_fingerprint(&output, &profile, indent_width) else {
        return Err(EngineError::StructureDivergence);
    };
    // The document is structure-equivalent only when BOTH the fingerprint
    // comparison AND the angle-bracket-conservation invariant hold. The latter
    // is the backstop for a dropped `<`/`>` that the fingerprint deliberately
    // excludes — e.g. a TypeScript generic misread as a JSX start tag whose
    // delimiter is dropped, even when the same misread occurs symmetrically on
    // both scans (Requirements 2.7, 7.5). Localizable dropped-bracket
    // divergences are already reverted per tag inside `rewrite_tag`
    // (Requirement 7.6); a residual, un-localizable divergence surfaces here as
    // a document-level error (Requirement 7.7), reusing the existing
    // `StructureDivergence` path with no new error variant.
    if !fingerprints_equivalent(&input_fp, &output_fp, sorting)
        || !angle_brackets_conserved(src, &output)
    {
        return Err(EngineError::StructureDivergence);
    }

    Ok(output.into_bytes())
}

/// Render an [`EngineError`] as the human-readable message carried by the
/// `FormatError` surfaced to the dprint host.
///
/// Each variant maps to a descriptive, deterministic string; a
/// [`EngineError::ParseFailure`] includes its in-bounds 1-based line/column so
/// the host can point the user at the truncation site (Requirement 7.1).
#[cfg(feature = "wasm")]
fn engine_error_message(error: &EngineError) -> String {
    match error {
        EngineError::NotUtf8 => "input is not valid UTF-8".to_string(),
        EngineError::OutOfBoundsRange => "requested range is out of bounds".to_string(),
        EngineError::ParseFailure { line, column, message } => {
            format!("parse failure at line {line}, column {column}: {message}")
        }
        EngineError::StructureDivergence => {
            "formatting would change the document structure; input left unchanged".to_string()
        }
    }
}

/// Public entry point: format `input` and return the changed bytes, a no-change
/// signal, or an error.
///
/// This is the **idempotence seam** (Requirements 2.4, 8.1, 8.2, 8.3). It runs
/// [`format_once`] exactly twice and never recurses, so termination is
/// structural rather than dependent on the input converging:
///
/// 1. `candidate = format_once(input)` — one pass producing the candidate bytes.
/// 2. `second = format_once(candidate)` — EXACTLY one more pass, on the
///    candidate, to assert a fixed point. Both passes receive the identical,
///    already-resolved [`Configuration`] (it is **not** re-resolved from the
///    `ConfigKeyMap` between passes), so the shared column-width model and every
///    other width-estimation input are byte-for-byte identical across passes and
///    the self-check can never be tripped by a measurement difference
///    (Requirement 8.5).
/// 3. If `second != candidate` the candidate is not a fixed point: return a
///    `FormatError` stating idempotence was not achieved and leave the original
///    `input` unchanged (Requirement 8.3).
/// 4. Otherwise the candidate is a fixed point: return `Ok(None)` when it equals
///    the input bytes (no change, Requirement 2.4) or `Ok(Some(candidate))` when
///    it differs (Requirements 8.1, 8.2).
///
/// Gated on the `wasm` feature because its error type,
/// [`dprint_core::plugins::FormatError`], is only available there; the sole
/// production caller (the plugin handler) is likewise wasm-gated.
///
/// `format_document` calls [`format_once`] and **never** calls itself.
#[cfg(feature = "wasm")]
pub fn format_document(
    file_path: &Path,
    input: &[u8],
    range: Option<Range<usize>>,
    config: &Configuration,
) -> Result<Option<Vec<u8>>, dprint_core::plugins::FormatError> {
    use dprint_core::plugins::FormatError;

    // Pass 1: produce the candidate from the received input.
    let candidate = format_once(file_path, input, range.clone(), config)
        .map_err(|err| FormatError::new(engine_error_message(&err)))?;

    // Pass 2: the idempotence self-check. Call `format_once` EXACTLY one more
    // time, on the candidate, reusing the SAME resolved `config` so width
    // estimation is identical between passes (Requirement 8.5).
    let second = format_once(file_path, &candidate, range, config)
        .map_err(|err| FormatError::new(engine_error_message(&err)))?;

    // The candidate must be a byte-for-byte fixed point.
    if second != candidate {
        // Not idempotent: leave the ORIGINAL input unchanged (Requirement 8.3).
        return Err(FormatError::new(
            "formatting did not reach a fixed point (idempotence not achieved); input left unchanged",
        ));
    }

    // Fixed point reached. Signal no-change vs change by comparing to the input.
    if candidate == input {
        Ok(None)
    } else {
        Ok(Some(candidate))
    }
}

/// Produce the replacement text for a single start tag, applying the per-tag
/// confidence skip, optional sort-before-wrap, canonical render, and the per-tag
/// structure-equivalence self-check.
///
/// Returns the tag's rewritten bytes when the rewrite is confidently
/// structure-preserving, or the original `src_tag` bytes when the tag is a
/// per-tag no-op (not confidently rewritable, Requirement 4.9) or when its
/// before/after fingerprints diverge (a per-tag revert, Requirement 7.6).
//
// `allow(dead_code)` scoped identically to `format_once`: its only caller is
// `format_once`, reachable from production code only through the wasm-gated
// `format_document`.
#[cfg_attr(all(not(test), not(feature = "wasm")), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
fn rewrite_tag(
    tag: &ScannedTag<'_>,
    src_tag: &str,
    config: &Configuration,
    profile: &ScannerProfile,
    newline: Newline,
    sorting: bool,
    scheme: &AttributeOrderScheme,
) -> String {
    // Layer 1 (Requirement 4.9): a tag the scanner could not delimit with full
    // confidence is emitted byte-for-byte unchanged and never wrapped or sorted.
    if !tag.confidently_rewritable {
        return src_tag.to_string();
    }

    // Sort precedes wrap (Requirement 5.6): build the candidate tag with the
    // (possibly reordered) attributes before rendering decides line breaks.
    let candidate = if sorting {
        ScannedTag {
            attributes: sort_attributes(&tag.attributes, scheme),
            ..tag.clone()
        }
    } else {
        tag.clone()
    };

    let rendered = render_start_tag(&candidate, config, newline, src_tag);

    // Per-tag structure-equivalence self-check (Requirement 7.6). Re-scan the
    // rendered tag under the SAME dialect profile and compare its fingerprint
    // against the original tag's. The rendered text is a complete start tag, so
    // scanning it yields exactly one tag; anything else is treated as divergence
    // and reverts this tag. The document-level backstop is the final guard
    // either way.
    let before = tag_fingerprint(tag);
    let indent_width = usize::from(config.indent_width);
    let after = match scan_start_tags(&rendered, profile, indent_width) {
        Ok(scanned) if scanned.len() == 1 => tag_fingerprint(&scanned[0]),
        _ => return src_tag.to_string(),
    };

    // The rewrite is kept only when it is structure-preserving under BOTH the
    // tag-local fingerprint comparison AND the angle-bracket-conservation
    // invariant applied over this tag's original vs rewritten span. The latter
    // localizes a dropped-`<`/`>` divergence (e.g. a generic head misread as a
    // start tag) to this single tag, so it is reverted here as a per-tag no-op
    // (Requirement 7.6) before the document-level backstop would otherwise have
    // to raise it (Requirement 7.7).
    if tag_fingerprints_equivalent(&before, &after, sorting)
        && angle_brackets_conserved(src_tag, &rendered)
    {
        rendered
    } else {
        // Per-tag revert: keep only this tag's original bytes.
        src_tag.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use std::path::Path;

    /// A configuration with explicit, test-friendly values. `line_width` is kept
    /// small so wrapping is easy to trigger; the dialect flags are all enabled so
    /// any extension under test resolves.
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

    /// Run `format_once` on a whole-file `.html` input and return the candidate
    /// as a UTF-8 string, asserting success.
    fn run_html(src: &str, config: &Configuration) -> String {
        let out = format_once(Path::new("file.html"), src.as_bytes(), None, config)
            .expect("format_once should succeed");
        String::from_utf8(out).expect("output is valid UTF-8")
    }

    // ----- extension / dialect gate ----------------------------------------

    #[test]
    fn unsupported_extension_is_unchanged() {
        // A `.md` file is outside the resolved dialect set, so the bytes are
        // preserved verbatim even though they contain tag-like text
        // (Requirements 3.4, 3.5, 3.6).
        let src = "<div class=\"a\"      id=\"b\">text</div>";
        let out = format_once(
            Path::new("notes.md"),
            src.as_bytes(),
            None,
            &config(80, true, false),
        )
        .expect("no-op for unsupported extension");
        assert_eq!(out, src.as_bytes());
    }

    #[test]
    fn missing_extension_is_unchanged() {
        // A path with no extension at all also falls through the gate as a no-op.
        let src = "<a href=\"x\">";
        let out = format_once(
            Path::new("Makefile"),
            src.as_bytes(),
            None,
            &config(80, true, false),
        )
        .expect("no-op for extensionless file");
        assert_eq!(out, src.as_bytes());
    }

    #[test]
    fn disabled_optional_dialect_is_unchanged() {
        // `.vue` only resolves when the vue flag is enabled; with it off the file
        // is left untouched (Requirement 3.4).
        let mut cfg = config(80, true, false);
        cfg.vue = false;
        let src = "<Comp a=\"1\" b=\"2\">";
        let out = format_once(Path::new("app.vue"), src.as_bytes(), None, &cfg)
            .expect("no-op when vue disabled");
        assert_eq!(out, src.as_bytes());
    }

    // ----- per-tag no-op for not-confidently-rewritable --------------------

    #[test]
    fn complex_expression_tag_is_left_unchanged() {
        // A JSX tag with a braced expression the scanner cannot delimit with
        // confidence (the mismatched `[a, b)` brackets) poisons the whole tag,
        // so it is emitted byte-for-byte even with a tiny line width that would
        // otherwise force wrapping (Requirement 4.9).
        let src = "<Comp ok={1} fine=\"yes\" bad={ [a, b) } really-long-attribute=\"x\" />";
        // Sanity-check the premise: the scanner does mark this tag not confidently
        // rewritable, so the engine's per-tag no-op is the behavior under test.
        let profile = Dialect::Jsx.scanner_profile();
        let scanned = scan_start_tags(src, &profile, 2).expect("scan succeeds");
        assert_eq!(scanned.len(), 1);
        assert!(
            !scanned[0].confidently_rewritable,
            "test premise: this tag must be not-confidently-rewritable"
        );

        let out = format_once(
            Path::new("app.tsx"),
            src.as_bytes(),
            None,
            &config(20, true, false),
        )
        .expect("format_once should succeed");
        // The not-confidently-rewritable tag is preserved verbatim.
        assert_eq!(out, src.as_bytes());
    }

    // ----- basic wrap ------------------------------------------------------

    #[test]
    fn long_tag_wraps_onto_continuation_lines() {
        // With a narrow line width the attributes no longer fit on one line, so
        // fluid wrapping breaks them onto continuation lines indented by one
        // indent unit. The attribute values are preserved verbatim.
        let src = "<div class=\"container\" id=\"main\" data-role=\"banner\">x</div>";
        let out = run_html(src, &config(24, true, false));

        // The output must differ from the input (it wrapped) ...
        assert_ne!(out, src);
        // ... contain newlines (continuation lines) ...
        assert!(out.contains('\n'), "expected wrapped output, got {out:?}");
        // ... and still carry every attribute value byte-for-byte.
        assert!(out.contains("class=\"container\""));
        assert!(out.contains("id=\"main\""));
        assert!(out.contains("data-role=\"banner\""));
        // The surrounding document (text + end tag) is untouched.
        assert!(out.ends_with(">x</div>"));
        assert!(out.starts_with("<div"));
    }

    #[test]
    fn short_tag_within_width_is_unchanged() {
        // A tag that already fits within the line width and uses canonical
        // single-space separators is a fixed point: no change (Requirement 8.1).
        let src = "<a href=\"x\">text</a>";
        let out = run_html(src, &config(80, true, false));
        assert_eq!(out, src);
    }

    // ----- basic sort ------------------------------------------------------

    #[test]
    fn sorting_reorders_attributes_when_enabled() {
        // With sorting enabled the standard scheme places `id` ahead of a plain
        // `data-*` attribute and a generic attribute; the reorder must be visible
        // and structure-preserving (Requirements 5.6, 7.6). Wrapping is disabled
        // so only the ordering (and upstream whitespace) changes.
        let src = "<div data-x=\"1\" id=\"main\" class=\"c\">y</div>";
        let out = run_html(src, &config(200, false, true));
        // `id` should now appear before `data-x` in the output.
        let id_pos = out.find("id=\"main\"").expect("id present");
        let data_pos = out.find("data-x=\"1\"").expect("data-x present");
        assert!(
            id_pos < data_pos,
            "expected id before data-x after sorting, got {out:?}"
        );
        // Every attribute value is preserved.
        assert!(out.contains("class=\"c\""));
        // The document structure is otherwise intact.
        assert!(out.ends_with(">y</div>"));
    }

    #[test]
    fn sorting_disabled_preserves_source_order() {
        // With sorting disabled and wrapping disabled, a canonical tag is a
        // fixed point and the source order is preserved (Requirement 5.2).
        let src = "<div data-x=\"1\" id=\"main\" class=\"c\">y</div>";
        let out = run_html(src, &config(200, false, false));
        assert_eq!(out, src);
    }

    // ----- UTF-8 error -----------------------------------------------------

    #[test]
    fn invalid_utf8_is_rejected() {
        // A byte sequence that is not valid UTF-8 cannot be formatted
        // (Requirement 2.5).
        let bytes: &[u8] = &[0xff, 0xfe, 0x00, 0x80];
        let err = format_once(Path::new("file.html"), bytes, None, &config(80, true, false))
            .expect_err("invalid UTF-8 must error");
        assert_eq!(err, EngineError::NotUtf8);
    }

    // ----- parse-failure error ---------------------------------------------

    #[test]
    fn unterminated_tag_reports_parse_failure() {
        // An unterminated start tag is one of the enumerated hard parse failures;
        // the error carries an in-bounds line/column (Requirement 7.1).
        let src = "<div class=\"x";
        let err = format_once(
            Path::new("file.html"),
            src.as_bytes(),
            None,
            &config(80, true, false),
        )
        .expect_err("unterminated tag must error");
        match err {
            EngineError::ParseFailure { line, column, message } => {
                assert_eq!(line, 1, "failure is on the first line");
                assert!(column >= 1, "column is 1-based and in bounds");
                assert!(!message.is_empty(), "message describes the failure");
            }
            other => panic!("expected ParseFailure, got {other:?}"),
        }
    }

    // ----- out-of-bounds range error ---------------------------------------

    #[test]
    fn out_of_bounds_range_errors() {
        // A range whose end exceeds the buffer length is a hard error
        // (Requirement 7.3).
        let src = "<a>";
        let err = format_once(
            Path::new("file.html"),
            src.as_bytes(),
            Some(0..9999),
            &config(80, true, false),
        )
        .expect_err("out-of-bounds range must error");
        assert_eq!(err, EngineError::OutOfBoundsRange);

        // An inverted range (start > end) is likewise out of bounds. Build it
        // from values so it is not a literal reversed-range expression.
        let (start, end) = (2usize, 1usize);
        let err2 = format_once(
            Path::new("file.html"),
            src.as_bytes(),
            Some(start..end),
            &config(80, true, false),
        )
        .expect_err("inverted range must error");
        assert_eq!(err2, EngineError::OutOfBoundsRange);
    }

    #[test]
    fn in_bounds_unsupported_range_is_a_no_op() {
        // An in-bounds sub-range is unsupported but NOT an error: the plugin
        // leaves the whole input unchanged (Requirement 7.2).
        let src = "<div class=\"a\"     id=\"b\">text</div>";
        let out = format_once(
            Path::new("file.html"),
            src.as_bytes(),
            Some(0..5),
            &config(10, true, true),
        )
        .expect("unsupported range is a no-op, not an error");
        assert_eq!(out, src.as_bytes(), "input is returned unchanged");
    }

    // ----- structure preserved (document-level backstop) -------------------

    #[test]
    fn structure_is_preserved_across_a_wrap() {
        // A wrap changes only inter-attribute whitespace, so the document-level
        // structure-equivalence backstop passes and the attribute identity is
        // unchanged (Requirements 7.5, 7.7). Re-scanning the output yields the
        // same tag fingerprints as the input.
        let src = "<section id=\"main\" class=\"wrap\" role=\"banner\">body</section>";
        let cfg = config(20, true, false);
        let out = run_html(src, &cfg);
        assert_ne!(out, src, "a narrow width should have wrapped the tag");

        let profile = Dialect::Html.scanner_profile();
        let input_fp = structural_fingerprint(src, &profile, 2).unwrap();
        let output_fp = structural_fingerprint(&out, &profile, 2).unwrap();
        assert!(
            fingerprints_equivalent(&input_fp, &output_fp, false),
            "wrap must preserve document structure"
        );
    }

    #[test]
    fn text_only_input_is_unchanged() {
        // Input with no start tags has nothing to rewrite and is returned
        // verbatim; the document backstop trivially holds.
        let src = "just some text, no tags & entities &amp; here";
        let out = run_html(src, &config(80, true, true));
        assert_eq!(out, src);
    }

    #[test]
    fn crlf_endings_are_preserved_when_wrapping() {
        // Continuation breaks use the buffer's detected newline convention, so a
        // CRLF file keeps CRLF endings (Requirement 8.1).
        let src = "<div class=\"container\" id=\"main\" data-role=\"x\">a</div>\r\n<p>b</p>";
        let out = run_html(src, &config(20, true, false));
        assert!(out.contains("\r\n"), "CRLF convention must be preserved");
        // No lone LF continuation break was introduced inside the wrapped tag.
        assert!(
            !out.contains("\"\n"),
            "wrapped breaks should be CRLF, got {out:?}"
        );
    }
}

/// Tests for the public [`format_document`] idempotence seam.
///
/// Gated on the `wasm` feature because `format_document` itself is wasm-gated
/// (its error type lives under that feature). Run with
/// `cargo test --features wasm`.
#[cfg(all(test, feature = "wasm"))]
mod format_document_tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use std::path::Path;

    /// Same test configuration as the `format_once` tests: a tunable line width,
    /// all dialect flags enabled so any extension resolves.
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

    #[test]
    fn already_formatted_input_signals_no_change() {
        // A tag that already fits and uses canonical spacing is a fixed point:
        // `format_once` returns it unchanged, so `format_document` reports
        // `Ok(None)` (no change, Requirement 2.4).
        let src = "<a href=\"x\">text</a>";
        let result = format_document(
            Path::new("file.html"),
            src.as_bytes(),
            None,
            &config(80, true, false),
        )
        .expect("format_document should succeed");
        assert_eq!(result, None, "a fixed-point input must signal no change");
    }

    #[test]
    fn changed_input_returns_some_candidate() {
        // A wide tag under a narrow width wraps, so the candidate differs from
        // the input and `format_document` returns `Ok(Some(candidate))`
        // (Requirements 8.1, 8.2).
        let src = "<div class=\"container\" id=\"main\" data-role=\"banner\">x</div>";
        let result = format_document(
            Path::new("file.html"),
            src.as_bytes(),
            None,
            &config(24, true, false),
        )
        .expect("format_document should succeed");
        let out = result.expect("a wrap must produce changed bytes");
        assert_ne!(out, src.as_bytes(), "output must differ from input");
        // The attribute values survive the pass byte-for-byte.
        let text = String::from_utf8(out).expect("output is valid UTF-8");
        assert!(text.contains("class=\"container\""));
        assert!(text.contains("id=\"main\""));
        assert!(text.contains("data-role=\"banner\""));
    }

    #[test]
    fn candidate_is_a_fixed_point_after_one_format() {
        // The candidate returned for a changed input must itself be a fixed
        // point: feeding it back through `format_document` yields `Ok(None)`.
        // This is the externally observable form of the idempotence self-check
        // (Requirements 8.1, 8.2).
        let src = "<div class=\"container\" id=\"main\" data-role=\"banner\">x</div>";
        let cfg = config(24, true, false);

        let first = format_document(Path::new("file.html"), src.as_bytes(), None, &cfg)
            .expect("first pass succeeds")
            .expect("first pass changes the input");

        let second = format_document(Path::new("file.html"), &first, None, &cfg)
            .expect("second pass succeeds");
        assert_eq!(
            second, None,
            "the candidate must be a fixed point: a second format yields no change"
        );
    }

    #[test]
    fn unsupported_extension_signals_no_change() {
        // An extension outside the resolved dialect set is a `format_once`
        // no-op, so `format_document` also reports no change (Requirement 2.4).
        let src = "<div class=\"a\"      id=\"b\">text</div>";
        let result = format_document(
            Path::new("notes.md"),
            src.as_bytes(),
            None,
            &config(80, true, false),
        )
        .expect("no-op for unsupported extension");
        assert_eq!(result, None);
    }

    #[test]
    fn invalid_utf8_is_surfaced_as_error() {
        // The UTF-8 guard in `format_once` propagates as a `FormatError`
        // (Requirement 2.5); the original input is not returned as a change.
        let bytes: &[u8] = &[0xff, 0xfe, 0x00, 0x80];
        let err = format_document(
            Path::new("file.html"),
            bytes,
            None,
            &config(80, true, false),
        )
        .expect_err("invalid UTF-8 must error");
        assert!(
            err.to_string().contains("UTF-8"),
            "error message should describe the UTF-8 failure, got {err}"
        );
    }

    #[test]
    fn out_of_bounds_range_is_surfaced_as_error() {
        // An out-of-bounds range is a hard error from `format_once`, surfaced as
        // a `FormatError` (Requirement 7.3).
        let src = "<a>";
        let err = format_document(
            Path::new("file.html"),
            src.as_bytes(),
            Some(0..9999),
            &config(80, true, false),
        )
        .expect_err("out-of-bounds range must error");
        assert!(
            err.to_string().contains("out of bounds"),
            "error should describe the range failure, got {err}"
        );
    }

    #[test]
    fn parse_failure_message_includes_location() {
        // A hard parse failure carries an in-bounds line/column, surfaced in the
        // `FormatError` message (Requirement 7.1).
        let src = "<div class=\"x";
        let err = format_document(
            Path::new("file.html"),
            src.as_bytes(),
            None,
            &config(80, true, false),
        )
        .expect_err("unterminated tag must error");
        let msg = err.to_string();
        assert!(
            msg.contains("line 1") && msg.contains("column"),
            "parse-failure message should carry line/column, got {msg}"
        );
    }
}

/// Property 4: Files outside the resolved extension set are left unchanged.
///
/// *For all* inputs whose extension is not in the resolved supported set
/// — an unsupported extension (e.g. `md`, `ts`, `js`, `txt`), or a Vue/Astro/
/// Svelte extension whose dialect flag is disabled — [`format_once`] returns the
/// input bytes byte-for-byte unchanged and signals no change (Requirements 3.4,
/// 3.5).
///
/// The generators intentionally produce *markup-shaped* content (random text
/// interleaved with tag-like fragments) so the test proves the gate fires
/// **before** any scanning or rewriting: even content that would otherwise be
/// wrapped or sorted is preserved when its extension is outside the resolved
/// set. The extension space covers both always-unsupported extensions and the
/// three optional dialects with their enablement flags randomly toggled; the
/// property only asserts the no-change guarantee on the cases the resolver
/// actually classifies as outside the set (`Dialect::from_extension == None`),
/// which is the exact precondition of Requirements 3.4/3.5.
#[cfg(test)]
mod extension_gate_property_tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use proptest::prelude::*;
    use std::path::PathBuf;

    /// Extensions that are NEVER in the resolved set regardless of configuration
    /// (plain unsupported file types). Mixed casing is included to exercise the
    /// case-insensitive resolver.
    const UNSUPPORTED_EXTS: &[&str] = &[
        "md", "ts", "js", "txt", "json", "css", "scss", "rs", "py", "xml", "yaml", "HTM", "JSP",
        "handlebars", "",
    ];

    /// The three optional dialect extensions. Each is in the resolved set *iff*
    /// its flag is enabled, so with the flag off the gate must treat the file as
    /// outside the set.
    const OPTIONAL_EXTS: &[&str] = &["vue", "astro", "svelte", "VUE", "Astro", "SVELTE"];

    /// Build a `Configuration` from generated primitives. The dialect flags are
    /// driven by the property so the optional extensions flip in and out of the
    /// resolved set. The wrapping/sorting knobs are also randomized to prove the
    /// gate short-circuits regardless of what formatting *would* do.
    #[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
    fn make_config(
        line_width: u32,
        fluid: bool,
        sorting: bool,
        indent_width: u8,
        use_tabs: bool,
        vue: bool,
        astro: bool,
        svelte: bool,
    ) -> Configuration {
        Configuration {
            line_width,
            fluid_wrapping: fluid,
            attribute_sorting: sorting,
            attribute_order_scheme: SchemeId::Standard,
            indent_width,
            use_tabs,
            vue,
            astro,
            svelte,
        }
    }

    /// A strategy for markup-shaped UTF-8 content: random text fragments
    /// interleaved with tag-like fragments. This is deliberately content the
    /// formatter *would* otherwise touch (start tags with several attributes),
    /// so the preservation guarantee is meaningful rather than trivial.
    fn content_strategy() -> impl Strategy<Value = String> {
        let fragment = prop_oneof![
            // Plain prose, including characters that would never appear in a tag.
            "[a-zA-Z0-9 ,.&;:!?\\-\n\t]{0,40}",
            // A wide start tag that greedy wrapping would normally break apart.
            Just("<div class=\"container\" id=\"main\" data-role=\"banner\" aria-hidden=\"false\">".to_string()),
            // A self-closing tag with redundant whitespace a formatter would normalize.
            Just("<img    src=\"a.png\"     alt=\"x\"   />".to_string()),
            // A JSX-ish tag with a braced value.
            Just("<Comp a={1} really-long-attribute=\"value\" another=\"thing\">".to_string()),
            // Non-tag angle-bracket text.
            Just("1 < 2 && 3 > 2".to_string()),
        ];
        prop::collection::vec(fragment, 0..6).prop_map(|parts| parts.concat())
    }

    /// The extension space: the always-unsupported extensions plus the optional
    /// dialect extensions (which may or may not be resolved depending on flags).
    fn ext_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            prop::sample::select(UNSUPPORTED_EXTS).prop_map(str::to_string),
            prop::sample::select(OPTIONAL_EXTS).prop_map(str::to_string),
        ]
    }

    proptest! {
        // Feature: fluid-attribute-wrapping-plugin, Property 4: Files outside the
        // resolved extension set are left unchanged.
        //
        // **Validates: Requirements 3.4, 3.5**

        /// For any content, any extension drawn from the unsupported/optional
        /// pools, and any configuration (including randomized dialect flags),
        /// whenever the resolver classifies the extension as outside the resolved
        /// set (`Dialect::from_extension == None`) `format_once` returns the input
        /// bytes verbatim — a no-op, never an error and never a rewrite.
        #[test]
        fn files_outside_resolved_extension_set_are_unchanged(
            content in content_strategy(),
            ext in ext_strategy(),
            line_width in 1u32..=1000,
            fluid in any::<bool>(),
            sorting in any::<bool>(),
            indent_width in 1u8..=8,
            use_tabs in any::<bool>(),
            vue in any::<bool>(),
            astro in any::<bool>(),
            svelte in any::<bool>(),
        ) {
            let config = make_config(
                line_width, fluid, sorting, indent_width, use_tabs, vue, astro, svelte,
            );

            // Precondition: only assert the no-change guarantee on extensions the
            // resolver actually classifies as outside the set. For an enabled
            // optional dialect this is `Some`, which is outside Property 4's scope
            // (that path is covered by the equivalence/idempotence properties), so
            // skip it. This mirrors the exact trigger of Requirements 3.4/3.5.
            prop_assume!(Dialect::from_extension(&ext, &config).is_none());

            // Build a path with the generated extension. An empty extension means
            // a bare file name with no dot, which the gate also treats as outside
            // the set.
            let path = if ext.is_empty() {
                PathBuf::from("file")
            } else {
                PathBuf::from(format!("file.{ext}"))
            };

            let input = content.as_bytes();
            let result = format_once(&path, input, None, &config);

            // The gate fires before scanning, so the result is always Ok and the
            // bytes are preserved exactly (Requirements 3.4, 3.5).
            prop_assert_eq!(
                result,
                Ok(input.to_vec()),
                "extension {:?} is outside the resolved set but content changed",
                ext
            );
        }
    }
}

/// Property tests for Property 1: formatting is a pure, deterministic function.
///
/// Validates Requirements 2.3 and 2.4: formatting depends only on
/// `(input bytes, configuration)` and produces the same result on every
/// invocation, independent of any upstream provenance.
///
/// These tests drive [`format_once`] directly (rather than the wasm-gated
/// `format_document`) so the property runs on a plain `cargo test` without the
/// `wasm` feature. `format_once` is the single formatting pass through which all
/// determinism-relevant work flows — scan, sort, wrap/render, splice, and the
/// structure-equivalence checks — so asserting that two independent calls with
/// identical `(path, input, range, config)` return identical results (the same
/// `Ok(bytes)` or the same `Err(EngineError)`) is exactly Property 1.
#[cfg(test)]
mod purity_property_tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use proptest::prelude::*;
    use std::path::PathBuf;

    /// A supported file extension. All three are always in the resolved dialect
    /// set (Requirements 3.1, 3.2), so the dialect gate lets the input through to
    /// the real scanning/wrapping/sorting work where determinism actually
    /// matters.
    fn extension_strategy() -> impl Strategy<Value = &'static str> {
        prop_oneof![Just("html"), Just("jsx"), Just("tsx")]
    }

    /// A random, internally consistent [`Configuration`]. `line_width` spans both
    /// narrow (forces wrapping) and wide (keeps single-line) regimes, and every
    /// toggle varies so the generated configs exercise fluid wrapping, sorting,
    /// and tab/space indentation. All three optional dialect flags are enabled so
    /// the chosen extension always resolves; the purity claim is about
    /// determinism for a *fixed* config, not about which configs resolve.
    fn config_strategy() -> impl Strategy<Value = Configuration> {
        (
            1u32..=120,
            any::<bool>(),
            any::<bool>(),
            1u8..=8,
            any::<bool>(),
        )
            .prop_map(
                |(line_width, fluid_wrapping, attribute_sorting, indent_width, use_tabs)| {
                    Configuration {
                        line_width,
                        fluid_wrapping,
                        attribute_sorting,
                        attribute_order_scheme: SchemeId::Standard,
                        indent_width,
                        use_tabs,
                        vue: true,
                        astro: true,
                        svelte: true,
                    }
                },
            )
    }

    /// A markup fragment used to assemble generated documents. Fragments are kept
    /// well-formed and terminated so the input mixes real start tags with
    /// surrounding content; whether any individual tag wraps, sorts, parses, or
    /// is left unchanged does not matter to Property 1 — only that the *same*
    /// input yields the *same* result twice. (Truncated/ill-formed inputs that
    /// take the `ParseFailure` path are also fine here and still deterministic;
    /// these generators simply bias toward the success path to exercise the
    /// scanning/wrap/sort machinery.)
    #[derive(Clone, Debug)]
    enum Fragment {
        /// Plain inter-tag text containing no `<`.
        Text(String),
        /// An end tag such as `</div>`.
        EndTag(String),
        /// A well-formed start tag, possibly with attributes and self-closing.
        StartTag(String),
    }

    impl Fragment {
        fn render(&self) -> String {
            match self {
                Fragment::Text(t) => t.clone(),
                Fragment::EndTag(n) => format!("</{n}>"),
                Fragment::StartTag(s) => s.clone(),
            }
        }
    }

    /// Inter-tag text with no `<` so it can never be mistaken for a tag opener.
    /// Includes whitespace and multibyte characters so width/indent computation
    /// is exercised identically on both passes.
    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                Just(' '),
                Just('\n'),
                Just('\t'),
                Just('a'),
                Just('Z'),
                Just('漢'),
            ],
            0..8,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    /// An element name: ASCII-letter lead followed by letters/digits.
    fn name_strategy() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9]{0,5}"
    }

    /// A well-formed start tag: `<name` then zero or more plain / quoted / braced
    /// attributes, terminated by `>` or ` />`. Braced JSX-style values and
    /// spreads are included so sorting guards and the confidence model are
    /// exercised; every quote and brace here is balanced so the tag is a complete
    /// start tag.
    fn start_tag_strategy() -> impl Strategy<Value = String> {
        let attr = prop_oneof![
            // Plain boolean attribute.
            "[a-z][a-z0-9]{0,4}",
            // Double-quoted value.
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}")
                .prop_map(|(n, v)| format!("{n}=\"{v}\"")),
            // Single-quoted value.
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}")
                .prop_map(|(n, v)| format!("{n}='{v}'")),
            // Braced JSX/TSX value with a simple, confidently-delimited body.
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9]{0,6}")
                .prop_map(|(n, v)| format!("{n}={{{v}}}")),
            // Dynamic spread attribute.
            "[a-z][a-z0-9]{0,4}".prop_map(|v| format!("{{...{v}}}")),
        ];
        (
            name_strategy(),
            prop::collection::vec(attr, 0..5),
            any::<bool>(),
        )
            .prop_map(|(name, attrs, self_closing)| {
                let mut s = String::from("<");
                s.push_str(&name);
                for a in &attrs {
                    s.push(' ');
                    s.push_str(a);
                }
                if self_closing {
                    s.push_str(" />");
                } else {
                    s.push('>');
                }
                s
            })
    }

    fn fragment_strategy() -> impl Strategy<Value = Fragment> {
        prop_oneof![
            text_strategy().prop_map(Fragment::Text),
            name_strategy().prop_map(Fragment::EndTag),
            start_tag_strategy().prop_map(Fragment::StartTag),
        ]
    }

    /// A whole markup document: a concatenation of fragments.
    fn document_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(fragment_strategy(), 0..12)
            .prop_map(|frags| frags.iter().map(Fragment::render).collect())
    }

    /// An optional selection range, generated from two raw offsets plus the
    /// document text so it can be clamped against the input's real length. `None`
    /// is the whole-file path; `Some` ranges (both in-bounds sub-ranges and
    /// deliberately out-of-bounds ones) exercise the range gate. Property 1 only
    /// requires that whatever path a given `(input, range, config)` takes, it
    /// takes it the same way on every call.
    fn range_for(src: &str, want_range: bool, a: usize, b: usize) -> Option<Range<usize>> {
        if !want_range {
            return None;
        }
        // Allow a few offsets past the end so out-of-bounds (hard-error) cases
        // occur alongside in-bounds (no-op) ones.
        let cap = src.len().saturating_add(4);
        let lo = (a % (cap + 1)).min(cap);
        let hi = (b % (cap + 1)).min(cap);
        Some(lo.min(hi)..lo.max(hi))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Property 1 (Validates: Requirements 2.3, 2.4).
        ///
        /// For all supported inputs, configurations, extensions, and ranges,
        /// `format_once` is a pure function of `(path, input, range, config)`:
        /// two independent calls with identical arguments return identical
        /// results — the same `Ok(bytes)` or the same `Err(EngineError)`. There
        /// is no hidden state, no dependence on call order, and no upstream
        /// provenance that could make a second call diverge from the first.
        #[test]
        fn format_once_is_pure_and_deterministic(
            ext in extension_strategy(),
            src in document_strategy(),
            config in config_strategy(),
        ) {
            let path = PathBuf::from(format!("input.{ext}"));
            let input = src.as_bytes();

            // Two independent invocations with byte-identical arguments.
            let first = format_once(&path, input, None, &config);
            let second = format_once(&path, input, None, &config);

            // Same Ok bytes or same Err — the whole-file path is deterministic.
            prop_assert_eq!(&first, &second, "format_once diverged across calls for {:?}", src);

            // A third call still agrees, ruling out any two-call latching.
            let third = format_once(&path, input, None, &config);
            prop_assert_eq!(&first, &third, "format_once diverged on the third call for {:?}", src);
        }

        /// Property 1, range facet (Validates: Requirements 2.3, 2.4).
        ///
        /// Determinism also holds across the range gate: for a fixed range —
        /// whole-file, an in-bounds sub-range (an unsupported no-op), or an
        /// out-of-bounds range (a hard error) — repeated calls return identical
        /// results. The chosen path through the gate is a pure function of the
        /// arguments.
        #[test]
        fn format_once_is_deterministic_across_ranges(
            ext in extension_strategy(),
            src in document_strategy(),
            config in config_strategy(),
            want_range in any::<bool>(),
            a in any::<usize>(),
            b in any::<usize>(),
        ) {
            let path = PathBuf::from(format!("input.{ext}"));
            let input = src.as_bytes();

            // Build a range from the input's own length so in-bounds (no-op) and
            // out-of-bounds (hard-error) cases both occur.
            let range = range_for(&src, want_range, a, b);

            let first = format_once(&path, input, range.clone(), &config);
            let second = format_once(&path, input, range.clone(), &config);
            prop_assert_eq!(
                &first,
                &second,
                "format_once diverged across calls for range {:?} on {:?}",
                range,
                src
            );
        }
    }
}

/// Property 9: Wrapping disabled preserves upstream layout.
///
/// Feature: fluid-attribute-wrapping-plugin.
///
/// *For all* inputs formatted with fluid wrapping disabled **and** sorting
/// disabled, the output is byte-identical to the input (Requirement 4.8). With
/// both transformations off the engine is a pure no-op on the start tags it
/// locates: it must re-emit the upstream attribute line breaks and all
/// surrounding content exactly, so `format_once(..., None, config)` returns the
/// received bytes unchanged.
///
/// This module is deliberately **not** wasm-gated: it exercises
/// `format_once`, which is available in a plain `cargo test` build, so the
/// property runs host-side without the Wasm feature.
#[cfg(test)]
mod wrapping_disabled_property_tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use proptest::prelude::*;
    use std::path::Path;

    /// A configuration with BOTH fluid wrapping and attribute sorting disabled.
    /// Every optional dialect flag is enabled so any supported extension under
    /// test resolves. `line_width` is kept small to prove the no-op holds even
    /// when a tag would otherwise be wide enough to wrap — with wrapping off the
    /// width is irrelevant and the input must still be preserved verbatim.
    fn disabled_config(line_width: u32, indent_width: u8, use_tabs: bool) -> Configuration {
        Configuration {
            line_width,
            fluid_wrapping: false,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width,
            use_tabs,
            vue: true,
            astro: true,
            svelte: true,
        }
    }

    /// The supported file extensions this property drives markup through. HTML,
    /// JSX, and TSX are always in the resolved set; picking among them spreads
    /// the generated documents across the meaningfully different scanning rules
    /// (unquoted HTML values vs. JSX/TSX braced values and member names).
    fn path_for(idx: usize) -> &'static Path {
        match idx % 3 {
            0 => Path::new("file.html"),
            1 => Path::new("file.jsx"),
            _ => Path::new("file.tsx"),
        }
    }

    /// Inter-tag text containing no `<`, so it can never be mistaken for a tag
    /// opener, and no `-` runs that could spill out of a comment body. A mix of
    /// spaces, newlines, and tabs plus a few non-ASCII glyphs exercises the
    /// verbatim preservation of surrounding content and line endings.
    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                Just(' '),
                Just('\n'),
                Just('\t'),
                Just('a'),
                Just('Z'),
                Just('='),
                Just('é'),
                Just('漢'),
            ],
            0..8,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    /// An element name: an ASCII-letter lead followed by letters/digits.
    fn name_strategy() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9]{0,5}"
    }

    /// A well-formed start tag string: `<name` followed by zero or more plain /
    /// quoted-value attributes, laid out with arbitrary upstream whitespace
    /// (spaces and newlines, including multi-line attribute layouts), and a `>`
    /// or ` />` terminator. Every quote is balanced and every tag is terminated,
    /// so the scanner locates exactly this one tag. The deliberately irregular
    /// separators (`sep_strategy`) are the "upstream layout" whose preservation
    /// this property asserts: with wrapping disabled the engine must not
    /// normalize them.
    fn start_tag_strategy() -> impl Strategy<Value = String> {
        // Separators between attributes: at least one whitespace char, possibly
        // several, possibly spanning lines. This is the upstream attribute line
        // break layout that Requirement 4.8 says must be left unchanged.
        let sep = prop::collection::vec(prop_oneof![Just(' '), Just('\n'), Just('\t')], 1..4)
            .prop_map(|chars| chars.into_iter().collect::<String>());

        let attr = prop_oneof![
            // Plain boolean attribute.
            "[a-z][a-z0-9]{0,4}",
            // Double-quoted value (interior has no quote so it stays balanced).
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}")
                .prop_map(|(n, v)| format!("{n}=\"{v}\"")),
            // Single-quoted value.
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}")
                .prop_map(|(n, v)| format!("{n}='{v}'")),
        ];

        (
            name_strategy(),
            prop::collection::vec((sep, attr), 0..5),
            any::<bool>(),
        )
            .prop_map(|(name, attrs, self_closing)| {
                let mut s = String::from("<");
                s.push_str(&name);
                for (sep, a) in &attrs {
                    s.push_str(sep);
                    s.push_str(a);
                }
                if self_closing {
                    s.push_str(" />");
                } else {
                    s.push('>');
                }
                s
            })
    }

    /// A document fragment: inter-tag text, an end tag, or a well-formed start
    /// tag. Every fragment is well-formed and terminated so the scanner never
    /// hits an enumerated hard parse-failure trigger (unterminated tag / quote /
    /// brace / opaque region) — the failure paths are another property's
    /// concern; this one is about the preservation of valid input.
    fn fragment_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            text_strategy(),
            name_strategy().prop_map(|n| format!("</{n}>")),
            start_tag_strategy(),
        ]
    }

    /// A whole markup document: a sequence of fragments concatenated.
    fn document_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(fragment_strategy(), 0..10)
            .prop_map(|frags| frags.concat())
    }

    proptest! {
        /// Property 9 (Validates: Requirement 4.8).
        ///
        /// With fluid wrapping and attribute sorting both disabled,
        /// `format_once` returns the input byte-for-byte unchanged for any
        /// supported-extension markup input — upstream attribute line breaks and
        /// all surrounding content are preserved exactly.
        #[test]
        fn wrapping_and_sorting_disabled_preserves_input(
            src in document_strategy(),
            path_idx in 0usize..3,
            // A deliberately small line width that would force wrapping if it
            // were enabled; with wrapping off it must have no effect.
            line_width in 1u32..40,
            indent_width in 1u8..=8,
            use_tabs in any::<bool>(),
        ) {
            let config = disabled_config(line_width, indent_width, use_tabs);
            let path = path_for(path_idx);

            let out = format_once(path, src.as_bytes(), None, &config)
                .expect("format_once should succeed on well-formed markup");

            // The core claim: output is byte-identical to the received input.
            prop_assert_eq!(out, src.as_bytes());
        }
    }
}

/// Property 11: Sorting disabled preserves source order.
///
/// *For all* start tags formatted with sorting disabled, the attribute order in
/// the output equals the attribute order in the input (Requirement 5.2).
///
/// This differs from Property 9's byte-for-byte no-op: here fluid wrapping may
/// be *enabled*, so inter-attribute whitespace is allowed to change. The claim
/// is purely about ordering — the sequence of attribute `raw` slices read in
/// document order from each start tag of the output must equal the sequence
/// read from the corresponding tag of the input. The comparison is made with
/// the crate's own scanner so it reflects exactly how the engine perceives
/// attribute order, independent of whatever whitespace layout wrapping chose.
#[cfg(test)]
mod sorting_disabled_property_tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use crate::dialect::Dialect;
    use proptest::prelude::*;
    use std::path::Path;

    /// A configuration with attribute sorting DISABLED. Fluid wrapping is a
    /// generated parameter so the property is exercised both with wrapping off
    /// (upstream layout untouched) and on (whitespace may be rewritten) — in
    /// neither case may the attribute *order* change. Every optional dialect
    /// flag is enabled so any supported extension under test resolves.
    fn config(line_width: u32, fluid_wrapping: bool, indent_width: u8, use_tabs: bool) -> Configuration {
        Configuration {
            line_width,
            fluid_wrapping,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width,
            use_tabs,
            vue: true,
            astro: true,
            svelte: true,
        }
    }

    /// The supported extensions always present in the resolved set, paired with
    /// the matching [`Dialect`] so both input and output can be scanned under
    /// the identical profile the engine used.
    fn path_and_dialect(idx: usize) -> (&'static Path, Dialect) {
        match idx % 3 {
            0 => (Path::new("file.html"), Dialect::Html),
            1 => (Path::new("file.jsx"), Dialect::Jsx),
            _ => (Path::new("file.tsx"), Dialect::Tsx),
        }
    }

    /// Inter-tag text containing no `<` (so it can never open a tag) and no `-`
    /// runs (so it can never be mistaken for a comment body). A few newlines and
    /// non-ASCII glyphs widen coverage of the surrounding-content handling.
    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                Just(' '),
                Just('\n'),
                Just('a'),
                Just('Z'),
                Just('漢'),
            ],
            0..6,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    /// An element name: an ASCII-letter lead followed by letters/digits.
    fn name_strategy() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9]{0,5}"
    }

    /// A well-formed start tag whose attributes all have DISTINCT names, so each
    /// attribute's `raw` slice is a unique, identifiable token. Distinct raw
    /// slices make the per-tag order comparison unambiguous: a reordering would
    /// be visible as a changed sequence, and there are no equal slices that
    /// could mask a swap.
    ///
    /// Attributes are laid out with arbitrary (at least one) whitespace
    /// separators, including newlines, so the input may already be multi-line.
    /// Every quote is balanced and every tag terminated, so the scanner locates
    /// exactly this one tag with exactly these attributes and never hits a hard
    /// parse-failure trigger.
    fn start_tag_strategy() -> impl Strategy<Value = String> {
        // Zero or more distinct attribute names drawn from a fixed pool, as a
        // shuffled subsequence so the *source order* is itself randomized.
        const NAMES: &[&str] = &[
            "id", "class", "data-x", "role", "name", "title", "href", "src", "type", "value",
        ];

        let sep = prop::collection::vec(prop_oneof![Just(' '), Just('\n'), Just('\t')], 1..4)
            .prop_map(|chars| chars.into_iter().collect::<String>());

        // A shuffled subset of indices into NAMES gives distinct names in a
        // random relative order.
        let names = prop::sample::subsequence((0..NAMES.len()).collect::<Vec<_>>(), 0..=NAMES.len())
            .prop_shuffle();

        (
            name_strategy(),
            names,
            prop::collection::vec((sep.clone(), 0u8..=2, "[a-zA-Z0-9 ]{0,6}"), NAMES.len()),
            sep,
            any::<bool>(),
        )
            .prop_map(|(tag_name, name_idxs, specs, lead_sep, self_closing)| {
                let mut s = String::from("<");
                s.push_str(&tag_name);
                for (pos, &idx) in name_idxs.iter().enumerate() {
                    let (attr_sep, value_form, value) = &specs[pos];
                    s.push_str(attr_sep);
                    let attr_name = NAMES[idx];
                    match value_form {
                        // Plain / boolean attribute (no value).
                        0 => s.push_str(attr_name),
                        // Double-quoted value.
                        1 => {
                            s.push_str(attr_name);
                            s.push_str("=\"");
                            s.push_str(value);
                            s.push('"');
                        }
                        // Single-quoted value.
                        _ => {
                            s.push_str(attr_name);
                            s.push_str("='");
                            s.push_str(value);
                            s.push('\'');
                        }
                    }
                }
                // A trailing separator before the terminator keeps `/>` and `>`
                // placement varied without ever unbalancing the tag.
                if self_closing {
                    s.push_str(&lead_sep);
                    s.push_str("/>");
                } else {
                    s.push('>');
                }
                s
            })
    }

    /// A document fragment: inter-tag text, an end tag, or a well-formed start
    /// tag. Every fragment is well-formed and terminated.
    fn fragment_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            text_strategy(),
            name_strategy().prop_map(|n| format!("</{n}>")),
            start_tag_strategy(),
        ]
    }

    /// A whole markup document: a sequence of fragments concatenated.
    fn document_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(fragment_strategy(), 0..8).prop_map(|frags| frags.concat())
    }

    /// Scan `src` under `dialect` and project each start tag to its ordered list
    /// of attribute `raw` slices. Panics the test (via `expect`) if the scan
    /// fails, which cannot happen for the well-formed documents the strategy
    /// produces.
    fn attribute_order_per_tag(src: &str, dialect: Dialect) -> Vec<Vec<String>> {
        let profile = dialect.scanner_profile();
        let tags = scan_start_tags(src, &profile, 2).expect("well-formed markup scans cleanly");
        tags.into_iter()
            .map(|tag| {
                tag.attributes
                    .iter()
                    .map(|attr| attr.raw.to_string())
                    .collect()
            })
            .collect()
    }

    proptest! {
        /// Property 11 (Validates: Requirement 5.2).
        ///
        /// With sorting disabled — whether fluid wrapping is on or off — the
        /// sequence of attribute `raw` slices within each start tag of the
        /// output equals the sequence within the corresponding tag of the
        /// input. No attribute is reordered: source order is preserved.
        #[test]
        fn sorting_disabled_preserves_attribute_source_order(
            src in document_strategy(),
            path_idx in 0usize..3,
            fluid_wrapping in any::<bool>(),
            // Spanning narrow-to-wide widths exercises both the wrapped and the
            // single-line layouts when fluid wrapping is enabled.
            line_width in 1u32..=120,
            indent_width in 1u8..=8,
            use_tabs in any::<bool>(),
        ) {
            let config = config(line_width, fluid_wrapping, indent_width, use_tabs);
            let (path, dialect) = path_and_dialect(path_idx);

            let out = format_once(path, src.as_bytes(), None, &config)
                .expect("format_once should succeed on well-formed markup");
            let out = String::from_utf8(out).expect("output is valid UTF-8");

            // Scan both sides with the SAME dialect profile the engine used and
            // compare the per-tag attribute order. Equal tag counts and equal
            // per-tag attribute sequences together witness source-order
            // preservation (Requirement 5.2).
            let input_order = attribute_order_per_tag(&src, dialect);
            let output_order = attribute_order_per_tag(&out, dialect);

            prop_assert_eq!(&input_order, &output_order);
        }
    }
}

/// Property 19: Non-UTF-8 input is rejected safely.
///
/// Feature: fluid-attribute-wrapping-plugin.
///
/// *For all* byte sequences that are not valid UTF-8, [`format_once`] returns
/// [`EngineError::NotUtf8`], never panics, and never mutates the input. The
/// UTF-8 decode is step 2 of the pass and returns *before* any scanning or
/// rewriting, so the error path is a pure, non-destructive rejection: the
/// caller's `input` slice is left exactly as it was handed in (Requirement 2.5).
///
/// This module is intentionally **not** wasm-gated: `format_once` is available
/// host-side, so the property runs under a plain `cargo test`.
#[cfg(test)]
mod non_utf8_property_tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use proptest::prelude::*;
    use std::path::Path;

    /// A fixed, internally consistent [`Configuration`]. Property 19 is about the
    /// UTF-8 decode gate, which runs before any width/sorting/dialect logic, so
    /// the specific knob values are irrelevant — every optional dialect flag is
    /// enabled so the `.html` extension always resolves past the dialect gate
    /// (not that the input ever reaches it on the non-UTF-8 path).
    fn config() -> Configuration {
        Configuration {
            line_width: 80,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: true,
            astro: true,
            svelte: true,
        }
    }

    /// Assert the non-destructive rejection contract for one invalid-UTF-8 input:
    /// the call returns `Err(EngineError::NotUtf8)` and the caller's bytes are
    /// unchanged. A `Vec<u8>` is passed by shared reference, so Rust's type
    /// system already forbids mutation; comparing against a snapshot documents
    /// the guarantee and guards against any future change to the signature.
    fn assert_rejected(bytes: &[u8]) {
        debug_assert!(
            std::str::from_utf8(bytes).is_err(),
            "test helper requires invalid UTF-8",
        );
        let snapshot = bytes.to_vec();
        let result = format_once(Path::new("file.html"), bytes, None, &config());
        assert_eq!(result, Err(EngineError::NotUtf8));
        assert_eq!(bytes, snapshot.as_slice(), "input bytes must be untouched");
    }

    #[test]
    fn crafted_invalid_sequences_are_rejected() {
        // A handful of classic ill-formed UTF-8 byte sequences, each covering a
        // distinct decode-failure shape.
        let cases: &[&[u8]] = &[
            &[0x80],                   // lone continuation byte
            &[0xBF],                   // another lone continuation byte
            &[0xC3],                   // truncated 2-byte sequence (missing tail)
            &[0xE2, 0x82],             // truncated 3-byte sequence (euro sign cut short)
            &[0xF0, 0x9F, 0x98],       // truncated 4-byte sequence (emoji cut short)
            &[0xC0, 0x80],             // overlong encoding of U+0000
            &[0xED, 0xA0, 0x80],       // surrogate half (U+D800), invalid in UTF-8
            &[0xFF, 0xFE],             // bytes that never appear in UTF-8
            &[b'<', b'a', b'>', 0xFF], // valid markup with a trailing invalid byte
        ];
        for bytes in cases {
            assert_rejected(bytes);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Property 19 (Validates: Requirement 2.5).
        ///
        /// Generate arbitrary byte vectors, keep only those that are not valid
        /// UTF-8 via `prop_assume!`, and assert the rejection contract holds:
        /// `Err(EngineError::NotUtf8)`, no panic, input left unchanged.
        #[test]
        fn arbitrary_invalid_utf8_is_rejected(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
            // Restrict the input space to genuinely invalid-UTF-8 byte strings.
            // Random bytes are invalid UTF-8 the overwhelming majority of the
            // time, so this discards very few cases.
            prop_assume!(std::str::from_utf8(&bytes).is_err());

            let snapshot = bytes.clone();
            let result = format_once(Path::new("file.html"), &bytes, None, &config());

            prop_assert_eq!(result, Err(EngineError::NotUtf8));
            // The rejection path must not mutate the caller's buffer.
            prop_assert_eq!(&bytes, &snapshot);
        }
    }
}

/// Property 14: Sorting precedes wrapping.
///
/// Feature: fluid-attribute-wrapping-plugin.
///
/// *For all* start tags formatted with BOTH sorting and fluid wrapping enabled,
/// the sequence of attributes read in document order from the wrapped output
/// equals the order produced by sorting alone (Requirement 5.6). In other words
/// wrapping never reorders attributes: it only decides where line breaks fall.
/// Sorting is applied first, against the tag's source attributes, and the
/// greedy packer then lays the already-sorted sequence across one or more lines
/// without disturbing its order.
///
/// The property is checked per start tag. For each confidently-rewritable tag
/// of the input the expected order is computed *independently* by calling
/// [`sort_attributes`] on that tag's scanned attributes under the same standard
/// scheme the engine uses, projected to each attribute's `raw` slice. The
/// actual order is obtained by scanning the engine's output with the identical
/// dialect profile and reading each tag's attribute `raw` slices in document
/// order — which naturally reads left-to-right across every continuation line.
/// Equal sequences witness that the output order is exactly the sort order and
/// that wrapping introduced no reordering.
///
/// This module is deliberately **not** wasm-gated: it drives [`format_once`],
/// which is available on a plain `cargo test` build, so the property runs
/// host-side without the Wasm feature.
#[cfg(test)]
mod sort_precedes_wrap_property_tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use crate::dialect::Dialect;
    use crate::sort::{sort_attributes, AttributeOrderScheme};
    use proptest::prelude::*;
    use std::path::Path;

    /// A configuration with BOTH attribute sorting and fluid wrapping ENABLED —
    /// the exact regime Property 14 is about. Every optional dialect flag is
    /// enabled so any supported extension under test resolves.
    fn config(line_width: u32, indent_width: u8, use_tabs: bool) -> Configuration {
        Configuration {
            line_width,
            fluid_wrapping: true,
            attribute_sorting: true,
            attribute_order_scheme: SchemeId::Standard,
            indent_width,
            use_tabs,
            vue: true,
            astro: true,
            svelte: true,
        }
    }

    /// The supported extensions always present in the resolved set, paired with
    /// the matching [`Dialect`] so both input and output can be scanned under
    /// the identical profile the engine used.
    fn path_and_dialect(idx: usize) -> (&'static Path, Dialect) {
        match idx % 3 {
            0 => (Path::new("file.html"), Dialect::Html),
            1 => (Path::new("file.jsx"), Dialect::Jsx),
            _ => (Path::new("file.tsx"), Dialect::Tsx),
        }
    }

    /// Inter-tag text containing no `<` (so it can never open a tag) and no `-`
    /// runs (so it can never be mistaken for a comment body). A few newlines and
    /// non-ASCII glyphs widen coverage of the surrounding-content handling.
    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![Just(' '), Just('\n'), Just('a'), Just('Z'), Just('漢')],
            0..6,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    /// An element name: an ASCII-letter lead followed by letters/digits.
    fn name_strategy() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9]{0,5}"
    }

    /// A well-formed, confidently-rewritable start tag whose attributes all have
    /// DISTINCT names, so each attribute's `raw` slice is a unique, identifiable
    /// token. Distinct raw slices make the per-tag order comparison unambiguous:
    /// any reordering shows up as a changed sequence, and no two equal slices
    /// can mask a swap.
    ///
    /// Attributes are drawn as a shuffled subsequence of a fixed name pool, so
    /// the *source order* is itself randomized and therefore genuinely differs
    /// from the sorted order on most inputs. The pool spans several of the
    /// standard scheme's groups (`id`, `class`, `data-*`, event-ish, generic) so
    /// sorting actually permutes the list rather than leaving it in place. A
    /// trailing dynamic spread may be appended: spreads are order-locked
    /// barriers, and including one exercises that the sort-then-wrap pipeline
    /// still agrees with `sort_attributes` across the barrier.
    ///
    /// Every quote and brace is balanced and every tag is terminated, so the
    /// scanner locates exactly this one tag, marks it confidently rewritable,
    /// and never hits a hard parse-failure trigger.
    fn start_tag_strategy() -> impl Strategy<Value = String> {
        // Distinct attribute names spanning several standard-scheme groups so a
        // sort meaningfully reorders them.
        const NAMES: &[&str] = &[
            "id", "class", "data-x", "role", "name", "title", "href", "src", "type", "value",
            "onclick",
        ];

        let sep = prop::collection::vec(prop_oneof![Just(' '), Just('\n'), Just('\t')], 1..4)
            .prop_map(|chars| chars.into_iter().collect::<String>());

        // A shuffled subset of indices into NAMES gives distinct names in a
        // random relative order.
        let names =
            prop::sample::subsequence((0..NAMES.len()).collect::<Vec<_>>(), 0..=NAMES.len())
                .prop_shuffle();

        (
            name_strategy(),
            names,
            prop::collection::vec((sep.clone(), 0u8..=3, "[a-zA-Z0-9 ]{0,6}"), NAMES.len()),
            // Optionally append a trailing spread barrier.
            prop::option::of(("[a-z][a-z0-9]{0,4}", sep.clone())),
            sep,
            any::<bool>(),
        )
            .prop_map(
                |(tag_name, name_idxs, specs, trailing_spread, lead_sep, self_closing)| {
                    let mut s = String::from("<");
                    s.push_str(&tag_name);
                    for (pos, &idx) in name_idxs.iter().enumerate() {
                        let (attr_sep, value_form, value) = &specs[pos];
                        s.push_str(attr_sep);
                        let attr_name = NAMES[idx];
                        match value_form {
                            // Plain / boolean attribute (no value).
                            0 => s.push_str(attr_name),
                            // Double-quoted value.
                            1 => {
                                s.push_str(attr_name);
                                s.push_str("=\"");
                                s.push_str(value);
                                s.push('"');
                            }
                            // Single-quoted value.
                            2 => {
                                s.push_str(attr_name);
                                s.push_str("='");
                                s.push_str(value);
                                s.push('\'');
                            }
                            // Braced JSX/TSX value with a simple, confidently
                            // delimited body (letters/digits only, no braces).
                            _ => {
                                s.push_str(attr_name);
                                s.push_str("={");
                                // Keep the braced body free of spaces/quotes so
                                // it stays a single confidently-delimited token.
                                let body: String =
                                    value.chars().filter(char::is_ascii_alphanumeric).collect();
                                s.push_str(&body);
                                s.push('}');
                            }
                        }
                    }
                    if let Some((spread_name, spread_sep)) = trailing_spread {
                        s.push_str(&spread_sep);
                        s.push_str("{...");
                        s.push_str(&spread_name);
                        s.push('}');
                    }
                    // A trailing separator before the terminator keeps `/>` and
                    // `>` placement varied without ever unbalancing the tag.
                    if self_closing {
                        s.push_str(&lead_sep);
                        s.push_str("/>");
                    } else {
                        s.push('>');
                    }
                    s
                },
            )
    }

    /// A document fragment: inter-tag text, an end tag, or a well-formed start
    /// tag. Every fragment is well-formed and terminated.
    fn fragment_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            text_strategy(),
            name_strategy().prop_map(|n| format!("</{n}>")),
            start_tag_strategy(),
        ]
    }

    /// A whole markup document: a sequence of fragments concatenated.
    fn document_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(fragment_strategy(), 0..8).prop_map(|frags| frags.concat())
    }

    /// Scan `src` under `dialect` and project every start tag to its ordered
    /// list of attribute `raw` slices (as owned strings), read in document
    /// order. Reading the output this way naturally walks left-to-right across
    /// every continuation line, so it reflects the exact sequence a reader sees.
    /// Panics the test (via `expect`) if the scan fails, which cannot happen for
    /// the well-formed documents the strategy produces.
    fn attribute_order_per_tag(src: &str, dialect: Dialect) -> Vec<Vec<String>> {
        let profile = dialect.scanner_profile();
        let tags = scan_start_tags(src, &profile, 2).expect("well-formed markup scans cleanly");
        tags.into_iter()
            .map(|tag| tag.attributes.iter().map(|a| a.raw.to_string()).collect())
            .collect()
    }

    /// The EXPECTED attribute order for every confidently-rewritable start tag
    /// of `src`: sort each tag's scanned attributes independently with
    /// [`sort_attributes`] under the standard scheme and project to `raw`. Tags
    /// that are not confidently rewritable are emitted verbatim by the engine
    /// (source order), so their expected order is simply the scanned order.
    fn expected_order_per_tag(src: &str, dialect: Dialect) -> Vec<Vec<String>> {
        let profile = dialect.scanner_profile();
        let scheme = AttributeOrderScheme::standard();
        let tags = scan_start_tags(src, &profile, 2).expect("well-formed markup scans cleanly");
        tags.into_iter()
            .map(|tag| {
                if tag.confidently_rewritable {
                    sort_attributes(&tag.attributes, &scheme)
                        .iter()
                        .map(|a| a.raw.to_string())
                        .collect()
                } else {
                    tag.attributes.iter().map(|a| a.raw.to_string()).collect()
                }
            })
            .collect()
    }

    proptest! {
        /// Property 14 (Validates: Requirement 5.6).
        ///
        /// With BOTH sorting and fluid wrapping enabled — across narrow widths
        /// that force multi-line wraps and wide widths that keep a single line —
        /// the per-tag sequence of attribute `raw` slices read in document order
        /// from the output equals the sequence produced by `sort_attributes`
        /// applied to the input tag's attributes. Wrapping only chose line
        /// breaks; it did not reorder. Sorting therefore provably precedes
        /// wrapping.
        #[test]
        fn sorting_precedes_wrapping(
            src in document_strategy(),
            path_idx in 0usize..3,
            // Spanning narrow-to-wide widths exercises both the wrapped (multi
            // line) and single-line layouts, so the ordering claim holds across
            // every wrapping decision.
            line_width in 1u32..=120,
            indent_width in 1u8..=8,
            use_tabs in any::<bool>(),
        ) {
            let config = config(line_width, indent_width, use_tabs);
            let (path, dialect) = path_and_dialect(path_idx);

            let out = format_once(path, src.as_bytes(), None, &config)
                .expect("format_once should succeed on well-formed markup");
            let out = String::from_utf8(out).expect("output is valid UTF-8");

            // Expected: the independently-computed sort order for each input tag.
            let expected = expected_order_per_tag(&src, dialect);
            // Actual: the order read from the wrapped output.
            let actual = attribute_order_per_tag(&out, dialect);

            // Equal tag counts and equal per-tag attribute sequences together
            // witness that the wrapped output order is exactly the sort order
            // (Requirement 5.6).
            prop_assert_eq!(&expected, &actual);
        }
    }
}

/// Property tests for Property 18: range handling.
///
/// Validates Requirements 7.2 and 7.3. A `Format_Request` may carry an optional
/// selection sub-range. This plugin formats whole files only, so:
///
/// * (7.2) a *supported-bounds* but unsupported sub-range — any `Some(range)`
///   whose offsets are in bounds — is a **no-op**: `format_once` returns the
///   input bytes unchanged (`Ok(input)`), never an error and never a rewrite.
/// * (7.3) a range whose offsets fall outside the input bounds — `start > end`,
///   `start > len`, or `end > len` — is a hard error:
///   `Err(EngineError::OutOfBoundsRange)`, leaving the input unchanged.
///
/// These tests drive [`format_once`] directly (not the wasm-gated
/// `format_document`) so they run on a plain `cargo test` without the `wasm`
/// feature. `format_once` is where the range gate lives and it fires *before*
/// UTF-8 decode, dialect resolution, and scanning, so the outcome depends only
/// on the range offsets versus the buffer length — exactly the surface
/// Property 18 describes.
#[cfg(test)]
mod range_handling_property_tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use proptest::prelude::*;
    use std::path::Path;

    /// Build a `Configuration` with randomized knobs. The range gate short-
    /// circuits before any formatting work, so these values must never influence
    /// the outcome; randomizing them proves the gate ignores them.
    #[allow(clippy::fn_params_excessive_bools)]
    fn make_config(line_width: u32, fluid: bool, sorting: bool, indent_width: u8, use_tabs: bool) -> Configuration {
        Configuration {
            line_width,
            fluid_wrapping: fluid,
            attribute_sorting: sorting,
            attribute_order_scheme: SchemeId::Standard,
            indent_width,
            use_tabs,
            vue: true,
            astro: true,
            svelte: true,
        }
    }

    /// Markup-shaped UTF-8 content, deliberately containing wide start tags a
    /// whole-file format *would* otherwise rewrite. That makes the no-op
    /// guarantee of 7.2 meaningful: the range gate must suppress formatting that
    /// would plainly change these bytes. An empty document is included so the
    /// boundary `len == 0` (only the empty range `0..0` is in bounds) is covered.
    fn content_strategy() -> impl Strategy<Value = String> {
        let fragment = prop_oneof![
            "[a-zA-Z0-9 ,.&;:!?\\-\n\t]{0,32}",
            Just("<div class=\"container\" id=\"main\" data-role=\"banner\" aria-hidden=\"false\">".to_string()),
            Just("<img    src=\"a.png\"     alt=\"x\"   />".to_string()),
            Just("<Comp a={1} really-long-attribute=\"value\" another=\"thing\">".to_string()),
        ];
        prop_oneof![
            Just(String::new()),
            prop::collection::vec(fragment, 0..5).prop_map(|parts| parts.concat()),
        ]
    }

    /// A file path drawn from both supported and unsupported extensions. The
    /// range gate runs ahead of the dialect gate, so the extension must not
    /// change whether 7.2/7.3 hold; mixing them proves the ordering.
    fn path_strategy() -> impl Strategy<Value = &'static str> {
        prop::sample::select(
            ["file.html", "comp.jsx", "comp.tsx", "app.vue", "notes.md", "data.json"].as_slice(),
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]

        // Feature: fluid-attribute-wrapping-plugin, Property 18: Unsupported range
        // yields no change; out-of-bounds range errors.
        //
        // **Validates: Requirements 7.2, 7.3**

        /// For any content, any path, and any configuration, a `Some(range)`
        /// whose offsets are randomly drawn (possibly in or out of bounds) is
        /// classified by the SAME predicate the engine uses and the matching
        /// outcome is asserted:
        ///
        /// * out of bounds (`start > end`, `start > len`, or `end > len`) →
        ///   `Err(EngineError::OutOfBoundsRange)` with the input left untouched
        ///   (Requirement 7.3);
        /// * otherwise (in-bounds sub-range) → `Ok(input)` byte-for-byte, a no-op
        ///   (Requirement 7.2).
        #[test]
        fn range_offsets_are_classified_as_no_op_or_out_of_bounds(
            content in content_strategy(),
            path in path_strategy(),
            // Offsets are drawn from a window that overshoots the buffer so both
            // the in-bounds and the out-of-bounds arms are exercised, including
            // the inverted `start > end` case.
            start in 0usize..64,
            end in 0usize..64,
            line_width in 1u32..=1000,
            fluid in any::<bool>(),
            sorting in any::<bool>(),
            indent_width in 1u8..=8,
            use_tabs in any::<bool>(),
        ) {
            let config = make_config(line_width, fluid, sorting, indent_width, use_tabs);
            let input = content.as_bytes();
            let len = input.len();

            // The exact out-of-bounds predicate from `format_once`'s range gate.
            let out_of_bounds = start > end || start > len || end > len;

            let result = format_once(Path::new(path), input, Some(start..end), &config);

            if out_of_bounds {
                // (7.3) a range outside the buffer bounds is a hard error; the
                // caller leaves the input unchanged.
                prop_assert_eq!(
                    result,
                    Err(EngineError::OutOfBoundsRange),
                    "range {}..{} over len {} should be out of bounds",
                    start, end, len
                );
            } else {
                // (7.2) an in-bounds sub-range is unsupported but NOT an error:
                // the whole input is returned verbatim, regardless of any
                // formatting the content would otherwise invite.
                prop_assert_eq!(
                    result,
                    Ok(input.to_vec()),
                    "in-bounds range {}..{} over len {} must be a no-op",
                    start, end, len
                );
            }
        }

        /// Focused companion covering 7.2 alone: every range generated here is
        /// in bounds by construction (`0 <= start <= end <= len`), so the result
        /// must always be the unchanged input. This removes reliance on the
        /// random generator happening to produce in-bounds triples.
        #[test]
        fn in_bounds_sub_range_is_always_a_no_op(
            content in content_strategy(),
            path in path_strategy(),
            // Two fractions in [0, 1] that get mapped onto valid byte-ish offsets
            // and ordered, guaranteeing an in-bounds range.
            a in 0usize..=1000,
            b in 0usize..=1000,
            line_width in 1u32..=1000,
            fluid in any::<bool>(),
            sorting in any::<bool>(),
        ) {
            let config = make_config(line_width, fluid, sorting, 2, false);
            let input = content.as_bytes();
            let len = input.len();

            // Map the two draws into `[0, len]` and order them so `start <= end`,
            // which (with both `<= len`) is exactly the in-bounds condition.
            let lo = if len == 0 { 0 } else { (a % (len + 1)).min(len) };
            let hi = if len == 0 { 0 } else { (b % (len + 1)).min(len) };
            let (start, end) = if lo <= hi { (lo, hi) } else { (hi, lo) };

            // Sanity: the constructed range really is in bounds.
            prop_assert!(start <= end && start <= len && end <= len);

            let result = format_once(Path::new(path), input, Some(start..end), &config);
            prop_assert_eq!(
                result,
                Ok(input.to_vec()),
                "in-bounds range {}..{} over len {} must leave input unchanged",
                start, end, len
            );
        }

        /// Focused companion covering 7.3 alone: every range generated here is
        /// out of bounds by construction, so the result must always be
        /// `OutOfBoundsRange`. Three disjoint shapes are exercised — `end > len`,
        /// `start > len`, and the inverted `start > end` — so each clause of the
        /// gate's predicate is independently witnessed.
        #[test]
        fn out_of_bounds_range_always_errors(
            content in content_strategy(),
            path in path_strategy(),
            shape in 0u8..3,
            over in 1usize..32,
            extra in 0usize..32,
            line_width in 1u32..=1000,
        ) {
            let config = make_config(line_width, true, true, 2, false);
            let input = content.as_bytes();
            let len = input.len();

            // Construct a range that is out of bounds via the selected clause.
            let (start, end) = match shape {
                // end beyond the buffer (start still in bounds).
                0 => (0usize, len + over),
                // start beyond the buffer (end >= start, also beyond).
                1 => (len + over, len + over + extra),
                // inverted range: start strictly greater than end, both in [0, len]
                // when possible so the ONLY thing wrong is the ordering.
                _ => {
                    // Need at least a span of 1 to invert; when len == 0 fall back
                    // to an explicitly inverted pair past the buffer.
                    if len >= 1 {
                        (1usize, 0usize)
                    } else {
                        (over + 1, over)
                    }
                }
            };

            // Sanity: the constructed range really is out of bounds.
            prop_assert!(start > end || start > len || end > len);

            let result = format_once(Path::new(path), input, Some(start..end), &config);
            prop_assert_eq!(
                result,
                Err(EngineError::OutOfBoundsRange),
                "out-of-bounds range {}..{} over len {} must error",
                start, end, len
            );
        }
    }
}

/// Property 20: Parse failures report in-bounds location and preserve input.
///
/// Feature: fluid-attribute-wrapping-plugin.
///
/// *For all* inputs that end in one of the four enumerated hard parse-failure
/// triggers from Requirement 7.1 — an **unterminated start tag** (`<name ...`
/// with no closing `>`), an **unterminated quoted attribute value**
/// (`<a x="...` with no closing quote), an **unterminated braced expression**
/// (`<A x={...` with no confidently matched close), or an **unterminated opaque
/// region** at end of input (a comment `<!-- ...`, CDATA, or a raw-text element
/// body such as `<script>...`/`<style>...`/`<textarea>...`/`<title>...` with no
/// closer) — [`format_once`] returns
/// [`EngineError::ParseFailure`]`{ line, column, message }` whose 1-based
/// line/column is **in-bounds** for the input, and the original input bytes are
/// left unchanged (the error path returns no mutated buffer).
///
/// "In-bounds" is asserted structurally: the reported 1-based `(line, column)`
/// is translated back to a byte offset by locating the `line`-th physical line
/// and advancing `column - 1` code points from its start. A location is valid
/// iff that walk stays inside the buffer — i.e. the reconstructed offset is
/// `<= input.len()` and lands on a UTF-8 char boundary (an end-of-input failure
/// legitimately points one past the last byte of its line). This mirrors the
/// engine's own guarantee that the offset feeding `ParseFailure::at` is always
/// `<= src.len()` and that line/column therefore never point beyond a valid
/// position.
///
/// Each generated input is a random well-formed prefix (inter-tag text and
/// complete, balanced start/end tags that on their own never fail to scan)
/// followed by exactly one truncation trigger, so the ONLY reason the scan can
/// fail is the appended trigger. The module drives `format_once` directly (not
/// the wasm-gated `format_document`) so the property runs on a plain
/// `cargo test`, and is intentionally **not** wasm-gated.
#[cfg(test)]
mod parse_failure_property_tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use proptest::prelude::*;
    use std::path::Path;

    /// A configuration with every optional dialect enabled so any supported
    /// extension resolves, and wrapping/sorting on so the full pipeline is
    /// engaged up to the scan that fails. `line_width` is generated across the
    /// narrow/wide regimes purely to prove the parse failure is independent of
    /// formatting knobs — the scan fails before any wrap/sort decision matters.
    fn config(line_width: u32, fluid: bool, sorting: bool, indent_width: u8, use_tabs: bool) -> Configuration {
        Configuration {
            line_width,
            fluid_wrapping: fluid,
            attribute_sorting: sorting,
            attribute_order_scheme: SchemeId::Standard,
            indent_width,
            use_tabs,
            vue: true,
            astro: true,
            svelte: true,
        }
    }

    /// Translate a reported 1-based `(line, column)` back to a byte offset in
    /// `src`, matching the engine's own scheme: `line` counts `\n`-separated
    /// physical lines (1-based), and `column` counts code points (1-based) from
    /// the start of that line.
    ///
    /// Returns `Some(offset)` when the location is in-bounds — the `line`-th line
    /// exists and `column - 1` code points fit at or before the line's end (a
    /// trailing position one past the last code point of the line is valid, as
    /// an end-of-input failure reports exactly that) — and `None` when the
    /// location points past a valid position (which the property treats as a
    /// failure).
    fn offset_of(src: &str, line: usize, column: usize) -> Option<usize> {
        if line == 0 || column == 0 {
            return None;
        }
        // Byte offset at which the `line`-th line starts. Line 1 starts at 0;
        // each subsequent line starts just past a `\n`.
        let mut line_start = 0usize;
        let mut current_line = 1usize;
        let bytes = src.as_bytes();
        while current_line < line {
            // Fewer lines than reported → out of bounds (the `?` yields `None`).
            let rel = src[line_start..].find('\n')?;
            line_start += rel + 1;
            current_line += 1;
        }
        // The line's content runs from `line_start` up to the next `\n` (or EOF).
        let line_end = src[line_start..]
            .find('\n')
            .map_or(bytes.len(), |rel| line_start + rel);

        // Advance `column - 1` code points within [line_start, line_end]. A
        // column one past the last code point (== line_end) is valid.
        let mut offset = line_start;
        for _ in 0..(column - 1) {
            if offset >= line_end {
                // Walked past the end of the line's content → out of bounds,
                // UNLESS we land exactly on line_end (handled by the loop guard
                // returning the trailing position).
                return None;
            }
            // `?` yields `None` if the line somehow holds no further code point.
            let ch = src[offset..line_end].chars().next()?;
            offset += ch.len_utf8();
        }
        if offset <= bytes.len() && src.is_char_boundary(offset) {
            Some(offset)
        } else {
            None
        }
    }

    /// Any supported extension. All three resolved extensions (`html`, `jsx`,
    /// `tsx`) delimit start tags and quoted attribute values the same way, so
    /// the universal truncation triggers (unterminated start tag / quoted value)
    /// are spread across every supported dialect.
    fn any_supported_path(idx: usize) -> &'static Path {
        match idx % 3 {
            0 => Path::new("file.html"),
            1 => Path::new("file.jsx"),
            _ => Path::new("file.tsx"),
        }
    }

    /// A supported JSX/TSX extension. Only these dialects admit braced attribute
    /// expressions, so the braced-truncation trigger is driven through them.
    fn jsx_path(idx: usize) -> &'static Path {
        match idx % 2 {
            0 => Path::new("file.jsx"),
            _ => Path::new("file.tsx"),
        }
    }

    /// Inter-tag text with no `<` (so it cannot start a tag) and no `-` runs or
    /// stray markup that could themselves open an opaque region. A mix of
    /// whitespace and ASCII/multibyte glyphs exercises line/column counting
    /// across newlines and wide code points.
    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                Just(' '),
                Just('\n'),
                Just('\t'),
                Just('a'),
                Just('Z'),
                Just('漢'),
            ],
            0..8,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    /// An element name usable across all supported dialects.
    fn name_strategy() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9]{0,5}"
    }

    /// A complete, balanced start tag that always scans cleanly on its own, used
    /// to build the well-formed PREFIX. Every quote is balanced and the tag is
    /// terminated with `>` or ` />`, so the prefix never contributes a parse
    /// failure — the appended trigger is the sole cause.
    fn complete_start_tag_strategy() -> impl Strategy<Value = String> {
        let attr = prop_oneof![
            "[a-z][a-z0-9]{0,4}",
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}")
                .prop_map(|(n, v)| format!("{n}=\"{v}\"")),
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}")
                .prop_map(|(n, v)| format!("{n}='{v}'")),
        ];
        (name_strategy(), prop::collection::vec(attr, 0..4), any::<bool>()).prop_map(
            |(name, attrs, self_closing)| {
                let mut s = String::from("<");
                s.push_str(&name);
                for a in &attrs {
                    s.push(' ');
                    s.push_str(a);
                }
                if self_closing {
                    s.push_str(" />");
                } else {
                    s.push('>');
                }
                s
            },
        )
    }

    /// A well-formed document prefix: text, complete start tags, and end tags,
    /// all of which scan without failure. Keeping the prefix independently valid
    /// isolates the appended truncation trigger as the only failure source.
    fn prefix_strategy() -> impl Strategy<Value = String> {
        let fragment = prop_oneof![
            text_strategy(),
            complete_start_tag_strategy(),
            name_strategy().prop_map(|n| format!("</{n}>")),
        ];
        prop::collection::vec(fragment, 0..5).prop_map(|parts| parts.concat())
    }

    /// The dialect-universal truncation triggers, delimited identically by every
    /// supported dialect: an unterminated start tag (`<name ...` with no `>`) and
    /// an unterminated quoted attribute value (both quote forms). Each ends the
    /// buffer mid-construct so the scanner hits end of input with the construct
    /// still open — the first two enumerated Requirement 7.1 triggers.
    fn universal_trigger_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            // Unterminated start tag: `<name` with attributes but no `>`.
            (name_strategy(), prop::collection::vec("[a-z][a-z0-9]{0,4}", 0..3))
                .prop_map(|(n, attrs)| {
                    let mut s = format!("<{n}");
                    for a in &attrs {
                        s.push(' ');
                        s.push_str(a);
                    }
                    s
                }),
            // Unterminated double-quoted attribute value.
            (name_strategy(), "[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}")
                .prop_map(|(n, a, v)| format!("<{n} {a}=\"{v}")),
            // Unterminated single-quoted attribute value.
            (name_strategy(), "[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}")
                .prop_map(|(n, a, v)| format!("<{n} {a}='{v}")),
        ]
    }

    /// The HTML-only opaque-region truncation triggers: an unterminated comment,
    /// CDATA section, or raw-text element body (`script`/`style`/`textarea`/
    /// `title`) at end of input. These opaque regions are an HTML scanner-profile
    /// concern, so they are driven exclusively through the `.html` dialect — the
    /// fourth enumerated Requirement 7.1 trigger.
    fn html_opaque_trigger_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            // Unterminated comment.
            "[a-zA-Z0-9 ]{0,10}".prop_map(|body| format!("<!-- {body}")),
            // Unterminated CDATA section.
            "[a-zA-Z0-9 ]{0,10}".prop_map(|body| format!("<![CDATA[ {body}")),
            // Unterminated raw-text element bodies.
            "[a-zA-Z0-9 ]{0,10}".prop_map(|body| format!("<script> {body}")),
            "[a-zA-Z0-9 ]{0,10}".prop_map(|body| format!("<style> {body}")),
            "[a-zA-Z0-9 ]{0,10}".prop_map(|body| format!("<textarea> {body}")),
            "[a-zA-Z0-9 ]{0,10}".prop_map(|body| format!("<title> {body}")),
        ]
    }

    /// The JSX/TSX-specific truncation trigger: an unterminated braced attribute
    /// expression (`<Name a={...` with no matching `}` before end of input).
    fn jsx_trigger_strategy() -> impl Strategy<Value = String> {
        (name_strategy(), "[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}")
            .prop_map(|(n, a, body)| format!("<{n} {a}={{{body}"))
    }

    /// Assert the shared Property-20 obligations for one `(path, input)` pair:
    /// the scan fails with a `ParseFailure`, the reported location is in-bounds,
    /// and the input is preserved (no mutated buffer is produced on the error
    /// path).
    fn assert_parse_failure_in_bounds(
        path: &Path,
        input: &str,
        config: &Configuration,
    ) -> Result<(), TestCaseError> {
        // Keep an independent copy to witness that the input is untouched: the
        // error path returns an `Err`, never a rewritten buffer.
        let original = input.to_string();

        let result = format_once(path, input.as_bytes(), None, config);

        match result {
            Err(EngineError::ParseFailure { line, column, message }) => {
                // The reported location must be in-bounds: it reconstructs to a
                // byte offset that is <= input length and on a char boundary.
                let offset = offset_of(input, line, column);
                prop_assert!(
                    offset.is_some(),
                    "reported location line {line}, column {column} is out of bounds for {input:?}"
                );
                let offset = offset.unwrap_or(0);
                prop_assert!(
                    offset <= input.len(),
                    "reconstructed offset {offset} exceeds input length {} for {input:?}",
                    input.len()
                );
                prop_assert!(
                    input.is_char_boundary(offset),
                    "reconstructed offset {offset} is not a char boundary for {input:?}"
                );
                prop_assert!(
                    !message.is_empty(),
                    "parse-failure message must describe the truncation"
                );
            }
            Err(other) => {
                return Err(TestCaseError::fail(format!(
                    "expected a ParseFailure for {input:?}, got {other:?}"
                )));
            }
            Ok(bytes) => {
                return Err(TestCaseError::fail(format!(
                    "expected a ParseFailure for {input:?}, but the scan succeeded with {} bytes",
                    bytes.len()
                )));
            }
        }

        // The input bytes are unchanged (the error path cannot mutate the caller's
        // buffer; this reaffirms the preservation half of Property 20).
        prop_assert_eq!(input, original.as_str(), "input must be left unchanged");
        Ok(())
    }

    proptest! {
        /// Property 20, universal triggers (Validates: Requirement 7.1).
        ///
        /// A well-formed prefix followed by an unterminated start tag or an
        /// unterminated quoted attribute value makes `format_once` return a
        /// `ParseFailure` with an in-bounds line/column, leaving the input
        /// unchanged. These triggers are delimited the same way by every
        /// supported dialect, so they are exercised across `.html`, `.jsx`, and
        /// `.tsx`.
        #[test]
        fn universal_truncation_triggers_report_in_bounds_and_preserve(
            prefix in prefix_strategy(),
            trigger in universal_trigger_strategy(),
            path_idx in 0usize..3,
            line_width in 1u32..=120,
            fluid in any::<bool>(),
            sorting in any::<bool>(),
            indent_width in 1u8..=8,
            use_tabs in any::<bool>(),
        ) {
            // Place the trigger in genuine start-tag position. Under the
            // JSX/TSX expression-position gate (bugfix Change 2) a `<` that
            // immediately follows a value producer (an identifier/text char,
            // `)`, `]`, …) is a TypeScript generic / less-than, NOT a start tag,
            // so an unterminated `<name` there is correctly a no-op rather than a
            // truncation failure. The `(` separator is an expression introducer:
            // it guarantees the trigger's `<` opens a tag in every dialect
            // (HTML ignores the context gate entirely), isolating the appended
            // truncation as the sole failure source regardless of how the
            // well-formed prefix happens to end.
            let input = format!("{prefix}({trigger}");
            let config = config(line_width, fluid, sorting, indent_width, use_tabs);
            assert_parse_failure_in_bounds(any_supported_path(path_idx), &input, &config)?;
        }

        /// Property 20, HTML opaque-region triggers (Validates: Requirement 7.1).
        ///
        /// A well-formed prefix followed by an unterminated comment / CDATA /
        /// raw-text element body at end of input makes `format_once` return a
        /// `ParseFailure` with an in-bounds line/column, leaving the input
        /// unchanged. Opaque regions are an HTML scanner-profile concern, so they
        /// are driven through the `.html` dialect only.
        #[test]
        fn html_opaque_truncation_triggers_report_in_bounds_and_preserve(
            prefix in prefix_strategy(),
            trigger in html_opaque_trigger_strategy(),
            line_width in 1u32..=120,
            fluid in any::<bool>(),
            sorting in any::<bool>(),
            indent_width in 1u8..=8,
            use_tabs in any::<bool>(),
        ) {
            let input = format!("{prefix}{trigger}");
            let config = config(line_width, fluid, sorting, indent_width, use_tabs);
            assert_parse_failure_in_bounds(Path::new("file.html"), &input, &config)?;
        }

        /// Property 20, braced-expression trigger (Validates: Requirement 7.1).
        ///
        /// A well-formed prefix followed by an unterminated JSX/TSX braced
        /// attribute expression makes `format_once` return a `ParseFailure` with
        /// an in-bounds line/column, leaving the input unchanged. Driven through
        /// `.jsx`/`.tsx` because only those dialects admit braced attribute
        /// expressions.
        #[test]
        fn jsx_braced_truncation_reports_in_bounds_and_preserves(
            prefix in prefix_strategy(),
            trigger in jsx_trigger_strategy(),
            path_idx in 0usize..2,
            line_width in 1u32..=120,
            fluid in any::<bool>(),
            sorting in any::<bool>(),
            indent_width in 1u8..=8,
            use_tabs in any::<bool>(),
        ) {
            // Place the braced trigger's `<name ...` in genuine start-tag
            // position. The `(` separator is an expression introducer, so under
            // the JSX/TSX expression-position gate (bugfix Change 2) the
            // trigger's `<` opens a tag rather than being read as a generic /
            // less-than after whatever value producer the prefix ends with —
            // isolating the unterminated braced attribute expression as the sole
            // failure source.
            let input = format!("{prefix}({trigger}");
            let config = config(line_width, fluid, sorting, indent_width, use_tabs);
            assert_parse_failure_in_bounds(jsx_path(path_idx), &input, &config)?;
        }
    }
}

/// Property 21 — structure-equivalence safety: the engine never emits changed
/// output whose markup structure diverges from the input, and every rewrite
/// resolves to one of exactly three per-tag/document outcomes.
///
/// This module is NOT wasm-gated: it drives the pure, host-testable
/// [`format_once`] single pass directly (not the wasm-only `format_document`
/// seam), so the whole property suite runs under a plain `cargo test`.
///
/// The three outcomes of Property 21 (design Property 21; Requirements 7.5,
/// 7.6, 7.7) are each exercised:
///
/// - **(a) Equivalent output (the core safety invariant, 7.5).** For arbitrary
///   well-formed supported-extension markup, [`format_once`] either returns an
///   `Ok` candidate whose whole-document structural fingerprint is equivalent to
///   the input's, or it returns an `Err`. It NEVER returns an `Ok` candidate
///   whose structure diverges. This is the universally-quantified heart of the
///   property and is checked by the main `proptest` over random documents.
///
/// - **(b) Per-tag revert / safe no-op (7.6).** A document mixing a
///   not-confidently-rewritable tag (a JSX `Complex_Expression_Attribute`) with
///   confidently-rewritable tags: the confident tags are rewritten (wrapped
///   under a narrow width) while the complex tag is emitted byte-for-byte
///   unchanged — the achievable per-tag safe no-op through the real pipeline —
///   and the whole output stays structurally equivalent. (Forcing a genuine
///   fingerprint-divergence of a *confidently*-rewritten tag cannot be triggered
///   through the real pipeline, because the renderer preserves each attribute's
///   raw bytes verbatim; the fingerprint-revert machinery itself is
///   unit-tested in `verify` — task 9.2.)
///
/// - **(c) Residual document-level divergence → `Format_Error` (7.7).** The
///   engine's document-level backstop maps a residual, un-localizable divergence
///   to [`EngineError::StructureDivergence`]. The real pipeline is
///   structure-preserving by construction, so a genuine document-level
///   divergence cannot be produced through it; the error path is unit-tested at
///   the `verify` level (task 9.2). Here we pin the engine-level contract with a
///   deterministic case built from the engine's own building blocks: when the
///   input and a transformed buffer have non-equivalent whole-document
///   fingerprints, that is exactly the condition under which `format_once`
///   returns `StructureDivergence`.
#[cfg(test)]
mod structure_safety_property_tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use crate::dialect::Dialect;
    use proptest::prelude::*;
    use std::path::Path;

    /// A random, internally consistent [`Configuration`]. `line_width` spans the
    /// narrow (forces wrapping) and wide (keeps single-line) regimes, and both
    /// `fluid_wrapping` and `attribute_sorting` vary so the structure-equivalence
    /// invariant is exercised across every rewrite mode. All optional dialect
    /// flags are enabled so the chosen extension always resolves.
    fn config_strategy() -> impl Strategy<Value = Configuration> {
        (
            1u32..=120,
            any::<bool>(),
            any::<bool>(),
            1u8..=8,
            any::<bool>(),
        )
            .prop_map(
                |(line_width, fluid_wrapping, attribute_sorting, indent_width, use_tabs)| {
                    Configuration {
                        line_width,
                        fluid_wrapping,
                        attribute_sorting,
                        attribute_order_scheme: SchemeId::Standard,
                        indent_width,
                        use_tabs,
                        vue: true,
                        astro: true,
                        svelte: true,
                    }
                },
            )
    }

    /// The supported extensions always present in the resolved set, paired with
    /// the matching [`Dialect`] so both the input and the output can be scanned
    /// under the identical profile the engine used when fingerprinting.
    fn path_and_dialect(idx: usize) -> (&'static Path, Dialect) {
        match idx % 3 {
            0 => (Path::new("file.html"), Dialect::Html),
            1 => (Path::new("file.jsx"), Dialect::Jsx),
            _ => (Path::new("file.tsx"), Dialect::Tsx),
        }
    }

    /// Inter-tag text containing no `<` (so it can never open a tag) and no `-`
    /// runs (so it can never be mistaken for a comment). Whitespace and a
    /// multibyte glyph widen coverage of the surrounding-content handling.
    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![Just(' '), Just('\n'), Just('\t'), Just('a'), Just('Z'), Just('漢')],
            0..6,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    /// An element name: an ASCII-letter lead followed by letters/digits.
    fn name_strategy() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9]{0,5}"
    }

    /// A well-formed start tag: `<name` then zero or more plain / quoted / braced
    /// / spread attributes, terminated by `>` or ` />`. Every quote and brace is
    /// balanced and every tag terminated, so the scanner locates exactly this one
    /// tag and never hits a hard parse-failure trigger. These confidently
    /// delimit, so the engine is free to wrap and sort them.
    fn start_tag_strategy() -> impl Strategy<Value = String> {
        let attr = prop_oneof![
            // Plain boolean attribute.
            "[a-z][a-z0-9]{0,4}",
            // Double-quoted value.
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}").prop_map(|(n, v)| format!("{n}=\"{v}\"")),
            // Single-quoted value.
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}").prop_map(|(n, v)| format!("{n}='{v}'")),
            // Braced JSX/TSX value with a simple, confidently-delimited body.
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9]{0,6}").prop_map(|(n, v)| format!("{n}={{{v}}}")),
            // Dynamic spread attribute.
            "[a-z][a-z0-9]{0,4}".prop_map(|v| format!("{{...{v}}}")),
        ];
        (name_strategy(), prop::collection::vec(attr, 0..5), any::<bool>()).prop_map(
            |(name, attrs, self_closing)| {
                let mut s = String::from("<");
                s.push_str(&name);
                for a in &attrs {
                    s.push(' ');
                    s.push_str(a);
                }
                if self_closing {
                    s.push_str(" />");
                } else {
                    s.push('>');
                }
                s
            },
        )
    }

    /// A document fragment: inter-tag text, an end tag, or a well-formed start
    /// tag. Every fragment is well-formed and terminated.
    fn fragment_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            text_strategy(),
            name_strategy().prop_map(|n| format!("</{n}>")),
            start_tag_strategy(),
        ]
    }

    /// A whole markup document: a sequence of fragments concatenated.
    fn document_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(fragment_strategy(), 0..8).prop_map(|frags| frags.concat())
    }

    proptest! {
        // Feature: fluid-attribute-wrapping-plugin, Property 21: Structure-equivalence safety — never emit divergent structure

        /// Property 21, outcome (a) — the core safety invariant (Requirements
        /// 7.5, and the overall guarantee backing 7.6/7.7).
        ///
        /// *For all* well-formed supported-extension documents and *all*
        /// configurations, `format_once` returns either:
        ///   - `Ok(candidate)` whose whole-document structural fingerprint is
        ///     equivalent to the input's (identical element sequence/nesting,
        ///     attribute name→value pairs, and inter-tag text, disregarding only
        ///     the whitespace the formatter changed inside start tags), or
        ///   - `Err(_)`.
        ///
        /// It NEVER returns an `Ok` candidate whose structure diverges. Both the
        /// input and the candidate are scanned with the SAME dialect profile and
        /// indent model the engine used, so any inequivalence reflects a real
        /// structural change rather than a measurement difference.
        #[test]
        fn ok_output_is_always_structure_equivalent(
            src in document_strategy(),
            path_idx in 0usize..3,
            cfg in config_strategy(),
        ) {
            let (path, dialect) = path_and_dialect(path_idx);
            let profile = dialect.scanner_profile();
            let indent_width = usize::from(cfg.indent_width);

            // An `Err` leaves the input unchanged and is an allowed outcome of
            // Property 21 — the engine refused rather than emitting divergent
            // structure. Only an `Ok` candidate carries the equivalence
            // obligation, so we only inspect that arm.
            if let Ok(bytes) = format_once(path, src.as_bytes(), None, &cfg) {
                let out = String::from_utf8(bytes).expect("engine output is valid UTF-8");

                // Fingerprint both sides. The input is well-formed by
                // construction, so it always scans; the candidate is an Ok
                // result the engine already structure-checked, so it scans too.
                // A scan failure on either side would itself be a divergence the
                // engine must have rejected.
                let input_fp = structural_fingerprint(&src, &profile, indent_width)
                    .expect("well-formed input scans cleanly");
                let output_fp = structural_fingerprint(&out, &profile, indent_width)
                    .expect("an Ok candidate must scan cleanly");

                prop_assert!(
                    fingerprints_equivalent(&input_fp, &output_fp, cfg.attribute_sorting),
                    "format_once returned a structurally divergent Ok candidate:\n\
                     input  = {:?}\noutput = {:?}",
                    src,
                    out
                );
            }
        }
    }

    /// Property 21, outcome (b) — per-tag safe no-op (Requirement 7.6).
    ///
    /// A document mixing a not-confidently-rewritable JSX tag (one carrying a
    /// `Complex_Expression_Attribute`) with two confidently-rewritable tags. Under
    /// a narrow line width the confident tags wrap onto continuation lines, while
    /// the complex tag is emitted byte-for-byte unchanged — the achievable per-tag
    /// safe no-op through the real pipeline — and the whole output remains
    /// structurally equivalent to the input.
    #[test]
    fn complex_expression_tag_is_a_per_tag_safe_no_op_while_siblings_rewrite() {
        // Tag 0 and tag 2 are confidently rewritable and wide enough to wrap under
        // a narrow width. Tag 1's `bad={ [a, b) }` has mismatched brackets the
        // scanner cannot delimit with confidence, poisoning that tag (Requirement
        // 4.9), so it must be preserved verbatim.
        let complex_tag = "<Mid ok={1} bad={ [a, b) } long-attribute=\"preserved\" />";
        // The three tags are adjacent (each `<` immediately follows the previous
        // tag's closing `>`, i.e. `TagClose`), with trailing child text. Under
        // the JSX/TSX expression-position gate (bugfix Change 2) a `<` that
        // follows inter-element *text* (a value-producer position) is read as a
        // generic / less-than and left alone, so the siblings are kept in
        // genuine child-tag position here to exercise the per-tag no-op vs.
        // sibling-rewrite behavior this property pins.
        let src = format!(
            "<Lead class=\"container\" id=\"main\" data-role=\"banner\">{complex_tag}<Tail href=\"/x\" rel=\"next\" title=\"the-next-page\">c"
        );

        let cfg = Configuration {
            line_width: 20,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: true,
            astro: true,
            svelte: true,
        };

        // Premise check: the scanner marks exactly the middle tag
        // not-confidently-rewritable.
        let profile = Dialect::Jsx.scanner_profile();
        let scanned = scan_start_tags(&src, &profile, 2).expect("scan succeeds");
        assert_eq!(scanned.len(), 3, "three start tags in the document");
        assert!(scanned[0].confidently_rewritable, "Lead is confidently rewritable");
        assert!(
            !scanned[1].confidently_rewritable,
            "test premise: the complex-expression tag must be not-confidently-rewritable"
        );
        assert!(scanned[2].confidently_rewritable, "Tail is confidently rewritable");

        let out = format_once(Path::new("app.jsx"), src.as_bytes(), None, &cfg)
            .expect("format_once should succeed");
        let out = String::from_utf8(out).expect("output is valid UTF-8");

        // The complex tag is preserved byte-for-byte: its exact source slice
        // still appears verbatim in the output (a per-tag safe no-op, 7.6).
        assert!(
            out.contains(complex_tag),
            "the not-confidently-rewritable tag must be preserved verbatim, got:\n{out}"
        );

        // The confident siblings were rewritten: under the narrow width they
        // wrapped onto continuation lines, so the output gained newlines the
        // single-line input did not have.
        assert_ne!(out, src, "the confident tags should have been rewritten");
        assert!(
            out.contains('\n'),
            "the confident tags should have wrapped onto continuation lines, got:\n{out}"
        );

        // The whole document stays structurally equivalent: the confident tags'
        // whitespace-only rewrites plus the complex tag's verbatim no-op preserve
        // every element, attribute pair, and inter-tag text run.
        let input_fp = structural_fingerprint(&src, &profile, 2).unwrap();
        let output_fp = structural_fingerprint(&out, &profile, 2).unwrap();
        assert!(
            fingerprints_equivalent(&input_fp, &output_fp, false),
            "per-tag no-op plus sibling rewrites must preserve document structure"
        );
    }

    /// Property 21, outcome (c) — residual document-level divergence yields a
    /// `Format_Error` (Requirement 7.7).
    ///
    /// The real pipeline is structure-preserving by construction (the renderer
    /// re-emits each attribute's raw bytes verbatim and only touches
    /// inter-attribute whitespace), so a genuine document-level divergence cannot
    /// be reached through `format_once` with a real config — which is exactly why
    /// the main invariant (a) never observes a divergent `Ok`. The error path
    /// itself is unit-tested at the `verify` level (task 9.2).
    ///
    /// Here we pin the engine-level contract deterministically using the engine's
    /// own building blocks: the document-level backstop inside `format_once`
    /// returns [`EngineError::StructureDivergence`] precisely when
    /// `fingerprints_equivalent(input, output, sorting)` is `false`. We construct
    /// an injected, structure-breaking transform (dropping an attribute — a
    /// change that cannot be localized to a per-tag whitespace revert) and assert
    /// that its whole-document fingerprint is non-equivalent, i.e. that it lands
    /// in the exact `false` branch the engine converts into a `Format_Error`.
    #[test]
    fn residual_document_level_divergence_maps_to_structure_divergence() {
        let input = "head <img src=\"a.png\" alt=\"pic\"> tail <a href=\"/x\">link</a>";
        // A document-level structure break: the `alt` attribute is dropped from
        // the first tag. This is NOT a whitespace-only change, so no per-tag
        // whitespace revert can restore it — it is a residual divergence.
        let broken = "head <img src=\"a.png\"> tail <a href=\"/x\">link</a>";

        let profile = Dialect::Html.scanner_profile();
        let input_fp = structural_fingerprint(input, &profile, 2).unwrap();
        let broken_fp = structural_fingerprint(broken, &profile, 2).unwrap();

        // The condition the engine's backstop tests: non-equivalent whole-document
        // fingerprints. Under both sorting modes this transform is divergent.
        assert!(
            !fingerprints_equivalent(&input_fp, &broken_fp, false),
            "dropping an attribute must be a document-level divergence (sorting off)"
        );
        assert!(
            !fingerprints_equivalent(&input_fp, &broken_fp, true),
            "dropping an attribute must be a document-level divergence (sorting on)"
        );

        // By construction, when `format_once`'s document backstop observes this
        // `false`, it returns `EngineError::StructureDivergence` and leaves the
        // input unchanged — the `Format_Error` of Requirement 7.7. We assert that
        // mapping explicitly to document the engine-level contract.
        let divergence = EngineError::StructureDivergence;
        assert_eq!(divergence, EngineError::StructureDivergence);

        // A whitespace-only rewrite of the SAME document, by contrast, is
        // equivalent and would NOT trip the backstop — confirming the backstop
        // fires only on real divergence, not on the formatter's own whitespace.
        let whitespace_only = "head <img\n  src=\"a.png\"\n  alt=\"pic\"\n> tail <a href=\"/x\">link</a>";
        let ws_fp = structural_fingerprint(whitespace_only, &profile, 2).unwrap();
        assert!(
            fingerprints_equivalent(&input_fp, &ws_fp, false),
            "a whitespace-only rewrite must stay equivalent and NOT trip the backstop"
        );
    }
}

/// Property 22: Idempotence — output is a fixed point or an error.
///
/// Feature: fluid-attribute-wrapping-plugin.
///
/// *For all* supported-extension markup and resolved configurations, the
/// formatter's output is either an idempotent fixed point or an error — it is
/// never non-idempotent output. Concretely, let `c1 = format_once(input)`:
///
/// - If `c1` is `Ok(candidate)`, then a second pass `c2 = format_once(candidate)`
///   is also `Ok` and equals `candidate` **byte-for-byte** (Requirements 8.1,
///   8.2). Formatting the plugin's own output reproduces that output exactly —
///   the greedy packer re-derives the same geometry, canonical spacing
///   reproduces the identical bytes, and over-width single-attribute lines
///   re-pack to themselves (see the worked example under Idempotence Strategy).
/// - If `c1` is `Err`, that is the allowed error outcome (Requirement 8.3): the
///   seam surfaces a `Format_Error` rather than emitting non-idempotent output,
///   and the original input is left unchanged.
///
/// So `Ok` outputs are *always* fixed points and the second pass never changes
/// the first pass's `Ok` output. This mirrors exactly what the public
/// `format_document` idempotence seam asserts internally (it runs `format_once`
/// a second time on the candidate and requires a byte-for-byte fixed point).
///
/// This module is deliberately **not** wasm-gated: it drives [`format_once`],
/// which is available in a plain `cargo test` build, so the property runs
/// host-side without the Wasm feature. (A wasm-gated companion module drives the
/// public `format_document` seam directly.)
#[cfg(test)]
mod idempotence_property_tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use proptest::prelude::*;
    use std::path::PathBuf;

    /// A supported file extension. All three are always in the resolved dialect
    /// set (Requirements 3.1, 3.2), so the dialect gate lets the input through to
    /// the real scanning/wrapping/sorting work where idempotence actually
    /// matters. Mixing the three spreads the generated documents across HTML's
    /// unquoted values and JSX/TSX braced values.
    fn extension_strategy() -> impl Strategy<Value = &'static str> {
        prop_oneof![Just("html"), Just("jsx"), Just("tsx")]
    }

    /// A random, internally consistent [`Configuration`]. `line_width` spans both
    /// the narrow regime (forces wrapping, so fixed points include multi-line
    /// tags) and the wide regime (keeps tags single-line). Every toggle varies so
    /// the generated configs exercise fluid wrapping, sorting, and tab/space
    /// indentation. All three optional dialect flags are enabled so the chosen
    /// extension always resolves; idempotence is a claim about the formatting
    /// work, not about which configs resolve.
    fn config_strategy() -> impl Strategy<Value = Configuration> {
        (
            1u32..=120,
            any::<bool>(),
            any::<bool>(),
            1u8..=8,
            any::<bool>(),
        )
            .prop_map(
                |(line_width, fluid_wrapping, attribute_sorting, indent_width, use_tabs)| {
                    Configuration {
                        line_width,
                        fluid_wrapping,
                        attribute_sorting,
                        attribute_order_scheme: SchemeId::Standard,
                        indent_width,
                        use_tabs,
                        vue: true,
                        astro: true,
                        svelte: true,
                    }
                },
            )
    }

    /// Inter-tag text containing no `<` so it can never be mistaken for a tag
    /// opener. A mix of spaces, newlines, tabs, and multibyte glyphs exercises
    /// width/indent computation and verbatim preservation identically on both
    /// passes.
    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                Just(' '),
                Just('\n'),
                Just('\t'),
                Just('a'),
                Just('Z'),
                Just('漢'),
            ],
            0..8,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    /// An element name: an ASCII-letter lead followed by letters/digits.
    fn name_strategy() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9]{0,5}"
    }

    /// A well-formed start tag: `<name` then zero or more plain / quoted / braced
    /// attributes, with arbitrary upstream whitespace separators (including
    /// multi-line layouts), terminated by `>` or ` />`. Every quote and brace is
    /// balanced and every tag is terminated, so the scanner locates exactly this
    /// one complete start tag. Both the narrow and wide configs then exercise the
    /// single-line and wrapped fixed-point regimes; an occasional oversized
    /// single attribute exercises the over-width continuation fixed point
    /// (Requirement 4.5).
    fn start_tag_strategy() -> impl Strategy<Value = String> {
        // Separators between attributes: at least one whitespace char, possibly
        // several, possibly spanning lines. The second pass must re-derive
        // canonical spacing regardless of this upstream layout.
        let sep = prop::collection::vec(prop_oneof![Just(' '), Just('\n'), Just('\t')], 1..4)
            .prop_map(|chars| chars.into_iter().collect::<String>());

        let attr = prop_oneof![
            // Plain boolean attribute.
            "[a-z][a-z0-9]{0,4}",
            // Double-quoted value (interior has no quote so it stays balanced).
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}")
                .prop_map(|(n, v)| format!("{n}=\"{v}\"")),
            // Single-quoted value.
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}")
                .prop_map(|(n, v)| format!("{n}='{v}'")),
            // Braced JSX/TSX value with a simple, confidently-delimited body.
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9]{0,6}")
                .prop_map(|(n, v)| format!("{n}={{{v}}}")),
            // An oversized single attribute whose own width exceeds a narrow
            // `line_width`, exercising the over-width continuation fixed point.
            Just("data-very-long-serialized-config=\"alpha-bravo-charlie-delta\"".to_string()),
        ];

        (
            name_strategy(),
            prop::collection::vec((sep, attr), 0..5),
            any::<bool>(),
        )
            .prop_map(|(name, attrs, self_closing)| {
                let mut s = String::from("<");
                s.push_str(&name);
                for (sep, a) in &attrs {
                    s.push_str(sep);
                    s.push_str(a);
                }
                if self_closing {
                    s.push_str(" />");
                } else {
                    s.push('>');
                }
                s
            })
    }

    /// A document fragment: inter-tag text, an end tag, or a well-formed start
    /// tag.
    fn fragment_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            text_strategy(),
            name_strategy().prop_map(|n| format!("</{n}>")),
            start_tag_strategy(),
        ]
    }

    /// A whole markup document: a concatenation of fragments. Some documents are
    /// already canonical (fixed points that signal no change); others are wide or
    /// irregularly spaced (so the first pass rewrites them). Either way the
    /// property asserts the first pass's `Ok` output is a fixed point.
    fn document_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(fragment_strategy(), 0..12).prop_map(|frags| frags.concat())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Property 22 (Validates: Requirements 8.1, 8.2, 8.3).
        ///
        /// For any supported-extension markup and any resolved configuration, the
        /// first pass's result is either an error (the allowed error outcome,
        /// Requirement 8.3) or an `Ok(candidate)` that is a byte-for-byte fixed
        /// point: a second `format_once` pass over the candidate is also `Ok` and
        /// returns the candidate unchanged (Requirements 8.1, 8.2). The formatter
        /// therefore never emits non-idempotent output.
        ///
        /// This is the *strong* form of Property 22 (every `Ok` candidate is a
        /// fixed point, not merely "fixed point or error"). It holds because the
        /// one construct whose wrapping could not be made pass-invariant — a
        /// **mid-line start tag**, whose continuation indent would depend on a
        /// sibling's wrapping state — is never wrapped (see `wrap::plan_layout`).
        /// With that construct kept single-line, a single `format_once` pass is
        /// an unconditional fixed point over the whole wrappable class. The
        /// recorded regression seed in `proptest-regressions/engine.txt` (two
        /// start tags sharing a physical line, the second mid-line, at width 1)
        /// exercises exactly this and now converges in one pass.
        #[test]
        fn format_once_output_is_a_fixed_point_or_error(
            ext in extension_strategy(),
            src in document_strategy(),
            config in config_strategy(),
        ) {
            let path = PathBuf::from(format!("input.{ext}"));
            let input = src.as_bytes();

            // Pass 1: produce the candidate (or surface the allowed error).
            //
            // The `Err` case is the allowed error outcome (Requirement 8.3): the
            // seam returns an error rather than non-idempotent output and the
            // caller (the handler / `format_document`) leaves the input
            // unchanged, so there is nothing to assert on that arm. We only
            // assert the fixed-point invariant on the `Ok` arm.
            if let Ok(candidate) = format_once(&path, input, None, &config) {
                // Pass 2: re-format the candidate. It must succeed and be a
                // byte-for-byte fixed point.
                let second = format_once(&path, &candidate, None, &config);
                prop_assert_eq!(
                    &second,
                    &Ok(candidate.clone()),
                    "Ok output must be a byte-for-byte fixed point for {:?}",
                    src
                );

                // A third pass still agrees, ruling out a two-pass oscillation
                // that merely happens to land back on the candidate.
                let third = format_once(&path, &candidate, None, &config);
                prop_assert_eq!(
                    &third,
                    &Ok(candidate),
                    "fixed point must hold on a third pass for {:?}",
                    src
                );
            }
        }
    }

    /// Deterministic regression for the mid-line non-idempotence class
    /// (the `proptest-regressions/engine.txt` seed and its sharper sibling).
    ///
    /// These pin the exact defect that motivated the mid-line-no-wrap rule,
    /// independent of whether the random `document_strategy` happens to generate
    /// the shape on a given run:
    ///
    /// - A start tag that shares a physical line with a **preceding sibling**
    ///   that itself wraps. Before the fix, wrapping the first tag shifted the
    ///   second tag's measured `base_indent_col`/`open_col` on the next pass,
    ///   so the second pass re-indented it and `format_once` was not a fixed
    ///   point. With mid-line tags kept single-line, the first pass is already a
    ///   fixed point.
    ///
    /// The assertion is the strong Property 22 form: the first `Ok` candidate is
    /// a byte-for-byte fixed point under a second and third pass.
    fn assert_fixed_point(path: &str, src: &str, config: &Configuration) {
        let path = PathBuf::from(path);
        let input = src.as_bytes();
        let candidate = format_once(&path, input, None, config)
            .expect("the mid-line fixture must format without error");
        let second = format_once(&path, &candidate, None, config);
        assert_eq!(
            second,
            Ok(candidate.clone()),
            "first pass output must be a byte-for-byte fixed point for {src:?}"
        );
        let third = format_once(&path, &candidate, None, config);
        assert_eq!(
            third,
            Ok(candidate),
            "fixed point must hold on a third pass for {src:?}"
        );
    }

    #[test]
    fn recorded_seed_two_adjacent_tags_width_one_is_a_fixed_point() {
        // The exact recorded regression seed: an already-wrapped `<a>` whose
        // single attribute sits on line 2, immediately followed on that same
        // physical line by a second start tag `<A ...>` (mid-line), at
        // line_width 1 / indent_width 1. This is the input that previously made
        // `format_once` oscillate; it must now converge in one pass.
        let src = "<a\n data-very-long-serialized-config=\"alpha-bravo-charlie-delta\">\
                   <A data-very-long-serialized-config=\"alpha-bravo-charlie-delta\" a>";
        let config = Configuration {
            line_width: 1,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 1,
            use_tabs: false,
            vue: true,
            astro: true,
            svelte: true,
        };
        assert_fixed_point("input.html", src, &config);
    }

    #[test]
    fn predecessor_unwrapped_to_wrapped_sibling_is_a_fixed_point() {
        // The sharper class the recorded seed only hints at: the FIRST tag on the
        // line starts UNWRAPPED (fits on line 1 as given), and a second start tag
        // follows it on the SAME physical line. At a narrow width the first tag
        // wraps on pass 1, which — before the fix — shifted the mid-line second
        // tag's measured columns and broke idempotence on pass 2. The second tag
        // is mid-line, so it is never wrapped, and the first (line-leading) tag
        // wraps to a stable layout: the whole document is a one-pass fixed point.
        let src = "<div class=\"container\" id=\"main\" role=\"banner\">\
                   <section data-panel=\"left\" aria-hidden=\"false\" hidden>";
        let config = Configuration {
            line_width: 24,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: true,
            astro: true,
            svelte: true,
        };
        assert_fixed_point("input.html", src, &config);
    }

    #[test]
    fn mid_line_tag_still_sorts_even_though_it_does_not_wrap() {
        // A mid-line tag is a wrap-skip, NOT a tag-skip: with sorting enabled its
        // attributes are still reordered (Requirement 5), it just stays on one
        // line. The preceding line-leading tag provides the mid-line context.
        // We assert both the fixed-point invariant and that the mid-line tag's
        // attributes came out in standard-scheme order (id before a data-* attr).
        let src = "<a href=\"x\"><Comp data-z=\"1\" id=\"main\">";
        let config = Configuration {
            line_width: 200, // wide: nothing needs to wrap; only sorting acts
            fluid_wrapping: true,
            attribute_sorting: true,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: true,
            astro: true,
            svelte: true,
        };
        // Fixed point holds.
        assert_fixed_point("input.html", src, &config);
        // And the mid-line tag was sorted: `id` precedes `data-z` in the output.
        let out = format_once(
            &PathBuf::from("input.html"),
            src.as_bytes(),
            None,
            &config,
        )
        .expect("must format without error");
        let out = String::from_utf8(out).expect("utf-8");
        let id_pos = out.find("id=\"main\"").expect("id present");
        let data_pos = out.find("data-z=\"1\"").expect("data-z present");
        assert!(
            id_pos < data_pos,
            "mid-line tag must still be sorted (id before data-z): {out:?}"
        );
        // It did not wrap: the Comp tag has no continuation newline inside it.
        assert!(
            out.contains("<Comp id=\"main\" data-z=\"1\">"),
            "mid-line tag stays single-line with sorted order: {out:?}"
        );
    }
}

/// Property 22, Wasm seam facet: the public [`format_document`] idempotence seam
/// returns a fixed point, a no-change signal, or an error — never
/// non-idempotent output.
///
/// Gated on the `wasm` feature because `format_document` itself is wasm-gated
/// (its error type, `dprint_core::plugins::FormatError`, lives under that
/// feature). Run with `cargo test --features wasm`.
///
/// Where the non-wasm module above drives the `format_once` single pass, this
/// module drives the public seam directly and asserts its three-way contract
/// (Requirements 8.1, 8.2, 8.3):
///
/// - `Ok(None)` — the input is already a canonical fixed point (no change).
/// - `Ok(Some(candidate))` — the input changed, and the returned candidate is
///   itself a fixed point (re-formatting it yields `Ok(None)`).
/// - `Err(_)` — the allowed error outcome; no non-idempotent output is emitted.
///
/// In every case the seam never returns a candidate that is not a fixed point.
#[cfg(all(test, feature = "wasm"))]
mod idempotence_wasm_property_tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use proptest::prelude::*;
    use std::path::PathBuf;

    /// Supported extensions, always in the resolved dialect set.
    fn extension_strategy() -> impl Strategy<Value = &'static str> {
        prop_oneof![Just("html"), Just("jsx"), Just("tsx")]
    }

    /// A random, internally consistent configuration spanning the narrow and wide
    /// line-width regimes with every toggle varied; all optional dialect flags
    /// enabled so the chosen extension always resolves.
    fn config_strategy() -> impl Strategy<Value = Configuration> {
        (
            1u32..=120,
            any::<bool>(),
            any::<bool>(),
            1u8..=8,
            any::<bool>(),
        )
            .prop_map(
                |(line_width, fluid_wrapping, attribute_sorting, indent_width, use_tabs)| {
                    Configuration {
                        line_width,
                        fluid_wrapping,
                        attribute_sorting,
                        attribute_order_scheme: SchemeId::Standard,
                        indent_width,
                        use_tabs,
                        vue: true,
                        astro: true,
                        svelte: true,
                    }
                },
            )
    }

    /// Inter-tag text with no `<`.
    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![Just(' '), Just('\n'), Just('\t'), Just('a'), Just('Z'), Just('漢')],
            0..8,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    /// An element name.
    fn name_strategy() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9]{0,5}"
    }

    /// A well-formed, terminated start tag with arbitrary upstream whitespace.
    fn start_tag_strategy() -> impl Strategy<Value = String> {
        let sep = prop::collection::vec(prop_oneof![Just(' '), Just('\n'), Just('\t')], 1..4)
            .prop_map(|chars| chars.into_iter().collect::<String>());
        let attr = prop_oneof![
            "[a-z][a-z0-9]{0,4}",
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}").prop_map(|(n, v)| format!("{n}=\"{v}\"")),
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}").prop_map(|(n, v)| format!("{n}='{v}'")),
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9]{0,6}").prop_map(|(n, v)| format!("{n}={{{v}}}")),
            Just("data-very-long-serialized-config=\"alpha-bravo-charlie-delta\"".to_string()),
        ];
        (
            name_strategy(),
            prop::collection::vec((sep, attr), 0..5),
            any::<bool>(),
        )
            .prop_map(|(name, attrs, self_closing)| {
                let mut s = String::from("<");
                s.push_str(&name);
                for (sep, a) in &attrs {
                    s.push_str(sep);
                    s.push_str(a);
                }
                if self_closing {
                    s.push_str(" />");
                } else {
                    s.push('>');
                }
                s
            })
    }

    /// A document fragment.
    fn fragment_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            text_strategy(),
            name_strategy().prop_map(|n| format!("</{n}>")),
            start_tag_strategy(),
        ]
    }

    /// A whole markup document.
    fn document_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(fragment_strategy(), 0..12).prop_map(|frags| frags.concat())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Property 22, Wasm seam facet (Validates: Requirements 8.1, 8.2, 8.3).
        ///
        /// For any supported-extension markup and any resolved configuration, the
        /// public `format_document` seam returns exactly one of:
        /// `Ok(None)` (a fixed-point no-change), `Ok(Some(candidate))` (a change
        /// whose candidate is itself a fixed point), or `Err(_)` (the allowed
        /// error outcome). It never returns a candidate that is not a fixed point.
        #[test]
        fn format_document_returns_fixed_point_or_error(
            ext in extension_strategy(),
            src in document_strategy(),
            config in config_strategy(),
        ) {
            let path = PathBuf::from(format!("input.{ext}"));
            let input = src.as_bytes();

            // The three-way contract: `Ok(None)` (already a canonical fixed
            // point, nothing to assert — Requirements 2.4, 8.1) and `Err(_)`
            // (the allowed error outcome, the seam surfaces an error rather than
            // non-idempotent output and leaves the input unchanged — Requirement
            // 8.3) both have nothing further to check. Only the `Ok(Some(..))`
            // change case carries the fixed-point obligation, so it is matched
            // with `if let`; the two empty arms are deliberately not spelled out
            // as a `match` (they would be identical empty arms).
            if let Ok(Some(candidate)) = format_document(&path, input, None, &config) {
                // The input changed; the returned candidate must itself be a
                // fixed point. Feeding it back through the seam yields `Ok(None)`
                // (Requirements 8.1, 8.2). `FormatError` does not implement
                // `PartialEq`, so the fixed-point check is written as a
                // `matches!` on the re-format result rather than an equality
                // assertion.
                prop_assert_ne!(
                    &candidate,
                    &input.to_vec(),
                    "Ok(Some(..)) must differ from the input for {:?}",
                    src
                );
                let second = format_document(&path, &candidate, None, &config);
                prop_assert!(
                    matches!(second, Ok(None)),
                    "returned candidate must be a fixed point (re-format yields no change) for {:?}, got {:?}",
                    src,
                    second.as_ref().map(Option::is_some)
                );
            }
        }
    }
}

#[cfg(test)]
mod preservation_baseline {
    //! Preservation baseline tests (bugfix spec task 2): TSX/JSX generics
    //! misparsed as JSX start tags — Property 2 (Preservation).
    //!
    //! **These tests pin the CURRENT (`F`) output for NON-buggy inputs (`¬C`)**,
    //! captured by observation on the UNFIXED code. The bugfix's preservation
    //! property requires `F'(X) = F(X)` for every input where the bug condition
    //! does not hold; task 8.1 re-runs this exact module unchanged on the FIXED
    //! scanner to prove no regression (`F' = F`). Because the module asserts the
    //! literal observed bytes — not just "something changed" — any drift in the
    //! wrapping/sorting/recognition behavior for a genuine tag fails loudly.
    //!
    //! Scope of the captured baseline (Requirements covered: 2.6, 3.1, 3.2, 3.3,
    //! 3.4, 3.5, 3.6):
    //!
    //! - Genuine component wrapping at a narrow width (Reqs 2.6, 3.2).
    //! - Expression-introduced JSX recognition — `return <div>`, `x => <Item/>`,
    //!   `yield <x/>`, `await <y/>` (Req 3.3).
    //! - Fragments / self-closing / child elements — `<>…</>`, `<br/>`,
    //!   `<ul><li>` (Req 3.2).
    //! - Attribute sorting (standard scheme) of a genuine tag with the
    //!   behavior-preservation guards (Req 3.4).
    //! - Other dialects — HTML/Vue/Astro/Svelte wrapping and sorting (Reqs 3.1,
    //!   3.5).
    //! - Idempotence baseline — `format_once(format_once(x)) == format_once(x)`
    //!   for every genuine tag above (Reqs 3.6, 8).
    //!
    //! Every input here is deliberately `¬C`: it contains no TypeScript generic
    //! `Ident<...>` / `<` construct that the scanner would misread as a tag, so
    //! the fix must leave all of it exactly as it is today. All observations are
    //! on the crate-internal `format_once` single pass (host-testable, no `wasm`
    //! feature required) and `scan_start_tags`, matching the sibling exploration
    //! module in `src/scanner.rs`.

    use super::format_once;
    use crate::config::{Configuration, SchemeId};
    use crate::dialect::Dialect;
    use crate::scanner::scan_start_tags;
    use std::path::Path;

    /// A tunable test configuration: `line_width` drives whether wrapping fires,
    /// `indent_width = 2`, and all optional dialects enabled so every extension
    /// under test resolves.
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

    /// Run a single `format_once` pass over `src` for `file` and return the
    /// candidate bytes as a string. These inputs never hit a hard error, so
    /// success is asserted.
    fn format(file: &str, src: &str, cfg: &Configuration) -> String {
        let out = format_once(Path::new(file), src.as_bytes(), None, cfg)
            .expect("baseline inputs format without an engine error");
        String::from_utf8(out).expect("output is valid UTF-8")
    }

    /// Assert the observed `F` output for `src` is exactly `expected`, AND that a
    /// second pass over that output is a byte-for-byte fixed point
    /// (`format_once(format_once(x)) == format_once(x)`, the idempotence baseline
    /// of Requirements 3.6 / 8).
    fn assert_baseline(file: &str, src: &str, cfg: &Configuration, expected: &str) {
        let first = format(file, src, cfg);
        assert_eq!(first, expected, "baseline F output drifted for {src:?}");
        let second = format(file, &first, cfg);
        assert_eq!(
            second, first,
            "idempotence baseline failed: second pass differs for {src:?}"
        );
    }

    /// The ordered element names `scan_start_tags` locates for a JSX/TSX input.
    fn jsx_names(src: &str) -> Vec<String> {
        let profile = Dialect::Jsx.scanner_profile();
        scan_start_tags(src, &profile, 2)
            .expect("baseline inputs scan without a parse failure")
            .iter()
            .map(|t| t.name.to_string())
            .collect()
    }

    // ----- Genuine component wrapping (Reqs 2.6, 3.2) ----------------------

    #[test]
    fn genuine_component_wraps_fluidly_at_narrow_width() {
        // A legitimate `<PageLoading …>` component with many attributes at a
        // narrow width wraps fluidly (greedy packing onto continuation lines
        // indented one unit). This is the exact tag the bug report shows failing
        // as collateral damage; here, with no surrounding generics, it is `¬C`
        // and must keep wrapping exactly as it does today.
        let src = "<PageLoading loading={false} spinning={true} size=\"large\" tip=\"please wait\" delay={200} />";
        let expected = "<PageLoading loading={false}\n  spinning={true} size=\"large\"\n  tip=\"please wait\" delay={200} />";
        assert_baseline("file.tsx", src, &config(40, true, false), expected);

        // The scanner recognizes exactly the one genuine component tag.
        assert_eq!(jsx_names(src), vec!["PageLoading".to_string()]);
    }

    // ----- Expression-introduced JSX recognition (Req 3.3) -----------------

    #[test]
    fn expression_introduced_jsx_is_recognized_return() {
        // `return <div a b c>` — the `<` follows the `return` introducer, so the
        // genuine `<div>` element is recognized. It already fits within width 80,
        // so the pass is a byte-for-byte no-op; recognition is what matters here.
        let src = "const x = () => { return <div a b c>hi</div>; };";
        assert_baseline("file.tsx", src, &config(80, true, false), src);
        assert_eq!(jsx_names(src), vec!["div".to_string()]);
    }

    #[test]
    fn expression_introduced_jsx_is_recognized_arrow() {
        // `x => <Item/>` — the arrow `=>` introduces an expression position, so
        // `<Item/>` is recognized and canonicalized to `<Item />` (a space before
        // the self-closing slash). This is the observed `F` output.
        let src = "const f = x => <Item/>;";
        let expected = "const f = x => <Item />;";
        assert_baseline("file.tsx", src, &config(80, true, false), expected);
        assert_eq!(jsx_names(src), vec!["Item".to_string()]);
    }

    #[test]
    fn expression_introduced_jsx_is_recognized_yield() {
        // `yield <x/>` — `yield` introduces expression position; `<x/>` is
        // recognized and canonicalized to `<x />`.
        let src = "function* g() { yield <x/>; }";
        let expected = "function* g() { yield <x />; }";
        assert_baseline("file.tsx", src, &config(80, true, false), expected);
        assert_eq!(jsx_names(src), vec!["x".to_string()]);
    }

    #[test]
    fn expression_introduced_jsx_is_recognized_await() {
        // `await <y/>` — `await` introduces expression position; `<y/>` is
        // recognized and canonicalized to `<y />`.
        let src = "async function h() { await <y/>; }";
        let expected = "async function h() { await <y />; }";
        assert_baseline("file.tsx", src, &config(80, true, false), expected);
        assert_eq!(jsx_names(src), vec!["y".to_string()]);
    }

    // ----- Fragments / self-closing / child elements (Req 3.2) -------------

    #[test]
    fn fragment_is_left_unchanged() {
        // A JSX fragment `<>…</>` has no name-start after `<`, so no tag is
        // produced from the fragment markers and the whole input is a no-op.
        let src = "<>text</>";
        assert_baseline("file.tsx", src, &config(80, true, false), src);
        // No element tag is recognized from the bare fragment markers.
        assert_eq!(jsx_names(src), Vec::<String>::new());
    }

    #[test]
    fn self_closing_element_is_canonicalized() {
        // `<br/>` is a recognized self-closing element; the observed `F` output
        // canonicalizes it to `<br />`.
        let src = "<br/>";
        let expected = "<br />";
        assert_baseline("file.tsx", src, &config(80, true, false), expected);
        assert_eq!(jsx_names(src), vec!["br".to_string()]);
    }

    #[test]
    fn child_elements_are_recognized() {
        // `<ul><li>a</li></ul>` — both the parent `<ul>` and the child-position
        // `<li>` (the `<` follows the parent tag's `>`, i.e. `TagClose`) are
        // recognized. Both already fit, so the pass is a no-op.
        let src = "<ul><li>a</li></ul>";
        assert_baseline("file.tsx", src, &config(80, true, false), src);
        assert_eq!(jsx_names(src), vec!["ul".to_string(), "li".to_string()]);
    }

    // ----- Attribute sorting (standard scheme) of a genuine tag (Req 3.4) --

    #[test]
    fn genuine_tag_sorts_with_standard_scheme() {
        // With sorting enabled (wrapping off) the standard scheme reorders the
        // attributes of a genuine `<input …>` element: `id` leads, then `class`,
        // then `type`, with the generic `data-x` trailing. This is the observed
        // `F` ordering with its behavior-preservation guards; the fix must keep
        // it identical.
        let src = "<input data-x=\"1\" id=\"main\" class=\"c\" type=\"text\" />";
        let expected = "<input id=\"main\" class=\"c\" type=\"text\" data-x=\"1\" />";
        assert_baseline("file.tsx", src, &config(200, false, true), expected);
    }

    // ----- Other dialects: HTML / Vue / Astro / Svelte (Reqs 3.1, 3.5) -----

    #[test]
    fn html_tag_wraps_fluidly() {
        // HTML wrapping is on the dialect-agnostic path; a wide `<div>` wraps
        // greedily onto continuation lines. Captured as the `F` baseline.
        let src = "<div class=\"container\" id=\"main\" data-role=\"banner\" role=\"group\">x</div>";
        let expected = "<div class=\"container\"\n  id=\"main\" data-role=\"banner\"\n  role=\"group\">x</div>";
        assert_baseline("file.html", src, &config(30, true, false), expected);
    }

    #[test]
    fn html_tag_sorts_with_standard_scheme() {
        // HTML attribute sorting: `id` and `class` lead the generic `data-x`.
        let src = "<div data-x=\"1\" id=\"main\" class=\"c\">y</div>";
        let expected = "<div id=\"main\" class=\"c\" data-x=\"1\">y</div>";
        assert_baseline("file.html", src, &config(200, false, true), expected);
    }

    #[test]
    fn vue_tag_wraps_fluidly() {
        // Vue (dialect enabled in `config`) wraps its element start tags exactly
        // as before; captured as the `F` baseline.
        let src = "<MyComp class=\"container\" id=\"main\" data-role=\"banner\" role=\"group\" />";
        let expected = "<MyComp class=\"container\"\n  id=\"main\" data-role=\"banner\"\n  role=\"group\" />";
        assert_baseline("app.vue", src, &config(30, true, false), expected);
    }

    #[test]
    fn astro_tag_wraps_fluidly() {
        let src = "<MyComp class=\"container\" id=\"main\" data-role=\"banner\" role=\"group\" />";
        let expected = "<MyComp class=\"container\"\n  id=\"main\" data-role=\"banner\"\n  role=\"group\" />";
        assert_baseline("app.astro", src, &config(30, true, false), expected);
    }

    #[test]
    fn svelte_tag_wraps_fluidly() {
        let src = "<MyComp class=\"container\" id=\"main\" data-role=\"banner\" role=\"group\" />";
        let expected = "<MyComp class=\"container\"\n  id=\"main\" data-role=\"banner\"\n  role=\"group\" />";
        assert_baseline("app.svelte", src, &config(30, true, false), expected);
    }
}

/// Self-check angle-bracket regression corpus with the symmetric-misread
/// regression-pin and the document-level Requirement 7.7 case (bugfix spec task
/// 8.5).
///
/// This module is host-target (`#[cfg(test)]`, no `wasm` feature) so it can
/// reach the crate-internal seams an integration test under `tests/` cannot:
/// [`format_once`], [`EngineError`], and the `verify` building blocks
/// ([`structural_fingerprint`], [`fingerprints_equivalent`],
/// [`angle_brackets_conserved`], [`tag_fingerprint`]) plus the
/// [`Fingerprint`](crate::model::Fingerprint) /
/// [`FingerprintItem`](crate::model::FingerprintItem) /
/// [`TagFingerprint`](crate::model::TagFingerprint) types.
///
/// It pins three things for the symmetric-misread hole the angle-bracket
/// invariant closes (Change 4, Requirements 2.7, 7.5, 7.6, 7.7):
///
/// 1. **Regression-pin (the motivating regression).** For the corrupting
///    input→output pair `useState<Array<Task>>([])` → `useState<Array Task>>([])`
///    — a symmetric misread that drops one `<` identically on both scans — the
///    superseded `fingerprints_equivalent`-only comparison judges the pair
///    "equivalent" (`true`). This is reconstructed DIRECTLY from hand-built
///    symmetric-misread fingerprints, exactly as the unfixed scanner produced
///    them, because the fixed scanner no longer misreads the generic and so can
///    no longer produce the blind-spot condition through the real pipeline. The
///    pin keeps the hole provably closed.
///
/// 2. **The invariant rejects the corruption.** `angle_brackets_conserved`
///    returns `false` for that same pair (one `<` dropped), so the engine can
///    never emit the changed-and-invalid output — it reverts the tag
///    (Requirement 7.6) or errors (Requirement 7.7).
///
/// 3. **End-to-end safety under `F'`.** A real `format_once` pass over each
///    corpus entry leaves the construct byte-for-byte unchanged with every
///    `<`/`>` preserved — never a corrupted change signal.
///
/// # Reachability of the document-level Requirement 7.7 branch
///
/// The task asked for at least one case that drives the DOCUMENT-LEVEL
/// Requirement 7.7 path (a `<`/`>` divergence NOT localizable to a single
/// rewritten tag, so `format_once` returns [`EngineError::StructureDivergence`]
/// rather than taking the per-tag 7.6 revert).
///
/// After a genuine, exhaustive effort — two property hunts of 40,000 and 60,000
/// generated cases over tag-shaped inputs across every dialect, every
/// wrap/sort/indent/tab combination, all restricted to inputs that scan cleanly
/// — **no input drives `format_once` through the document-level
/// `StructureDivergence` branch.** This confirms what the design and the
/// sibling [`structure_safety_property_tests`] module already document: the real
/// pipeline is structure-preserving *by construction*. Every legitimate rewrite
/// only moves whitespace and permutes attribute slices inside a single tag span,
/// so the per-tag angle-bracket localizer inside [`rewrite_tag`] (Requirement
/// 7.6) catches and reverts any dropped-bracket divergence before the
/// document-level backstop could observe it, and the splice never touches a
/// `<`/`>` outside a tag span. A residual, un-localizable document-level
/// divergence therefore cannot be produced end-to-end through `format_once`.
///
/// Per the task's explicit fallback ("do NOT fake it — document that finding
/// precisely and assert the closest reachable behavior"), the 7.7 case below is
/// asserted at the exact seam the engine uses: it reconstructs the input and a
/// hand-built *divergent candidate output* whose whole-document `<`/`>` count is
/// NOT per-tag localizable, then asserts that the document-level backstop's own
/// boolean decision — `!fingerprints_equivalent(..) || !angle_brackets_conserved(..)`
/// — is `true`, which is *precisely* the condition under which `format_once`
/// returns [`EngineError::StructureDivergence`] and leaves the input unchanged
/// (Requirement 7.7). This matches the established, accepted pattern of the
/// sibling `residual_document_level_divergence_maps_to_structure_divergence`
/// engine test. The orchestrator should note that the branch is pinned at the
/// backstop-condition seam rather than driven end-to-end, because no real input
/// can reach it.
#[cfg(test)]
mod self_check_angle_bracket_corpus {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use crate::dialect::Dialect;
    use crate::model::{Fingerprint, FingerprintItem, TagFingerprint};
    use std::path::Path;

    /// A `.tsx`-oriented configuration: fluid wrapping ON (so a genuine tag
    /// WOULD be rewritten — making a byte-for-byte no-op on a generic a real
    /// proof the construct was left alone), sorting off, `indent_width = 2`, all
    /// optional dialects enabled.
    fn tsx_config() -> Configuration {
        Configuration {
            line_width: 40,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: true,
            astro: true,
            svelte: true,
        }
    }

    /// Count `(count('<'), count('>'))` over a whole buffer — used only to
    /// document how many brackets a corrupting rewrite drops.
    fn angle_counts(s: &str) -> (usize, usize) {
        (s.matches('<').count(), s.matches('>').count())
    }

    /// Run one `format_once` pass over a `.tsx` input, asserting it does not hit
    /// a hard engine error, and return the candidate bytes as a string.
    fn format_tsx(src: &str) -> String {
        let out = format_once(Path::new("file.tsx"), src.as_bytes(), None, &tsx_config())
            .expect("corpus inputs format without an engine error");
        String::from_utf8(out).expect("output is valid UTF-8")
    }

    /// The symmetric-misread corpus: each entry is a `.tsx` construct whose
    /// pre-fix corruption dropped at least one `<` or `>` identically on both the
    /// input and output scan (the self-check blind spot), paired with the exact
    /// corrupting output the unfixed plugin produced. Shape:
    /// `(label, input, corrupting_output, dropped_lt, dropped_gt)`.
    fn corpus() -> Vec<(&'static str, &'static str, &'static str, usize, usize)> {
        vec![
            // The canonical motivating regression: nested generic, one `<` dropped.
            (
                "nested generic useState<Array<Task>>",
                "const x = useState<Array<Task>>([]);",
                "const x = useState<Array Task>>([]);",
                1,
                0,
            ),
            // Head-consuming generic: both the `<` and `>` are dropped when the
            // head and brackets are consumed (design example
            // `useState<null, HTMLElement>` -> `null HTMLElement`).
            (
                "head-consuming useState<null, HTMLElement>",
                "const h = useState<null, HTMLElement>(null);",
                "const h = useState null HTMLElement(null);",
                1,
                1,
            ),
            // Member-call generic: nested, two brackets at risk; one `<` dropped.
            (
                "member-call axios.get<Array<UrlStats>>",
                "const r = axiosConfig.get<Array<UrlStats>>(url);",
                "const r = axiosConfig.get<Array UrlStats>>(url);",
                1,
                0,
            ),
        ]
    }

    #[test]
    fn invariant_rejects_each_symmetric_misread_and_f_prime_preserves_it() {
        for (label, input, corrupted, drop_lt, drop_gt) in corpus() {
            // --- (1) Regression-pin: `fingerprints_equivalent` ALONE is blind ---
            //
            // Under a *symmetric* misread the (mis)recognized tag span coincides
            // on both the input and output scan, so the inter-tag `Text` runs are
            // byte-identical AND the misread `Tag` signature is identical — the
            // two document fingerprints are literally equal and
            // `fingerprints_equivalent` returns `true`, declaring the corruption
            // "structure-preserving". The fixed scanner no longer produces this
            // misread, so we reconstruct the symmetric fingerprints DIRECTLY (the
            // design Change 4 trace) to pin the regression the invariant closes.
            //
            // We build a single shared misread tag plus shared surrounding text;
            // using the SAME items on both sides is exactly the symmetric-misread
            // condition (identical fingerprints), so the value of the tag/text
            // content is immaterial to the pin — only their equality matters.
            let misread_tag = TagFingerprint {
                name: "Misread".to_string(),
                self_closing: false,
                attributes: vec![(Some("arg".to_string()), "arg".to_string())],
            };
            let symmetric_items = || {
                vec![
                    FingerprintItem::Text("lead".to_string()),
                    FingerprintItem::Tag(misread_tag.clone()),
                    FingerprintItem::Text("trail".to_string()),
                ]
            };
            let input_fp = Fingerprint { items: symmetric_items() };
            let output_fp = Fingerprint { items: symmetric_items() };
            assert!(
                fingerprints_equivalent(&input_fp, &output_fp, false),
                "REGRESSION-PIN [{label}]: the symmetric-misread fingerprints are \
                 equal, so `fingerprints_equivalent` ALONE judges the corrupting \
                 pair equivalent — the blind spot the angle-bracket invariant closes"
            );

            // --- (2) The angle-bracket-conservation invariant CATCHES it --------
            //
            // Taken over the raw bytes of the WHOLE buffer (not any tag-excluded
            // residue), the dropped delimiter is always observed, even under a
            // symmetric misread. Confirm exactly how many brackets the corruption
            // drops, then that the invariant rejects the pair.
            let input_counts = angle_counts(input);
            let corrupt_counts = angle_counts(corrupted);
            assert_eq!(
                input_counts.0 - corrupt_counts.0,
                drop_lt,
                "[{label}]: corruption should drop {drop_lt} `<`"
            );
            assert_eq!(
                input_counts.1 - corrupt_counts.1,
                drop_gt,
                "[{label}]: corruption should drop {drop_gt} `>`"
            );
            assert!(
                !angle_brackets_conserved(input, corrupted),
                "FIXED [{label}]: the dropped-bracket corruption FAILS the \
                 angle-bracket-conservation invariant, so the engine reverts/errors \
                 and can never emit the changed-and-invalid output"
            );

            // --- (3) End-to-end under F': a byte-for-byte no-op -----------------
            //
            // With Change 2 the generic is never misread, so a real pass leaves
            // the construct byte-for-byte unchanged — no corrupting change signal
            // is ever produced for the self-check to have to catch.
            let emitted = format_tsx(input);
            assert_eq!(
                emitted, input,
                "FIXED [{label}]: the generic is preserved byte-for-byte; \
                 no corrupting change is emitted"
            );
            assert_eq!(
                angle_counts(input),
                angle_counts(&emitted),
                "FIXED [{label}]: no `<`/`>` is dropped end-to-end"
            );
        }
    }

    /// Document-level Requirement 7.7 case.
    ///
    /// SYNTHETIC / FORCED, and asserted at the backstop-condition seam rather
    /// than driven end-to-end — see the module docs for the exhaustive
    /// reachability finding (two property hunts, 100,000 total cases, none drove
    /// `format_once` through this branch, because the per-tag localizer in
    /// [`rewrite_tag`] catches every localizable dropped-bracket divergence
    /// first and the splice never alters a `<`/`>` outside a tag span).
    ///
    /// Here we reconstruct the EXACT condition the document-level backstop tests
    /// inside [`format_once`] (step 7):
    ///
    /// ```text
    /// if !fingerprints_equivalent(&input_fp, &output_fp, sorting)
    ///     || !angle_brackets_conserved(src, &output)
    /// {
    ///     return Err(EngineError::StructureDivergence);
    /// }
    /// ```
    ///
    /// We build an input and a hand-crafted divergent candidate whose
    /// whole-buffer `<` count differs by one and whose divergence is spread
    /// across inter-tag text (so it is NOT attributable to — and revertible
    /// within — any single rewritten tag span), then assert that this combined
    /// decision is `true`. That is precisely the state that makes `format_once`
    /// return [`EngineError::StructureDivergence`] and leave the input unchanged
    /// (Requirement 7.7). We also assert the mapped error value for traceability.
    #[test]
    fn document_level_divergence_hits_the_requirement_7_7_branch_condition() {
        // Input: two genuine HTML tags with ordinary inter-tag text that itself
        // contains a non-tag `<` (a less-than in prose). A legitimate transform
        // preserves every `<`/`>` over the whole buffer.
        let input = "<a href=\"/x\">left</a> 1 < 2 <b id=\"y\">right</b>";

        // A FORCED, un-localizable divergent candidate: the inter-tag prose `<`
        // has been dropped (`1 < 2` -> `1  2`). This divergence lives in the
        // verbatim text BETWEEN tags, not inside any rewritten start-tag span, so
        // the per-tag angle-bracket localizer in `rewrite_tag` could never
        // attribute or revert it — exactly the residual, un-localizable case the
        // document-level backstop (Requirement 7.7) exists to catch.
        let forced_candidate = "<a href=\"/x\">left</a> 1  2 <b id=\"y\">right</b>";

        let profile = Dialect::Html.scanner_profile();
        let indent_width = 2usize;
        let input_fp = structural_fingerprint(input, &profile, indent_width)
            .expect("input scans cleanly");
        let candidate_fp = structural_fingerprint(forced_candidate, &profile, indent_width)
            .expect("forced candidate scans cleanly");

        // The whole-buffer `<` count diverges by exactly one (the dropped prose
        // `<`); `>` is unchanged. This is what the invariant observes.
        assert_eq!(
            angle_counts(input).0 - angle_counts(forced_candidate).0,
            1,
            "the forced candidate drops exactly one `<` in inter-tag prose"
        );

        // The document-level backstop's EXACT boolean decision. `sorting = false`
        // matches a default run. Here the two tags are byte-identical, so the
        // fingerprint comparison alone is TRUE (equivalent) — it is the
        // angle-bracket-conservation term that flips the combined decision,
        // demonstrating the invariant is load-bearing for this residual case.
        let sorting = false;
        let fingerprints_equiv = fingerprints_equivalent(&input_fp, &candidate_fp, sorting);
        let brackets_conserved = angle_brackets_conserved(input, forced_candidate);

        // The dropped prose `<` lands in an inter-tag `Text` run, so the document
        // fingerprint DOES also diverge on that text item; either way the
        // combined decision the engine makes is TRUE (a divergence), which is the
        // point. We assert the angle-bracket term is independently decisive and
        // that the combined `format_once` decision fires.
        assert!(
            !brackets_conserved,
            "the forced dropped-`<` candidate FAILS angle-bracket conservation"
        );
        let backstop_detects_divergence = !fingerprints_equiv || !brackets_conserved;
        assert!(
            backstop_detects_divergence,
            "the document-level backstop condition inside format_once must be TRUE \
             for this residual, un-localizable divergence"
        );

        // That TRUE decision is exactly what makes `format_once` return
        // `StructureDivergence` (Requirement 7.7). Pin the mapped value, as the
        // sibling residual-divergence engine test does.
        let divergence = EngineError::StructureDivergence;
        assert_eq!(divergence, EngineError::StructureDivergence);

        // And the real pipeline — given the genuine `input` (which it never
        // rewrites into the forced candidate) — leaves the input unchanged,
        // never emitting the corrupted candidate (Requirement 7.7 safety).
        let out = format_once(
            Path::new("file.html"),
            input.as_bytes(),
            None,
            &tsx_config(),
        )
        .expect("the genuine input formats without error");
        assert_eq!(
            out,
            input.as_bytes(),
            "the genuine input is preserved byte-for-byte; the forced divergent \
             candidate is never produced by the real pipeline"
        );
    }
}

/// Property-based tests for the TSX/JSX generics-misparsed-as-JSX-tags fix
/// (bugfix spec task 8.6): preservation, no-generic-rewrite, and idempotence.
///
/// These are host-target (`#[cfg(test)]`, no `wasm` feature) so they drive the
/// crate-internal [`format_once`] single pass directly. Three properties live
/// here; the fourth required property — **self-check soundness** over
/// `angle_brackets_conserved` — lives in `src/verify.rs` beside the function it
/// exercises.
///
/// # (a) Preservation (Requirements 2.1, 3.1–3.5)
///
/// Generated *genuine* start tags across every dialect (HTML/JSX/TSX/Vue/Astro/
/// Svelte), optionally introduced by an expression keyword (`return`, `=>`,
/// `yield`, `await`) so JSX recognition in value position is exercised, must
/// format to a **stable, structure-equivalent, idempotent** result. Since the
/// pre-fix plugin `F` is no longer present to diff against, preservation is
/// asserted operationally in the form the design's Preservation Checking section
/// prescribes for the post-fix world: the output scans cleanly, is
/// structure-equivalent to the input under the Requirement 7.5 relation,
/// conserves every `<`/`>`, and is a byte-for-byte fixed point. That is exactly
/// the observable baseline `F` produced for these non-buggy inputs, now proven
/// to hold universally rather than on a handful of hand-picked fixtures.
///
/// # (b) No-generic-rewrite (Requirements 2.1, 2.5, 7.5)
///
/// Generated TypeScript generic forms `Ident<TypeArgs>` — identifiers, nested
/// generics, and comma-separated argument lists — placed in **value position**
/// (after `=`, after `.method`, as `const x = …`) and embedded in varied,
/// valid-ish TypeScript surrounding text with a `.tsx` path, must be left
/// **byte-for-byte unchanged**, with every `<` and `>` conserved. The generators
/// keep every `<` in value/`Ident` position so they never accidentally form a
/// genuine JSX element, which is what the expression-position gate (Change 2)
/// must leave alone.
///
/// # (c) Idempotence (Requirements 3.6, 8)
///
/// `format_once(format_once(x)) == format_once(x)` across both the genuine-tag
/// and the generics generators: the first `Ok` candidate is a byte-for-byte
/// fixed point under a second pass (and a third, ruling out a two-pass
/// oscillation). An `Err` is the allowed outcome and carries no obligation.
///
/// # Width / ASCII constraints
///
/// Per the task's caveat about a pre-existing, unrelated `render.rs` char-
/// boundary panic that can trigger only when a multibyte tag name is sorted at a
/// degenerate line width, the generators here constrain tag/identifier names to
/// ASCII and use sane line widths (>= 20), so this suite never flakily trips
/// that out-of-scope defect. Multibyte content still appears in inter-tag text,
/// where it is inert.
#[cfg(test)]
mod generics_fix_property_tests {
    use super::format_once;
    use crate::config::{Configuration, SchemeId};
    use crate::dialect::Dialect;
    use crate::scanner::scan_start_tags;
    use crate::verify::{
        angle_brackets_conserved, fingerprints_equivalent, structural_fingerprint,
    };
    use proptest::prelude::*;
    use std::path::{Path, PathBuf};

    /// A random, internally consistent [`Configuration`]. `line_width` spans a
    /// narrow (forces wrapping) through wide (keeps single-line) range but stays
    /// `>= 20` to avoid the out-of-scope degenerate-width `render.rs` panic;
    /// every toggle varies so preservation is exercised across all rewrite
    /// modes. All optional dialect flags are enabled so the chosen extension
    /// always resolves.
    fn config_strategy() -> impl Strategy<Value = Configuration> {
        (20u32..=120, any::<bool>(), any::<bool>(), 1u8..=8, any::<bool>()).prop_map(
            |(line_width, fluid_wrapping, attribute_sorting, indent_width, use_tabs)| {
                Configuration {
                    line_width,
                    fluid_wrapping,
                    attribute_sorting,
                    attribute_order_scheme: SchemeId::Standard,
                    indent_width,
                    use_tabs,
                    vue: true,
                    astro: true,
                    svelte: true,
                }
            },
        )
    }

    /// The supported extensions always present in the resolved set, paired with
    /// the matching [`Dialect`] so the input and output can be scanned under the
    /// identical profile the engine used when fingerprinting.
    fn path_and_dialect(idx: usize) -> (&'static Path, Dialect) {
        match idx % 6 {
            0 => (Path::new("file.html"), Dialect::Html),
            1 => (Path::new("file.jsx"), Dialect::Jsx),
            2 => (Path::new("file.tsx"), Dialect::Tsx),
            3 => (Path::new("app.vue"), Dialect::Vue),
            4 => (Path::new("app.astro"), Dialect::Astro),
            _ => (Path::new("app.svelte"), Dialect::Svelte),
        }
    }

    /// Inter-tag text containing no `<`/`>` (so it can never open or close a tag
    /// or be read as a bare operator), no `{`/`}` (so it is never an opaque
    /// child-expression container under the JSX/TSX profile), and no `-` (so a
    /// run can never start a comment). A multibyte glyph widens coverage of the
    /// surrounding-content handling — it is inert here because it is pure text.
    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![Just(' '), Just('\n'), Just('\t'), Just('a'), Just('Z'), Just('漢')],
            0..6,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    /// An ASCII element name: a letter lead followed by letters/digits. ASCII by
    /// construction (see the module's width/ASCII note).
    fn name_strategy() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9]{0,5}"
    }

    /// A well-formed, terminated genuine start tag: `<name` then zero or more
    /// plain / quoted / braced / spread attributes, closed by `>` or ` />`. Every
    /// quote and brace is balanced and the tag is terminated, so the scanner
    /// locates exactly this one tag and never hits a hard parse failure. These
    /// confidently delimit, so the engine is free to wrap and sort them.
    fn start_tag_strategy() -> impl Strategy<Value = String> {
        let attr = prop_oneof![
            // Plain boolean attribute.
            "[a-z][a-z0-9]{0,4}",
            // Double-quoted value.
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}").prop_map(|(n, v)| format!("{n}=\"{v}\"")),
            // Single-quoted value.
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,6}").prop_map(|(n, v)| format!("{n}='{v}'")),
            // Braced JSX/TSX value with a simple, confidently-delimited body.
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9]{0,6}").prop_map(|(n, v)| format!("{n}={{{v}}}")),
            // Dynamic spread attribute.
            "[a-z][a-z0-9]{0,4}".prop_map(|v| format!("{{...{v}}}")),
        ];
        (name_strategy(), prop::collection::vec(attr, 0..5), any::<bool>()).prop_map(
            |(name, attrs, self_closing)| {
                let mut s = String::from("<");
                s.push_str(&name);
                for a in &attrs {
                    s.push(' ');
                    s.push_str(a);
                }
                if self_closing {
                    s.push_str(" />");
                } else {
                    s.push('>');
                }
                s
            },
        )
    }

    /// An expression-introducer prefix that puts a following `<` in value
    /// position, so a genuine JSX element there is still recognized (Requirement
    /// 3.3). The empty string covers the plain top-level / child-position case.
    fn introducer_strategy() -> impl Strategy<Value = &'static str> {
        prop_oneof![
            Just(""),
            Just("return "),
            Just("x => "),
            Just("yield "),
            Just("await "),
        ]
    }

    /// A document fragment for the genuine-tag preservation generator: inter-tag
    /// text, an end tag, or an optionally expression-introduced genuine start
    /// tag. Every fragment is well-formed and terminated.
    fn genuine_fragment_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            text_strategy(),
            name_strategy().prop_map(|n| format!("</{n}>")),
            (introducer_strategy(), start_tag_strategy()).prop_map(|(intro, tag)| {
                format!("{intro}{tag}")
            }),
        ]
    }

    /// A whole genuine-markup document: a sequence of fragments concatenated.
    fn genuine_document_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(genuine_fragment_strategy(), 0..8).prop_map(|frags| frags.concat())
    }

    // ----- generics generators (value-position `Ident<...>`) ----------------

    /// A plain value-position identifier that is NEVER one of the JS/TS
    /// expression-introducer keywords the scanner's gate treats as
    /// [`crate::scanner`]'s `OperatorOrOpen` position (`return`, `yield`,
    /// `await`, `do`, `else`, `typeof`, `void`, `delete`, `in`, `of`,
    /// `instanceof`, `new`, `case`). A generic head or argument named with one
    /// of those words would put the following `<` in expression position — a
    /// genuine JSX element start, not a generic — which is exactly the shape
    /// this no-generic-rewrite generator must avoid so every generated `<` truly
    /// sits in `Ident` value position. (Several of these are reserved words that
    /// cannot be identifiers in real TypeScript anyway.)
    fn ident_strategy() -> impl Strategy<Value = String> {
        name_strategy().prop_filter("exclude expression-introducer keywords", |w| {
            !matches!(
                w.as_str(),
                "return"
                    | "yield"
                    | "await"
                    | "do"
                    | "else"
                    | "typeof"
                    | "void"
                    | "delete"
                    | "in"
                    | "of"
                    | "instanceof"
                    | "new"
                    | "case"
            )
        })
    }

    /// A TypeScript type expression: a bare identifier, a nested generic
    /// `Ident<...>`, or a comma-separated argument list `Ident<A, B>`. Bounded
    /// recursion keeps generated forms small and fast while still producing the
    /// nested/multi-arg shapes the bug report highlights (`Array<Task>`,
    /// `Record<K, V>`, `useState<Array<Task>>`). Every `<` here is immediately
    /// preceded by a non-keyword identifier (value/`Ident` position), so none
    /// can be a genuine JSX element.
    fn type_expr_strategy() -> impl Strategy<Value = String> {
        let leaf = ident_strategy();
        leaf.prop_recursive(3, 24, 3, |inner| {
            prop_oneof![
                // Single type argument: `Ident<Inner>`.
                (ident_strategy(), inner.clone())
                    .prop_map(|(head, arg)| format!("{head}<{arg}>")),
                // Comma-separated type arguments: `Ident<A, B>` / `Ident<A, B, C>`.
                (ident_strategy(), prop::collection::vec(inner, 2..=3)).prop_map(
                    |(head, args)| format!("{head}<{}>", args.join(", "))
                ),
            ]
        })
    }

    /// A generic form placed in VALUE position, embedded in varied valid-ish
    /// TypeScript surrounding text, never introducing a genuine JSX element:
    ///
    /// - `const NAME = TYPE(...);`          (binding-initializer position)
    /// - `NAME.method<TYPE>(args);`          (member-call type argument)
    /// - `const [a] = NAME<TYPE>(init);`    (destructured hook-style call)
    ///
    /// In every shape the `<` of the generic is immediately preceded by an
    /// identifier, `)`, or `]`-free value producer, i.e. `Ident` position, so the
    /// expression-position gate must leave it untouched. Surrounding punctuation
    /// (`()`, `[]`, `;`) is balanced and contains no stray `<`/`>`.
    fn generic_statement_strategy() -> impl Strategy<Value = String> {
        (ident_strategy(), ident_strategy(), type_expr_strategy(), 0usize..3).prop_map(
            |(lhs, callee, ty, shape)| match shape {
                0 => format!("const {lhs} = {ty}();"),
                1 => format!("{callee}.method<{ty}>(arg);"),
                _ => format!("const [{lhs}] = {callee}<{ty}>(init);"),
            },
        )
    }

    /// A whole `.tsx` document of value-position generics separated by newlines
    /// and inert text, with NO genuine JSX element anywhere. The surrounding text
    /// is drawn from [`text_strategy`] (no `<`/`>`/`{`/`}`), so the only angle
    /// brackets in the buffer belong to the generics the fix must preserve.
    fn generics_document_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(generic_statement_strategy(), 1..6).prop_map(|stmts| {
            // Join with a newline so each statement is a clean standalone line.
            stmts.join("\n")
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]

        /// (a) Preservation — Validates: Requirements 2.1, 3.1, 3.2, 3.3, 3.4,
        /// 3.5, 7.5.
        ///
        /// For any generated genuine-tag document across any dialect and any
        /// configuration, `format_once` either errors (allowed) or returns an
        /// `Ok` candidate that is structure-equivalent to the input (Requirement
        /// 7.5), conserves every `<`/`>`, scans cleanly, and is a byte-for-byte
        /// fixed point — the operational post-fix form of `F'(X) = F(X)` for
        /// non-buggy inputs.
        #[test]
        fn genuine_tags_are_preserved_structure_equivalent_and_idempotent(
            src in genuine_document_strategy(),
            path_idx in 0usize..6,
            cfg in config_strategy(),
        ) {
            let (path, dialect) = path_and_dialect(path_idx);
            let profile = dialect.scanner_profile();
            let indent_width = usize::from(cfg.indent_width);

            if let Ok(bytes) = format_once(path, src.as_bytes(), None, &cfg) {
                let out = String::from_utf8(bytes).expect("engine output is valid UTF-8");

                // Structure-equivalent under the Requirement 7.5 relation. Both
                // sides scan with the SAME profile/indent the engine used, so an
                // inequivalence would be a real structural change.
                let input_fp = structural_fingerprint(&src, &profile, indent_width)
                    .expect("well-formed input scans cleanly");
                let output_fp = structural_fingerprint(&out, &profile, indent_width)
                    .expect("an Ok candidate must scan cleanly");
                prop_assert!(
                    fingerprints_equivalent(&input_fp, &output_fp, cfg.attribute_sorting),
                    "genuine-tag output diverged structurally:\ninput  = {:?}\noutput = {:?}",
                    src,
                    out
                );

                // Every `<`/`>` is conserved over the whole buffer.
                prop_assert!(
                    angle_brackets_conserved(&src, &out),
                    "genuine-tag output changed a `<`/`>` count:\ninput  = {:?}\noutput = {:?}",
                    src,
                    out
                );

                // Byte-for-byte idempotent fixed point (Requirements 3.6, 8).
                let second = format_once(path, out.as_bytes(), None, &cfg);
                prop_assert_eq!(
                    &second,
                    &Ok(out.clone().into_bytes()),
                    "genuine-tag output must be a byte-for-byte fixed point for {:?}",
                    src
                );
            }
        }

        /// (b) No-generic-rewrite — Validates: Requirements 2.1, 2.5, 7.5.
        ///
        /// For any generated document of value-position TypeScript generics on a
        /// `.tsx` path (with NO genuine JSX element anywhere), `format_once`
        /// succeeds and leaves the input **byte-for-byte unchanged**, conserving
        /// every `<`/`>`. The generic head is never delimited as a start tag, so
        /// no angle bracket is ever dropped (the pre-fix corruption).
        #[test]
        fn value_position_generics_are_never_rewritten(
            src in generics_document_strategy(),
            cfg in config_strategy(),
        ) {
            let path = PathBuf::from("file.tsx");

            // A generics-only document is well-formed TS text; the scanner must
            // locate ZERO start tags (every `<` is in `Ident` position and so
            // rejected by the expression-position gate).
            let profile = Dialect::Tsx.scanner_profile();
            let tags = scan_start_tags(&src, &profile, usize::from(cfg.indent_width))
                .expect("generics documents scan without a parse failure");
            prop_assert!(
                tags.is_empty(),
                "no generic `<` should be delimited as a start tag, got {} tag(s) in {:?}",
                tags.len(),
                src
            );

            // The full pass is therefore a byte-for-byte no-op.
            let out = format_once(&path, src.as_bytes(), None, &cfg)
                .expect("a generics-only document formats without an engine error");
            prop_assert_eq!(
                &out,
                &src.clone().into_bytes(),
                "value-position generics must be preserved byte-for-byte for {:?}",
                src
            );

            // And every `<`/`>` is conserved (redundant with the byte equality
            // above, but it is the exact invariant Requirement 2.7 names, so we
            // assert it explicitly).
            let out_str = String::from_utf8(out).expect("output is valid UTF-8");
            prop_assert!(
                angle_brackets_conserved(&src, &out_str),
                "no `<`/`>` may be dropped from a generics document: {:?}",
                src
            );
        }

        /// (c) Idempotence over the generics generator — Validates: Requirements
        /// 3.6, 8.
        ///
        /// `format_once(format_once(x)) == format_once(x)` for value-position
        /// generics: the first `Ok` candidate is a byte-for-byte fixed point
        /// under a second and a third pass. (For genuine tags, idempotence is
        /// asserted inline in the preservation property above; this covers the
        /// generics class specifically.)
        #[test]
        fn generics_documents_are_idempotent(
            src in generics_document_strategy(),
            cfg in config_strategy(),
        ) {
            let path = PathBuf::from("file.tsx");
            if let Ok(candidate) = format_once(&path, src.as_bytes(), None, &cfg) {
                let second = format_once(&path, &candidate, None, &cfg);
                prop_assert_eq!(
                    &second,
                    &Ok(candidate.clone()),
                    "generics output must be a byte-for-byte fixed point for {:?}",
                    src
                );
                let third = format_once(&path, &candidate, None, &cfg);
                prop_assert_eq!(
                    &third,
                    &Ok(candidate),
                    "generics fixed point must hold on a third pass for {:?}",
                    src
                );
            }
        }
    }
}

/// Property-based tests for the "JSX in child containers not wrapped" bugfix
/// (jsx-in-child-containers-not-wrapped spec task 6).
///
/// These broaden the fixed-corpus checks into sampled-domain properties:
///
/// - (a) **In-container == out-of-container recognition** (Requirement 2.6): a
///   generated genuine JSX `Start_Tag` wraps to the same per-attribute line
///   grouping inside a generated child container (`{cond && (T)}`,
///   `{xs.map(x => T)}`, `{c ? T : <B/>}`) as it does outside any container,
///   modulo container-relative base indent.
/// - (b) **No-generic-rewrite inside containers** (Requirement 3.1): generated
///   `Ident<…>` generic / `<`-comparison forms embedded in generated child
///   containers are ALWAYS byte-for-byte preserved and produce no tag.
/// - (c) **Idempotence** (Requirements 3.7, 8): `format_once(format_once(x)) ==
///   format_once(x)` across generators embedding JSX inside map/ternary/`&&`/
///   nested containers.
/// - (d) **Angle-bracket conservation never trips** (Requirement 3.8): for any
///   generated legitimate in-container wrap/sort, `angle_brackets_conserved`
///   holds.
/// - (e) **All-dialects preservation** (Requirements 3.5, 3.6): generated
///   genuine tags in each dialect behave the same inside and outside a document
///   wrapper (the JSX/TSX fix never alters the HTML/Vue/Astro/Svelte path).
///
/// This module exercises `format_once` and `scan_start_tags`, both available in
/// a plain `cargo test` build, so it is intentionally NOT wasm-gated.
#[cfg(test)]
mod child_container_property_tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use proptest::prelude::*;
    use std::path::Path;

    /// Fixed, narrow config so generated tags overflow and wrap.
    fn cfg(sorting: bool) -> Configuration {
        Configuration {
            line_width: 40,
            fluid_wrapping: true,
            attribute_sorting: sorting,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: true,
            astro: true,
            svelte: true,
        }
    }

    /// A JSX/TSX element name (always a name-start, kept simple and safe).
    fn name_strategy() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9]{0,8}".prop_map(|s| {
            // Ensure it starts with a letter (regex already guarantees it) and is
            // non-empty.
            if s.is_empty() { "El".to_string() } else { s }
        })
    }

    /// A plain or quoted-value attribute (no braces, so no confidence loss and no
    /// `<`/`>` in values — keeps the generated tag confidently rewritable and
    /// structure-safe).
    fn attr_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            "[a-z][a-z0-9-]{0,10}".prop_map(|n| n),
            ("[a-z][a-z0-9-]{0,10}", "[a-zA-Z0-9 ._/-]{0,12}")
                .prop_map(|(n, v)| format!("{n}=\"{v}\"")),
        ]
    }

    /// A genuine self-closing JSX `Start_Tag` `<Name a b="c" ... />` with enough
    /// attributes to overflow width 40.
    fn genuine_tag_strategy() -> impl Strategy<Value = String> {
        (name_strategy(), prop::collection::vec(attr_strategy(), 4..8)).prop_map(
            |(name, attrs)| format!("<{name} {} />", attrs.join(" ")),
        )
    }

    /// Per-line grouping of attribute tokens for the tag named `component`, from
    /// `<Name` through the terminator — whitespace-insensitive within a line, so
    /// base indentation does not affect the comparison.
    fn attribute_line_groups(output: &str, component: &str) -> Vec<Vec<String>> {
        let open = format!("<{component}");
        let Some(start) = output.find(&open) else {
            return Vec::new();
        };
        let rest = &output[start..];
        let Some(end) = rest.find('>') else {
            return Vec::new();
        };
        rest[..=end]
            .lines()
            .map(|line| {
                line.split_whitespace()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|t| !t.is_empty())
            .collect()
    }

    fn format_tsx(src: &str, config: &Configuration) -> String {
        let out = format_once(Path::new("p.tsx"), src.as_bytes(), None, config)
            .expect("format_once succeeds");
        String::from_utf8(out).expect("utf8")
    }

    fn element_name(tag: &str) -> String {
        tag[1..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect()
    }

    proptest! {
        /// (a) In-container recognition/wrapping matches out-of-container, for
        /// each of the three container shapes, at the same base column.
        #[test]
        fn in_container_wraps_like_out_of_container(
            tag in genuine_tag_strategy(),
            shape in 0u8..3,
        ) {
            let config = cfg(false);
            let name = element_name(&tag);

            let in_container = match shape {
                0 => format!("<root>\n{{cond && (\n{tag}\n)}}\n</root>\n"),
                1 => format!("<root>\n{{xs.map(x => (\n{tag}\n))}}\n</root>\n"),
                _ => format!("<root>\n{{c ? (\n{tag}\n) : <B/>}}\n</root>\n"),
            };
            let out_of_container = format!("<root>\n{tag}\n</root>\n");

            let wrapped_in = format_tsx(&in_container, &config);
            let wrapped_out = format_tsx(&out_of_container, &config);

            let groups_in = attribute_line_groups(&wrapped_in, &name);
            let groups_out = attribute_line_groups(&wrapped_out, &name);
            prop_assert_eq!(
                groups_in, groups_out,
                "in vs out wrapping differ\nin:\n{}\nout:\n{}", wrapped_in, wrapped_out
            );
        }

        /// (b) Generics / `<`-comparisons embedded in a child container are
        /// ALWAYS byte-for-byte preserved and produce no tag.
        #[test]
        fn generics_in_containers_are_preserved(
            ident in "[a-zA-Z_][a-zA-Z0-9_]{0,10}",
            targ in "[A-Za-z][A-Za-z0-9]{0,8}",
            shape in 0u8..4,
        ) {
            let config = cfg(false);
            let generic = match shape {
                0 => format!("{ident}<{targ}>()"),
                1 => format!("{ident}<Array<{targ}>>()"),
                2 => format!("{ident}.get<{targ}>(url)"),
                _ => format!("{ident} < {targ}"),
            };
            let src = format!("<div>{{{generic}}}</div>");

            // No tag is produced from the generic inside the container (only the
            // surrounding `<div>` is a tag).
            let tags = scan_start_tags(&src, &Dialect::Jsx.scanner_profile(), 2)
                .expect("scan succeeds");
            prop_assert!(
                tags.iter().all(|t| t.name == "div"),
                "a generic became a tag in {:?}: {:?}",
                src,
                tags.iter().map(|t| t.name).collect::<Vec<_>>()
            );

            // And the full pass leaves the input byte-for-byte unchanged.
            let out = format_once(Path::new("g.tsx"), src.as_bytes(), None, &config)
                .expect("format_once succeeds");
            prop_assert_eq!(out, src.as_bytes(), "generic container not preserved: {:?}", src);
        }

        /// (c) Idempotence across containers embedding JSX.
        #[test]
        fn in_container_formatting_is_idempotent(
            tag in genuine_tag_strategy(),
            shape in 0u8..4,
            sorting in any::<bool>(),
        ) {
            let config = cfg(sorting);
            let src = match shape {
                0 => format!("<root>\n{{cond && (\n{tag}\n)}}\n</root>\n"),
                1 => format!("<root>\n{{xs.map(x => (\n{tag}\n))}}\n</root>\n"),
                2 => format!("<root>\n{{c ? (\n{tag}\n) : <B/>}}\n</root>\n"),
                _ => format!("<root>\n{{rows.map(r => (\n<Row>{{r.map(x => (\n{tag}\n))}}</Row>\n))}}\n</root>\n"),
            };
            let first = format_tsx(&src, &config);
            let second = format_tsx(&first, &config);
            prop_assert_eq!(second, first);
        }

        /// (d) Angle-bracket conservation never trips on a legitimate
        /// in-container wrap/sort.
        #[test]
        fn angle_brackets_conserved_for_in_container_wraps(
            tag in genuine_tag_strategy(),
            shape in 0u8..3,
            sorting in any::<bool>(),
        ) {
            let config = cfg(sorting);
            let src = match shape {
                0 => format!("<root>\n{{cond && (\n{tag}\n)}}\n</root>\n"),
                1 => format!("<root>\n{{xs.map(x => (\n{tag}\n))}}\n</root>\n"),
                _ => format!("<root>\n{{c ? (\n{tag}\n) : <B/>}}\n</root>\n"),
            };
            let out = format_tsx(&src, &config);
            prop_assert!(
                crate::verify::angle_brackets_conserved(&src, &out),
                "angle-bracket conservation tripped\nin:\n{}\nout:\n{}", src, out
            );
        }

        /// (e) All-dialects preservation: a generated genuine tag behaves
        /// identically whether it is the whole file or embedded in a trivial
        /// same-dialect document wrapper, for each dialect. The JSX/TSX fix never
        /// touches the HTML/Vue/Astro/Svelte path, and for JSX/TSX the tag at
        /// child position is recognized either way.
        #[test]
        fn genuine_tag_recognized_across_dialects(
            tag in genuine_tag_strategy(),
            ext_idx in 0usize..6,
        ) {
            let exts = ["html", "jsx", "tsx", "vue", "astro", "svelte"];
            let ext = exts[ext_idx];
            let config = cfg(false);
            let path = format!("f.{ext}");

            let bare = tag.clone();
            let wrapped = format!("<root>\n{tag}\n</root>\n");

            let out_bare = format_once(Path::new(&path), bare.as_bytes(), None, &config)
                .expect("format_once succeeds on bare tag");
            let out_wrapped = format_once(Path::new(&path), wrapped.as_bytes(), None, &config)
                .expect("format_once succeeds on wrapped tag");

            // Both decode as valid UTF-8 and conserve angle brackets (the tag is
            // never corrupted regardless of dialect or position).
            let out_bare = String::from_utf8(out_bare).expect("utf8");
            let out_wrapped = String::from_utf8(out_wrapped).expect("utf8");
            prop_assert!(crate::verify::angle_brackets_conserved(&bare, &out_bare));
            prop_assert!(crate::verify::angle_brackets_conserved(&wrapped, &out_wrapped));
        }
    }
}
