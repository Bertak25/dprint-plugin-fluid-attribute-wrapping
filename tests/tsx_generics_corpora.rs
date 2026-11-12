//! TSX/JSX generics regression corpora (task 8.2).
//!
//! These fixtures lock in the fix for the "TSX/JSX generics misparsed as JSX
//! tags" bug. Before the fix, the scanner would misread TypeScript generic type
//! arguments (`useState<Array<Task>>(…)`, `Record<K, V>`, `Omit<T, id>`, …) and
//! `<` comparison operators as JSX start tags, reflow the "attributes", and drop
//! angle brackets — corrupting valid TypeScript.
//!
//! Every fixture here is valid TypeScript in which no `<` introduces a JSX
//! element. The plugin MUST therefore leave each one BYTE-FOR-BYTE unchanged and
//! MUST preserve the markup structure under the Requirement 7.5 equivalence
//! relation (`structural_fingerprint` / `fingerprints_equivalent`) — exactly the
//! same public seam and equivalence machinery the sibling
//! `tests/structure_equivalence_corpora.rs` uses.
//!
//! This file is deliberately structured as several small, single-purpose test
//! functions so later tasks (8.3 mixed statement+JSX cascade, 8.4 genuine-tag
//! preservation) can add their own `#[test]` functions here without disturbing
//! the generics fixtures below.
//!
//! Requires the `wasm` feature (for `format_document`). Run with:
//! `cargo test --features wasm`.

#![cfg(feature = "wasm")]

use std::path::Path;

use dprint_plugin_fluid_attribute_wrapping::config::{Configuration, SchemeId};
use dprint_plugin_fluid_attribute_wrapping::dialect::Dialect;
use dprint_plugin_fluid_attribute_wrapping::format_document;
use dprint_plugin_fluid_attribute_wrapping::verify::{
    fingerprints_equivalent, structural_fingerprint,
};

/// Build a test configuration. `line_width` is tunable so fixtures can (fail to)
/// force wrapping; all optional dialects are enabled so every extension
/// resolves. Generics fixtures use a deliberately narrow width to prove the
/// plugin does NOT wrap a misread generic even under pressure.
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

/// Format `input` for the file at `file_name`, assert the output is
/// BYTE-FOR-BYTE identical to the input (treating `Ok(None)` — the plugin's
/// "no change" signal — as "output == input"), and additionally assert the
/// markup structure is equivalent to the input under the plugin's OWN
/// Requirement 7.5 fingerprint relation (valid-TS structure preserved).
///
/// Panics (fails the test) if formatting errors, if any byte changes, or if the
/// structures diverge.
fn assert_byte_for_byte_preserved(file_name: &str, input: &str, config: &Configuration) {
    let result = format_document(Path::new(file_name), input.as_bytes(), None, config)
        .unwrap_or_else(|err| panic!("format_document errored for {file_name:?}: {err}"));

    // `None` is the plugin's no-change signal: output == input verbatim. Any
    // `Some(bytes)` MUST equal the input byte-for-byte for a preservation
    // fixture — a changed byte here is the corruption this corpus guards.
    match result {
        None => {}
        Some(bytes) => {
            let output = String::from_utf8(bytes).expect("output is valid UTF-8");
            assert_eq!(
                output, input,
                "expected byte-for-byte preservation for {file_name:?}\n  input:  {input:?}\n  output: {output:?}",
            );
        }
    }

    // Belt-and-braces: even granting the byte-equality above, assert the markup
    // structure is equivalent under the plugin's own fingerprint relation, so
    // this corpus also stands as a valid-TS structure check (Requirement 7.5).
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .expect("fixture file names carry a supported extension");
    let dialect = Dialect::from_extension(ext, config)
        .unwrap_or_else(|| panic!("extension {ext:?} must resolve to a supported dialect"));
    let profile = dialect.scanner_profile();
    let indent_width = usize::from(config.indent_width);

    let input_fp = structural_fingerprint(input, &profile, indent_width)
        .expect("input fingerprint should scan");
    let output_fp = structural_fingerprint(input, &profile, indent_width)
        .expect("output fingerprint should scan");

    assert!(
        fingerprints_equivalent(&input_fp, &output_fp, config.attribute_sorting),
        "structure diverged for {file_name:?}\n  input: {input:?}",
    );
}

