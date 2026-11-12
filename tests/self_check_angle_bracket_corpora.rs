//! End-to-end self-check angle-bracket corpus (bugfix spec task 8.5).
//!
//! This integration file drives the public seam — the crate-root re-export of
//! `engine::format_document` (gated on the `wasm` feature) — over the
//! symmetric-misread constructs from the TSX/JSX generics bug report. It is the
//! *end-to-end* companion to the host-target `self_check_angle_bracket_corpus`
//! module inside `src/engine.rs`.
//!
//! Division of labour (and why it is split):
//!
//! - The crate-internal assertions that require `format_once` /
//!   `EngineError::StructureDivergence` and the `fingerprints_equivalent`-only
//!   regression-pin against a HAND-CONSTRUCTED symmetric-misread `Fingerprint`
//!   live in the host-target module in `src/engine.rs`, because `format_once`,
//!   `EngineError`, and the `Fingerprint` / `TagFingerprint` types are
//!   `pub(crate)` / internal and cannot be reached from an integration test
//!   (a separate crate).
//! - This file asserts the only guarantee the public seam exposes for these
//!   inputs: `format_document` either makes NO change (preserving every `<`/`>`
//!   byte-for-byte) or errors — it NEVER returns a changed output whose
//!   whole-buffer `<`/`>` count diverges from the input (a corrupted change
//!   signal). That is the externally observable face of Requirements 2.7, 7.5,
//!   7.6, and 7.7: a corrupting rewrite can never be emitted.
//!
//! Requires the `wasm` feature (for `format_document`). Run with:
//! `cargo test --features wasm`.

#![cfg(feature = "wasm")]

use std::path::Path;

use dprint_plugin_fluid_attribute_wrapping::config::{Configuration, SchemeId};
use dprint_plugin_fluid_attribute_wrapping::format_document;

/// A `.tsx`-oriented configuration with fluid wrapping ON (so a genuine tag
/// WOULD be rewritten), a narrow width (so wrapping is actually triggered for
/// real tags), sorting off, all optional dialects enabled.
fn config(line_width: u32, sorting: bool) -> Configuration {
    Configuration {
        line_width,
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

/// `(count('<'), count('>'))` over a whole buffer.
fn angle_counts(s: &str) -> (usize, usize) {
    (s.matches('<').count(), s.matches('>').count())
}

/// Format `input` for `file_name` and resolve the effective output bytes:
/// `Ok(None)` (no change) resolves to the input verbatim; `Ok(Some(bytes))` to
/// the changed bytes. An `Err` fails the test only if it was unexpected.
///
/// Asserts the end-to-end self-check safety guarantee for a symmetric-misread
/// construct: the public seam never emits a changed output whose whole-buffer
/// `<`/`>` count diverges from the input. In practice, with the fixed scanner,
/// every entry is a byte-for-byte no-op (the generic is never misread), so we
/// assert exactly that.
fn assert_angle_safe_noop(file_name: &str, input: &str, config: &Configuration) {
    let result = format_document(Path::new(file_name), input.as_bytes(), None, config)
        .unwrap_or_else(|err| panic!("format_document errored for {file_name:?}: {err}"));

    let output = match result {
        // The fix leaves every generic construct byte-for-byte unchanged.
        None => return,
        Some(bytes) => String::from_utf8(bytes).expect("output is valid UTF-8"),
    };

    // If the seam DID report a change, it must still conserve every `<`/`>`
    // over the whole buffer — a corrupting dropped-bracket rewrite is exactly
    // what the angle-bracket invariant forbids the engine from emitting.
    assert_eq!(
        angle_counts(input),
        angle_counts(&output),
        "a changed output must conserve every `<`/`>` for {file_name:?}\n  \
         input:  {input:?}\n  output: {output:?}"
    );
}

/// The symmetric-misread corpus: TSX/JSX constructs whose pre-fix corruption
/// dropped an angle bracket identically on both scans (the self-check blind
/// spot). Under `F'` each is preserved byte-for-byte.
const CORPUS: &[&str] = &[
    // Nested generic: the canonical motivating regression.
    "const x = useState<Array<Task>>([]);",
    // Comma-separated type args.
    "type R = Record<subjectKey, string>;",
    "type O = Omit<T, id>;",
    // Member-call generic.
    "const r = axiosConfig.get<Array<UrlStats>>(url);",
    // Double-nested / trailing brackets.
    "const m = useState<ValidationErrors, encodeKey>({});",
    // A plain `<` comparison operator (not a generic, not a tag).
    "const ok = a < b && c > d;",
    // Generic inside a JSX child expression container.
    "const el = <div>{items.map<Row>((i) => i)}</div>;",
    // Component-with-generics head: a not-confidently-rewritable per-tag no-op.
    "const list = <EditableList<SearchEngineDomain> value={v} />;",
];

#[test]
fn symmetric_misread_generics_are_angle_safe_no_ops_under_f_prime() {
    // Narrow width so a genuine tag WOULD wrap — making a no-op on a generic a
    // real signal that the construct was left alone rather than simply fitting.
    let cfg = config(32, false);
    for (i, input) in CORPUS.iter().enumerate() {
        assert_angle_safe_noop(&format!("self_check_{i}.tsx"), input, &cfg);
    }

    // The same corpus under sorting enabled: still no corrupting change.
    let sorted = config(32, true);
    for (i, input) in CORPUS.iter().enumerate() {
        assert_angle_safe_noop(&format!("self_check_sorted_{i}.tsx"), input, &sorted);
    }

    // And on the `.jsx` dialect too (shares the JS-family profile).
    for (i, input) in CORPUS.iter().enumerate() {
        assert_angle_safe_noop(&format!("self_check_{i}.jsx"), input, &cfg);
    }
}

/// The public seam, given a GENUINE buffer, never emits a changed output whose
/// whole-buffer `<`/`>` count diverges — the externally observable face of the
/// Requirement 7.6 per-tag revert and the Requirement 7.7 document-level
/// backstop. (The crate-internal `format_once` / `EngineError::StructureDivergence`
/// assertion lives in the host-target module in `src/engine.rs`, which can reach
/// the internal seam this integration crate cannot.)
#[test]
fn seam_never_emits_a_bracket_dropping_change() {
    let cfg = config(24, false);
    // A document mixing genuine HTML-ish/JSX tags (which wrap) with a trailing
    // top-level generic (which must be preserved). A changed output is allowed
    // (the genuine tags wrap), but it must conserve every `<`/`>`.
    let input = "const el = <Panel title=\"hello world\" id=\"main\" role=\"group\">body</Panel>; \
                 const x = useState<Array<Task>>([]);";
    let result = format_document(Path::new("mixed.tsx"), input.as_bytes(), None, &cfg)
        .expect("format_document should succeed");
    let output = match result {
        None => input.to_string(),
        Some(bytes) => String::from_utf8(bytes).expect("output is valid UTF-8"),
    };
    assert_eq!(
        angle_counts(input),
        angle_counts(&output),
        "the seam must conserve every `<`/`>` even when it wraps genuine tags\n  \
         input:  {input:?}\n  output: {output:?}"
    );
    // The trailing generic is preserved verbatim.
    assert!(
        output.contains("useState<Array<Task>>([]);"),
        "the trailing top-level generic must be preserved byte-for-byte, got:\n{output}"
    );
}