// ---------------------------------------------------------------------------
// Statement-position generics: `const x = Ident<…>(…)`.
//
// These are the headline failing constructs from the bug report. Each `<`
// follows an identifier (value position), so none introduces a JSX element —
// the plugin must leave every angle bracket in place.
// ---------------------------------------------------------------------------

#[test]
fn statement_position_generics_are_preserved() {
    // Narrow width to prove a misread generic is NOT wrapped even under pressure.
    let cfg = config(24, true, false);
    let corpus = [
        // Nested single-arg generic in a call — the canonical failure.
        "const [tasks, setTasks] = useState<Array<Task>>([]);",
        // Two-arg generic with a comma (previously reflowed as attributes).
        "const map: Record<subjectKey, string> = {};",
        // Two single-letter type params.
        "const map2: Record<K, V> = {};",
        // Utility type with a bare-ident second arg.
        "type A = Omit<T, id>;",
        // Utility type with a longer first arg.
        "type B = Omit<SearchEngineDomain, id>;",
        // Comma-separated args with a trailing `>>` and an empty-object call.
        "const [errs] = useState<ValidationErrors, encodeKey>>({});",
        // `null` as a first type arg plus a DOM type.
        "const r = useState<null, HTMLElement>(null);",
    ];
    for (i, input) in corpus.iter().enumerate() {
        assert_byte_for_byte_preserved(&format!("stmt_generic_{i}.tsx"), input, &cfg);
    }
}

// ---------------------------------------------------------------------------
// Member-call generics: `obj.method<…>(…)`.
// ---------------------------------------------------------------------------

#[test]
fn member_call_generics_are_preserved() {
    let cfg = config(24, true, false);
    let corpus = [
        // The member-call form from the bug report.
        "const stats = axiosConfig.get<Array<UrlStats>>(url);",
        // A deeper member chain with a generic call.
        "const res = client.api.fetch<Response<Data>>(endpoint);",
    ];
    for (i, input) in corpus.iter().enumerate() {
        assert_byte_for_byte_preserved(&format!("member_generic_{i}.tsx"), input, &cfg);
    }
}

// ---------------------------------------------------------------------------
// `<` as a comparison / shift operator (never a tag).
// ---------------------------------------------------------------------------

#[test]
fn comparison_operators_are_preserved() {
    let cfg = config(24, true, false);
    let corpus = [
        // Plain comparison — `b` is a value, not a tag name.
        "const ok = a < b;",
        // Comparison inside a call argument.
        "const n = Math.max(a < b ? 1 : 2, 0);",
    ];
    for (i, input) in corpus.iter().enumerate() {
        assert_byte_for_byte_preserved(&format!("cmp_{i}.tsx"), input, &cfg);
    }
}

// ---------------------------------------------------------------------------
// Generics inside a JSX child `{ … }` expression container.
//
// The child-container contents must be skipped wholesale: a generic or `<`
// operator living inside `{ … }` must never be scanned for tags.
// ---------------------------------------------------------------------------

#[test]
fn generics_inside_child_containers_are_preserved() {
    let cfg = config(40, true, false);
    let corpus = [
        // A generic call expression evaluated inside a child container.
        "const view = <div>{useState<Array<Task>>([])}</div>;",
        // A `<` comparison inside a child container.
        "const view2 = <section>{a < b ? x : y}</section>;",
    ];
    for (i, input) in corpus.iter().enumerate() {
        assert_byte_for_byte_preserved(&format!("child_generic_{i}.tsx"), input, &cfg);
    }
}

// ---------------------------------------------------------------------------
// Component-with-generics head: `<EditableList<SearchEngineDomain>>`.
//
// An in-attribute-region `<` makes the tag not-confidently-rewritable, so the
// plugin leaves it byte-for-byte unchanged as a per-tag no-op (Requirement 4.9).
// ---------------------------------------------------------------------------

#[test]
fn component_with_generics_head_is_preserved() {
    let cfg = config(24, true, false);
    let corpus = [
        // The component-head form from the bug report.
        "const el = <EditableList<SearchEngineDomain>>{rows}</EditableList>;",
        // A single type-parameter component head.
        "const el2 = <List<Item>>{rows}</List>;",
    ];
    for (i, input) in corpus.iter().enumerate() {
        assert_byte_for_byte_preserved(&format!("component_generic_{i}.tsx"), input, &cfg);
    }
}

// ---------------------------------------------------------------------------
// Mixed statement + JSX (drift / cascade) corpus — task 8.3.
//
// These fixtures interleave fragments (`<>…</>`), self-closing tags (`<br/>`),
// a genuine component with many attributes (which MUST wrap at a narrow width),
// and a trailing TOP-LEVEL TypeScript generic such as
// `const [t] = useState<Array<Task>>([]);`.
//
// This specifically exercises the drift-proof expression-position gate
// (Requirement 2.5): after arbitrary JSX — fragments, self-closing tags,
// nested children, genuine components — the running `last_significant`
// classifier must NOT have drifted, so the trailing generic (which is in value
// position) is still correctly rejected as a tag and every `<`/`>` of it
// survives verbatim. Simultaneously the genuine components must keep wrapping
// exactly as before (Requirement 2.6) — the fix must not disable real JSX.
//
// Because wrapping intentionally CHANGES bytes, a byte-for-byte helper is the
// wrong tool here. Instead this corpus asserts three things per fixture:
//   1. the output is structurally equivalent to the input under the plugin's
//      own Requirement 7.5 fingerprint relation (no markup structure lost);
//   2. the trailing top-level generic is preserved VERBATIM — its exact
//      substring (every `<` and `>`) still appears in the output;
//   3. the genuine component actually WRAPPED — the output differs from the
//      input and the component's start tag now spans multiple lines (its name
//      is immediately followed by a line break rather than a space).
// ---------------------------------------------------------------------------

/// Resolve the effective output bytes for a fixture (`Ok(None)` == input
/// verbatim), decode as UTF-8, and return the output string. Fails the test on
/// a formatting error.
fn format_to_string(file_name: &str, input: &str, config: &Configuration) -> String {
    let result = format_document(Path::new(file_name), input.as_bytes(), None, config)
        .unwrap_or_else(|err| panic!("format_document errored for {file_name:?}: {err}"));
    let output_bytes = match result {
        Some(bytes) => bytes,
        None => input.as_bytes().to_vec(),
    };
    String::from_utf8(output_bytes).expect("output is valid UTF-8")
}

/// Assert the input is structurally equivalent to the output under the plugin's
/// OWN Requirement 7.5 fingerprint relation. Unlike the byte-for-byte helper,
/// this tolerates the whitespace a wrapping rewrite introduces, which is exactly
/// what we want for fixtures where a genuine component legitimately wraps.
fn assert_structure_equivalent(file_name: &str, input: &str, output: &str, config: &Configuration) {
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .expect("fixture file names carry a supported extension");
    let dialect = Dialect::from_extension(ext, config)
        .unwrap_or_else(|| panic!("extension {ext:?} must resolve to a supported dialect"));
    let profile = dialect.scanner_profile();
    let indent_width = usize::from(config.indent_width);

    let input_fp = structural_fingerprint(input, &profile, indent_width)
        .expect("input fingerprint should scan");
    let output_fp = structural_fingerprint(output, &profile, indent_width)
        .expect("output fingerprint should scan");

    assert!(
        fingerprints_equivalent(&input_fp, &output_fp, config.attribute_sorting),
        "structure diverged for {file_name:?}\n  input:  {input:?}\n  output: {output:?}",
    );
}

/// Assert that `generic` — the trailing top-level TypeScript generic — survives
/// byte-for-byte in `output`. This is the drift-proof gate's payoff: the generic
/// must not have been misread as a JSX tag and had its angle brackets dropped
/// (Requirement 2.5).
fn assert_generic_preserved(file_name: &str, generic: &str, output: &str) {
    assert!(
        output.contains(generic),
        "trailing generic {generic:?} was not preserved verbatim for {file_name:?}\n  output: {output:?}",
    );
}

/// Assert that the genuine component named `component` actually wrapped: the
/// output must differ from the input (a change occurred) and the component's
/// start tag must now span multiple lines, i.e. the byte span from `<Name` up
/// to the tag terminator contains at least one line break (a continuation line
/// the renderer emitted for an overflowing attribute list).
fn assert_component_wrapped(file_name: &str, component: &str, input: &str, output: &str) {
    assert_ne!(
        output, input,
        "expected the genuine component {component:?} to wrap (output should differ from input) for {file_name:?}",
    );
    // Locate the component's start tag and confirm it spans multiple lines. The
    // renderer keeps `<Name` plus the first attribute on the opening line and
    // breaks before each subsequent attribute, so a wrapped tag has a newline
    // somewhere between `<Name` and the terminating `>`.
    let open = format!("<{component}");
    let start = output
        .find(&open)
        .unwrap_or_else(|| panic!("component {component:?} missing from output for {file_name:?}\n  output: {output:?}"));
    let after = &output[start + open.len()..];
    let terminator = after
        .find('>')
        .unwrap_or_else(|| panic!("unterminated start tag for {component:?} in {file_name:?}\n  output: {output:?}"));
    let tag_span = &after[..terminator];
    assert!(
        tag_span.contains('\n'),
        "expected component {component:?} to wrap onto multiple lines for {file_name:?}\n  output: {output:?}",
    );
}

#[test]
fn mixed_statement_and_jsx_cascade_preserves_trailing_generic_and_wraps_components() {
    // Narrow width so the genuine component start tags are forced to wrap, while
    // the trailing top-level generics (value position) must stay intact.
    let cfg = config(28, true, false);

    // Each fixture: (file name, source, the component that must wrap, the
    // trailing top-level generic that must survive verbatim).
    struct Fixture {
        name: &'static str,
        src: &'static str,
        component: &'static str,
        generic: &'static str,
    }

    let fixtures = [
        // 1. Fragment wrapping a self-closing tag and a genuine component, then
        //    a trailing top-level generic. The fragment `<>`/`</>`, the
        //    self-closing `<br/>`, and the component must not cause the gate to
        //    drift into misreading the trailing `useState<Array<Task>>`.
        Fixture {
            name: "cascade_0.tsx",
            src: "const view = (\n  <>\n    <br/>\n    <PageLoading loading={false} fullScreen={true} label=\"Loading now\" />\n  </>\n);\nconst [tasks, setTasks] = useState<Array<Task>>([]);\n",
            component: "PageLoading",
            generic: "useState<Array<Task>>",
        },
        // 2. Interleave a nested child element, a fragment, and a genuine
        //    component with many attributes, then a trailing multi-arg generic
        //    (comma + `>>`) that previously reflowed as attributes.
        Fixture {
            name: "cascade_1.tsx",
            src: "function App() {\n  return (\n    <ul>\n      <li>one</li>\n      <>\n        <Avatar src=\"/a.png\" alt=\"User avatar\" size={48} rounded={true} />\n      </>\n    </ul>\n  );\n}\nconst m = useState<Record<subjectKey, string>>({});\n",
            component: "Avatar",
            generic: "useState<Record<subjectKey, string>>",
        },
        // 3. Several self-closing tags and a fragment before a genuine component,
        //    then a trailing member-call generic (`axiosConfig.get<Array<…>>`).
        Fixture {
            name: "cascade_2.tsx",
            src: "const header = (\n  <>\n    <hr/>\n    <br/>\n    <NavBar title=\"Dashboard\" sticky={true} theme=\"dark\" onToggle={toggle} />\n  </>\n);\nconst stats = axiosConfig.get<Array<UrlStats>>(url);\n",
            component: "NavBar",
            generic: "axiosConfig.get<Array<UrlStats>>",
        },
    ];

    for fx in &fixtures {
        let output = format_to_string(fx.name, fx.src, &cfg);
        // (1) No markup structure was lost in the rewrite.
        assert_structure_equivalent(fx.name, fx.src, &output, &cfg);
        // (2) The trailing top-level generic survived verbatim (drift-proof gate).
        assert_generic_preserved(fx.name, fx.generic, &output);
        // (3) The genuine component still wrapped across lines.
        assert_component_wrapped(fx.name, fx.component, fx.src, &output);
    }
}

// ---------------------------------------------------------------------------
// Genuine-tag preservation corpus — task 8.4.
//
// The sibling corpora above prove the fix does not touch TypeScript generics
// (the `C` / buggy inputs). This corpus proves the complementary half of the
// bugfix's preservation property: for GENUINE JSX/TSX markup (the `¬C` inputs)
// the fixed plugin `F'` must behave EXACTLY like the pre-fix plugin `F` — same
// recognition, same fluid wrapping, same canonicalization, same sorting. The
// fix must not disable real JSX as collateral damage (Requirements 2.6, 3.2,
// 3.3, 3.4).
//
// The concrete expected outputs below are the SAME observed `F` outputs the
// task-2 `preservation_baseline` module in `src/engine.rs` pinned on the
// UNFIXED code (e.g. `<Item/>` → `<Item />`, greedy fluid wrapping onto
// continuation lines indented one unit, standard-scheme attribute order). This
// corpus re-asserts them through the PUBLIC `format_document` seam so "fixed
// output equals pre-fix output" is checked end-to-end.
//
// Every fixture ALSO asserts idempotence: formatting the output a second time
// via `format_document` produces byte-identical output (format once → output;
// format output again → same bytes), treating `Ok(None)` as "output == input".
// ---------------------------------------------------------------------------

/// Assert that `format_document` turns `input` into exactly `expected` for the
/// file at `file_name`, AND that the result is idempotent: feeding `expected`
/// (the output) back through `format_document` yields byte-identical bytes
/// (`Ok(None)`, the no-change signal, counts as "output == input == expected").
///
/// This is the genuine-tag analogue of the task-2 baseline's `assert_baseline`:
/// it pins the observed `F` output AND the byte-for-byte fixed point, but drives
/// the public `format_document` seam (with its own idempotence self-check)
/// rather than the crate-internal `format_once`.
fn assert_formats_to(file_name: &str, input: &str, expected: &str, config: &Configuration) {
    // Pass 1: input -> expected.
    let first = format_to_string(file_name, input, config);
    assert_eq!(
        first, expected,
        "fixed output must equal the pre-fix baseline for {file_name:?}\n  input:    {input:?}\n  expected: {expected:?}\n  actual:   {first:?}",
    );

    // Pass 2 (idempotence): expected -> expected, byte-for-byte.
    let second = format_to_string(file_name, &first, config);
    assert_eq!(
        second, first,
        "idempotence failed: a second format_document pass changed the output for {file_name:?}\n  first:  {first:?}\n  second: {second:?}",
    );
}

#[test]
fn genuine_component_wraps_fluidly_and_is_idempotent() {
    // A legitimate `<PageLoading …>` component with many attributes wraps
    // fluidly: as many attributes as fit on each line up to the width, then
    // continuation lines indented one unit (width 2). This is the exact tag the
    // bug report shows as collateral damage — here, with no surrounding
    // generics, it is `¬C` and must keep wrapping exactly as the pre-fix plugin
    // did. (Requirements 2.6, 3.2.)
    let cfg = config(40, true, false);
    let input = "<PageLoading loading={false} spinning={true} size=\"large\" tip=\"please wait\" delay={200} />";
    let expected = "<PageLoading loading={false}\n  spinning={true} size=\"large\"\n  tip=\"please wait\" delay={200} />";
    assert_formats_to("genuine_pageloading.tsx", input, expected, &cfg);
}

#[test]
fn expression_introduced_jsx_is_recognized_and_preserved() {
    // Expression-position introducers (`return`, `=>`, `yield`, `await`) each
    // put the following `<` in start-tag position, so the genuine element is
    // recognized. Self-closing tags are canonicalized to `<Name />` (a space
    // before the slash) — the observed pre-fix behavior. Width 80 so nothing
    // wraps; recognition + canonicalization are what matter. (Requirement 3.3.)
    let cfg = config(80, true, false);

    // `return <div …>` — fits within width, recognized, byte-for-byte no-op.
    assert_formats_to(
        "expr_return.tsx",
        "const x = () => { return <div a b c>hi</div>; };",
        "const x = () => { return <div a b c>hi</div>; };",
        &cfg,
    );

    // `x => <Item/>` — arrow introduces expression position; `<Item/>`
    // canonicalizes to `<Item />`.
    assert_formats_to(
        "expr_arrow.tsx",
        "const f = x => <Item/>;",
        "const f = x => <Item />;",
        &cfg,
    );

    // `yield <x/>` — `yield` introduces expression position; canonicalized.
    assert_formats_to(
        "expr_yield.tsx",
        "function* g() { yield <x/>; }",
        "function* g() { yield <x />; }",
        &cfg,
    );

    // `await <y/>` — `await` introduces expression position; canonicalized.
    assert_formats_to(
        "expr_await.tsx",
        "async function h() { await <y/>; }",
        "async function h() { await <y />; }",
        &cfg,
    );
}

#[test]
fn fragments_self_closing_and_nested_children_are_preserved() {
    // Fragments, self-closing elements, and nested child elements must all be
    // recognized/canonicalized exactly as the pre-fix plugin did. (Req 3.2.)
    let cfg = config(80, true, false);

    // A bare fragment `<>…</>` has no name-start after `<`, so no tag is
    // produced from the fragment markers — byte-for-byte no-op.
    assert_formats_to("fragment.tsx", "<>text</>", "<>text</>", &cfg);

    // A lone self-closing `<br/>` canonicalizes to `<br />`.
    assert_formats_to("self_closing.tsx", "<br/>", "<br />", &cfg);

    // Nested children: both the parent `<ul>` and the child-position `<li>`
    // (the `<` follows the parent tag's closing `>`, i.e. `TagClose`) are
    // recognized; both already fit, so this is a no-op.
    assert_formats_to(
        "nested_children.tsx",
        "<ul><li>a</li></ul>",
        "<ul><li>a</li></ul>",
        &cfg,
    );

    // A fragment wrapping a self-closing tag and a nested child, all genuine
    // JSX, left structurally intact (self-closing canonicalized to `<br />`).
    assert_formats_to(
        "fragment_nested.tsx",
        "<><br/><ul><li>x</li></ul></>",
        "<><br /><ul><li>x</li></ul></>",
        &cfg,
    );
}

#[test]
fn genuine_tag_sorts_with_standard_scheme_and_is_idempotent() {
    // With sorting on (wrapping off) the standard scheme reorders a genuine
    // `<input …>`: `id` leads, then `class`, then `type`, with the generic
    // `data-x` trailing — the observed pre-fix ordering with its
    // behavior-preservation guards. The fix must keep it identical. (Req 3.4.)
    let cfg = config(200, false, true);
    assert_formats_to(
        "genuine_sort.tsx",
        "<input data-x=\"1\" id=\"main\" class=\"c\" type=\"text\" />",
        "<input id=\"main\" class=\"c\" type=\"text\" data-x=\"1\" />",
        &cfg,
    );
}

// ---------------------------------------------------------------------------
// Component-with-nested-generics head PLUS trailing attributes — task 10.2.
//
// This locks in the already-correct no-op behavior for the real-world shape a
// user hit: a component head carrying TypeScript type arguments
// (`<Select<Array<number>> …>`) that is ALSO followed by several real
// attributes, with the closing `>` on a LATER line and a body plus a matching
// `</Select>` close tag. Structurally this is the Change 3 case: the in-
// attribute-region `<` (the one right after the `Select` name) makes the whole
// start tag not-confidently-rewritable, so the plugin leaves it byte-for-byte
// unchanged (a per-tag no-op, Requirement 4.9). The attribute-less
// `<EditableList<SearchEngineDomain>>` is covered by the scanner's
// `component_generics_head_tests`; this exercises the end-to-end public
// `format_document` seam for the "generics head PLUS trailing attributes,
// multi-line" shape.
//
// Each assertion is belt-and-braces:
//   1. the whole fixture is a byte-for-byte no-op (`Ok(None)` or `Some(bytes)`
//      equal to the input);
//   2. every `<`/`>` is preserved (whole-buffer angle-bracket counts equal —
//      the Change 4 conservation invariant, mirrored here as a local helper);
//   3. the markup structure is equivalent under the plugin's own Requirement
//      7.5 relation (`structural_fingerprint` / `fingerprints_equivalent`);
//   4. the result is idempotent — a second `format_document` pass is
//      byte-identical.
// ---------------------------------------------------------------------------

/// `(count('<'), count('>'))` over a whole buffer — the same whole-buffer
/// angle-bracket tally the engine's `angle_brackets_conserved` invariant uses
/// (Requirement 2.7 / Change 4), reused here to assert no `<`/`>` is dropped.
fn angle_counts(s: &str) -> (usize, usize) {
    (s.matches('<').count(), s.matches('>').count())
}

/// End-to-end no-op assertion for the generics-head-plus-attributes shape:
///   - `format_document` leaves `input` byte-for-byte unchanged;
///   - every `<`/`>` survives (angle-bracket counts equal);
///   - the markup structure is equivalent under Requirement 7.5;
///   - a second `format_document` pass is byte-identical (idempotence).
fn assert_generics_head_noop(file_name: &str, input: &str, config: &Configuration) {
    // (1) Byte-for-byte no-op, plus structure equivalence (Requirement 7.5).
    assert_byte_for_byte_preserved(file_name, input, config);

    // (2) Every `<`/`>` is preserved over the whole buffer. If the seam did
    //     report a change (it must not for this shape), it would still have to
    //     conserve brackets; asserting against the known-no-op output makes the
    //     conservation explicit.
    let output = format_to_string(file_name, input, config);
    assert_eq!(
        angle_counts(input),
        angle_counts(&output),
        "every `<`/`>` must be preserved for {file_name:?}\n  input:  {input:?}\n  output: {output:?}",
    );

    // (4) Idempotence: a second pass over the output is byte-identical.
    let second = format_to_string(file_name, &output, config);
    assert_eq!(
        second, output,
        "idempotence failed: a second format_document pass changed the output for {file_name:?}\n  first:  {output:?}\n  second: {second:?}",
    );
}

#[test]
fn component_generics_head_with_trailing_attributes_is_multiline_noop() {
    // A narrow width would normally force a genuine component to wrap, but this
    // tag is not-confidently-rewritable (the `<` after `Select`), so it must be
    // left byte-for-byte unchanged regardless of width.
    let cfg = config(28, true, false);

    // The real-world shape from the bug report: `<Select<Array<number>> …>`
    // with several attributes, the closing `>` on a LATER line, a body, and a
    // matching `</Select>` close tag.
    let input = concat!(
        "const view = (\n",
        "  <Select<Array<number>>\n",
        "    labelId=\"task-select-label\"\n",
        "    multiple\n",
        "    value={x}\n",
        "    onChange={on}\n",
        "    className=\"w-full\"\n",
        "  >\n",
        "    {items}\n",
        "  </Select>\n",
        ");\n",
    );
    assert_generics_head_noop("select_generics_head_multiline.tsx", input, &cfg);

    // A single-type-parameter variant on one head line, still multi-line overall.
    let input2 = concat!(
        "function Picker() {\n",
        "  return (\n",
        "    <Dropdown<Item>\n",
        "      id=\"picker\"\n",
        "      disabled\n",
        "      value={current}\n",
        "      onChange={handle}\n",
        "    >\n",
        "      {options}\n",
        "    </Dropdown>\n",
        "  );\n",
        "}\n",
    );
    assert_generics_head_noop("dropdown_generics_head_multiline.tsx", input2, &cfg);
}
