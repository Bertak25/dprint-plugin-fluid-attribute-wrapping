//! Property-based tests for the "JSX nested container start tag not wrapped"
//! bugfix (jsx-nested-container-start-tag-not-wrapped spec task 6).
//!
//! These broaden the fixed-corpus checks in
//! `tests/jsx_attribute_value_containers_corpora.rs` into sampled-domain
//! properties over the recursive two-level rewrite. An enclosing JSX/TSX element
//! Start_Tag can carry an attribute whose **value** is a braced expression
//! container holding another genuine JSX element (`primary={ <a …>…</a> }`,
//! `secondary={ <Chip … /> }`). Correct behavior — implemented by the recursive
//! children-tree fix of task 4 — is **two-level formatting**: the enclosing tag's
//! own top-level attributes are fluid-wrapped (and, where enabled, sorted) AND
//! every genuine JSX element nested inside an attribute value is *independently*
//! fluid-wrapped/sorted on its own rendered geometry, while all non-element bytes
//! (generics, `<`-operators, template literals, calls, arrows, ternaries,
//! spreads, regex literals, text children) are preserved byte-for-byte.
//!
//! The properties implemented here, keyed to task 6:
//!
//! - (a) **Both-levels / in-out equivalence** (Bug 2.1, 2.5, 2.8): a generated
//!   element placed as an enclosing tag's attribute value wraps to the identical
//!   attribute-line grouping as the same element outside any container (modulo
//!   base indent), AND the enclosing tag wraps identically to the same-attribute
//!   tag whose element-bearing value is replaced by a plain value of equal width.
//! - (b) **Selective rewrite** (Bug 2.1, 3.1, 3.2, 3.9): for inputs mixing
//!   generics, `<` operators, ordinary expressions, template literals, arrows,
//!   ternaries, spreads, regex literals, and a genuine element inside attribute
//!   values, ONLY the genuine element is rewritten and every non-element byte is
//!   byte-for-byte preserved.
//! - (c) **Idempotence over both levels** (Bug 3.7; Requirement 8, 8.5): inputs
//!   with in-attribute-value JSX at several depths reach a byte-for-byte fixed
//!   point under `format_document`. Idempotence relies on the shared
//!   `display_width` column model being used at BOTH the enclosing and the child
//!   level in both self-check passes (Requirement 8.5), which a geometry taken
//!   from the source rather than the rendered position would violate — property
//!   (d) is the dedicated fixture for that risk.
//! - (d) **Dedicated two-level re-wrap idempotence fixture** (Finding 4; Bug
//!   3.7): the enclosing tag has enough attributes to WRAP AND carries an
//!   element-bearing attribute value whose inner element ITSELF overflows the
//!   line width, so both levels re-wrap in the same pass and the inner element is
//!   spliced at a MOVED continuation column — exactly the case that would
//!   oscillate if child geometry were read from the source. LF/CRLF and
//!   sorting-enabled variants are included.
//! - (e) **Structure equivalence + angle-bracket conservation over children**
//!   (Bug 3.8; Requirement 7.5, 7.8): input/output markup structure is identical
//!   under the Requirement 7.5 fingerprint relation (extended over `children`),
//!   and angle-bracket conservation never trips on a legitimate two-level
//!   wrap/sort.
//! - (f) **Arbitrary depth** (Bug 2.2): a
//!   container-within-a-container-within-an-attribute-value fixture
//!   (`a={ <X b={ <Y c={ <Z …/> }/> }/> }`) produces a correctly nested
//!   `children` tree with one node per genuine element and correct,
//!   non-overlapping relative spans, each wrapped by its own recursive
//!   `rewrite_tag`.
//!
//! Properties (a)–(e) drive the PUBLIC `format_document` seam (like
//! `tests/jsx_in_child_containers_corpora.rs`), so the whole file is
//! `wasm`-gated. Property (f) inspects the scanner's `children` tree through the
//! public `scan_start_tags` API. Run with `cargo test --features wasm`.

#![cfg(feature = "wasm")]

use std::path::Path;

use dprint_plugin_fluid_attribute_wrapping::config::{Configuration, SchemeId};
use dprint_plugin_fluid_attribute_wrapping::dialect::Dialect;
use dprint_plugin_fluid_attribute_wrapping::format_document;
use dprint_plugin_fluid_attribute_wrapping::model::{NestedChild, ScannedTag};
use dprint_plugin_fluid_attribute_wrapping::scanner::scan_start_tags;
use dprint_plugin_fluid_attribute_wrapping::verify::{
    angle_brackets_conserved, fingerprints_equivalent, structural_fingerprint,
};

use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Shared helpers (mirroring the corpus file so this suite is self-contained).
// ---------------------------------------------------------------------------

/// Build a test configuration. `line_width` is tunable; all optional dialects
/// are enabled so every extension resolves.
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

/// Resolve the effective output bytes for a fixture (`Ok(None)` == input
/// verbatim), decode as UTF-8, and return the output string.
fn format_to_string(file_name: &str, input: &str, config: &Configuration) -> String {
    let result = format_document(Path::new(file_name), input.as_bytes(), None, config)
        .unwrap_or_else(|err| panic!("format_document errored for {file_name:?}: {err}"));
    let output_bytes = match result {
        Some(bytes) => bytes,
        None => input.as_bytes().to_vec(),
    };
    String::from_utf8(output_bytes).expect("output is valid UTF-8")
}

/// Byte offset of the start tag whose element name is EXACTLY `component`,
/// requiring a non-identifier byte (whitespace, `/`, `>`) right after the name
/// so a prefix like `<Wr` does not match a longer `<Wrap`. Returns `None` if no
/// such tag exists.
fn find_start_tag(output: &str, component: &str) -> Option<usize> {
    let open = format!("<{component}");
    let mut from = 0usize;
    while let Some(rel) = output[from..].find(&open) {
        let at = from + rel;
        let after = output.as_bytes().get(at + open.len());
        match after {
            // Name boundary: space/newline/tab, self-close `/`, or terminator `>`.
            Some(b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>') | None => return Some(at),
            _ => from = at + open.len(),
        }
    }
    None
}

/// Per-line grouping of attribute tokens for the start tag named `component`,
/// from `<Name` through the first following terminator `>` — whitespace
/// insensitive within a line, so base indentation does not affect the
/// comparison. Used to compare an INNER element's wrap SHAPE (the inner element
/// has no nested `>` of its own in these fixtures, so the first `>` terminates
/// its start tag). Matches the element name exactly (not as a prefix).
fn attribute_line_groups(output: &str, component: &str) -> Vec<Vec<String>> {
    let start = find_start_tag(output, component)
        .unwrap_or_else(|| panic!("output must contain a <{component}> start tag:\n{output}"));
    let rest = &output[start..];
    let end = rest.find('>').expect("tag terminator") + 1;
    rest[..end]
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|toks| !toks.is_empty())
        .collect()
}

/// The ordered sequence of top-level attribute NAMES that begin each line of
/// the enclosing start tag `component`, over the whole output. Captures the
/// enclosing greedy-wrap shape (which attribute each continuation line starts
/// with) without being confused by `>`/`/>` inside an attribute value: it keys
/// off the known top-level attribute names rather than tokenizing the whole
/// span.
fn enclosing_line_leading_attrs(output: &str, component: &str, attr_names: &[&str]) -> Vec<String> {
    let open = format!("<{component}");
    output
        .lines()
        .filter_map(|line| {
            let body = line.trim_start().strip_prefix(&open).unwrap_or_else(|| line.trim_start());
            let body = body.trim_start();
            attr_names
                .iter()
                .find(|name| {
                    body.strip_prefix(**name)
                        .is_some_and(|after| after.starts_with(['=', ' ', '\t']) || after.is_empty())
                })
                .map(|name| (*name).to_string())
        })
        .collect()
}

/// Assert `format_document` is idempotent on `input`: pass 2 over the pass-1
/// output is byte-for-byte identical. Returns the pass-1 output.
fn assert_idempotent(
    file_name: &str,
    input: &str,
    config: &Configuration,
) -> Result<String, TestCaseError> {
    let first = format_to_string(file_name, input, config);
    let second = format_to_string(file_name, &first, config);
    prop_assert_eq!(
        &second,
        &first,
        "idempotence failed for {:?}\n  first:  {:?}\n  second: {:?}",
        file_name,
        first,
        second
    );
    Ok(first)
}

/// Assert input↔output structure equivalence under the plugin's Requirement 7.5
/// fingerprint relation (extended over `children`), AND angle-bracket
/// conservation over the whole buffer.
fn assert_structure_safe(
    file_name: &str,
    input: &str,
    output: &str,
    config: &Configuration,
) -> Result<(), TestCaseError> {
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

    prop_assert!(
        fingerprints_equivalent(&input_fp, &output_fp, config.attribute_sorting),
        "structure diverged for {:?}\n  input:  {:?}\n  output: {:?}",
        file_name,
        input,
        output
    );
    prop_assert!(
        angle_brackets_conserved(input, output),
        "no `<`/`>` may be added or dropped for {:?}\n  input:  {:?}\n  output: {:?}",
        file_name,
        input,
        output
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Generators.
// ---------------------------------------------------------------------------

/// A JSX element name — always a name-start, kept simple and distinctive so the
/// element can be located in the output. Starts with an uppercase letter to
/// avoid colliding with the `root`/`span` wrappers used in fixtures.
fn name_strategy() -> impl Strategy<Value = String> {
    "[A-Z][A-Za-z0-9]{1,8}".prop_map(|s| s)
}

/// A quoted-value attribute VALUE (no braces, so no confidence loss and no
/// stray `<`/`>` in values — keeps the generated tag confidently rewritable and
/// structure-safe). Values are ASCII with no `<`, `>`, `{`, `}`, `"`. The
/// attribute NAME is supplied by the caller so names stay UNIQUE within a tag
/// (duplicate attribute names are a behavior-affecting case the sorter must not
/// reorder, which is out of scope for these layout/idempotence properties).
fn attr_value_strategy() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 ._/-]{3,14}".prop_map(|v| v)
}

/// A genuine self-closing JSX `Start_Tag` `<Name a0="…" a1="…" ... />` with
/// enough UNIQUELY-named attributes to overflow the narrow test widths. Returns
/// `(name, tag)`. Unique names (`a0`, `a1`, …) keep the tag free of
/// duplicate-name/behavior-affecting ordering concerns so the layout and
/// idempotence properties isolate the wrapping/splicing logic.
fn genuine_tag_strategy() -> impl Strategy<Value = (String, String)> {
    (name_strategy(), prop::collection::vec(attr_value_strategy(), 4..8)).prop_map(
        |(name, values)| {
            let attrs = values
                .iter()
                .enumerate()
                .map(|(i, v)| format!("a{i}=\"{v}\""))
                .collect::<Vec<_>>()
                .join(" ");
            (name.clone(), format!("<{name} {attrs} />"))
        },
    )
}

/// The BODY of a non-element attribute value that must be preserved
/// byte-for-byte: a generic, a `<`-operator, an ordinary expression, a template
/// literal, an arrow, a ternary, a pure spread, or a regex literal. Returns the
/// value body WITHOUT a leading `name=` (the caller assigns a UNIQUE name), or,
/// for the pure-spread case, a full `{...rest}` token (the caller detects the
/// leading `{` and emits it verbatim with no name). Each body contains no
/// genuine JSX element, so `delimit_start_tag` records no child for it.
///
/// Every body is one the scanner delimits with full confidence in isolation
/// (mirroring the task-3 preservation baseline), so it is preserved
/// byte-for-byte: value-position generics and `<`-operators are not misread as
/// tags, template literals / arrows / ternaries are ordinary expression text,
/// and the regex literal `/a}b/` is consumed by the regex scanner (its `}` does
/// not close the brace).
fn non_element_value_body_strategy() -> impl Strategy<Value = String> {
    let ident = "[a-z][a-zA-Z0-9]{0,6}";
    let targ = "[A-Z][A-Za-z0-9]{0,6}";
    prop_oneof![
        // Generic call: `{fn<Targ>()}`.
        (ident, targ).prop_map(|(f, t)| format!("{{{f}<{t}>()}}")),
        // Nested generic: `{fn<Array<Targ>>()}`.
        (ident, targ).prop_map(|(f, t)| format!("{{{f}<Array<{t}>>()}}")),
        // `<`-operator: `{x < y}`.
        (ident, ident).prop_map(|(x, y)| format!("{{{x} < {y}}}")),
        // Ordinary expression: `{expr}`.
        ident.prop_map(|e| format!("{{{e}}}")),
        // Template literal: `{`${a}-${b}`}`.
        (ident, ident).prop_map(|(x, y)| format!("{{`${{{x}}}-${{{y}}}`}}")),
        // Arrow call: `{() => fn(x)}`.
        (ident, ident).prop_map(|(f, x)| format!("{{() => {f}({x})}}")),
        // Ternary: `{c ? a : b}`.
        (ident, ident, ident).prop_map(|(c, x, y)| format!("{{{c} ? {x} : {y}}}")),
        // Pure spread: `{...rest}` (caller emits with no name).
        ident.prop_map(|r| format!("{{...{r}}}")),
        // Regex literal in a call: `{re.test(/a}b/)}`.
        Just("{re.test(/a}b/)}".to_string()),
    ]
}

/// Map a `path_idx` to a `.jsx`/`.tsx` file name — the two dialects that descend
/// into attribute-value element containers.
fn jsx_or_tsx_path(path_idx: u8) -> &'static str {
    if path_idx.is_multiple_of(2) { "p.jsx" } else { "p.tsx" }
}

// ===========================================================================
// (a) Both-levels / in-out equivalence — Bug 2.1, 2.5, 2.8.
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// (a1) A nested attribute-value element wraps to the IDENTICAL
    /// per-attribute line grouping as the same element placed outside any
    /// container at the same base column. Wrapping SHAPE must not depend on
    /// living inside an attribute-value container (Bug 2.5).
    ///
    /// `<Wrap prop={ ` is 13 columns wide, so the in-container inner element
    /// begins at column 13; the out-of-container copy is indented 13 spaces to
    /// share that base column, isolating the wrapping decision from base indent.
    #[test]
    fn nested_element_wraps_identically_in_and_out_of_container(
        (name, tag) in genuine_tag_strategy(),
        path_idx in 0u8..2,
    ) {
        let cfg = config(60, true, false);
        let path = jsx_or_tsx_path(path_idx);

        let in_container = format!("<Wrap prop={{ {tag} }} />\n");
        let out_of_container = format!("             {tag}\n");

        let wrapped_in = format_to_string(path, &in_container, &cfg);
        let wrapped_out = format_to_string(path, &out_of_container, &cfg);

        let groups_in = attribute_line_groups(&wrapped_in, &name);
        let groups_out = attribute_line_groups(&wrapped_out, &name);
        prop_assert_eq!(
            groups_in, groups_out,
            "a nested element must wrap identically in and out of an attribute-value container\nin:\n{}\nout:\n{}",
            wrapped_in, wrapped_out
        );
    }

    /// (a2) The ENCLOSING tag wraps its OWN top-level attributes identically
    /// whether or not one of its attribute values carries a nested element
    /// (Bug 2.8 / Design Decision 0). The element-bearing variant uses a SHORT,
    /// single-line nested element; the plain variant replaces it with a value of
    /// the SAME rendered width, so the enclosing greedy-wrap decision is
    /// identical iff the enclosing tag stays confidently rewritable.
    #[test]
    fn enclosing_tag_wraps_identically_with_and_without_element_value(
        inner_name in name_strategy(),
        path_idx in 0u8..2,
    ) {
        let cfg = config(60, true, false);
        let path = jsx_or_tsx_path(path_idx);

        // A short, non-wrapping nested element so only the ENCLOSING tag's own
        // layout is under test. `<In />` is the shortest genuine element.
        let element_value = format!("body={{ <{inner_name} /> }}");
        // A plain value of equal rendered width (all ASCII, no `<`/`>`), so the
        // enclosing greedy-wrap decision is identical.
        let plain_value = format!("body={{{}}}", "x".repeat(element_value.len() - "body={}".len()));
        prop_assert_eq!(
            element_value.len(),
            plain_value.len(),
            "test premise: both `body` values must have equal rendered width"
        );

        let with_element = format!(
            "<ListPanel header=\"welcome\" footer=\"goodbye\" variant=\"info\" {element_value} dense divider />\n",
        );
        let without_element = format!(
            "<ListPanel header=\"welcome\" footer=\"goodbye\" variant=\"info\" {plain_value} dense divider />\n",
        );

        let out_with = format_to_string(path, &with_element, &cfg);
        let out_without = format_to_string(path, &without_element, &cfg);

        let names = ["header", "footer", "variant", "body", "dense", "divider"];
        let leading_with = enclosing_line_leading_attrs(&out_with, "ListPanel", &names);
        let leading_without = enclosing_line_leading_attrs(&out_without, "ListPanel", &names);

        prop_assert!(
            out_without.lines().filter(|l| !l.trim().is_empty()).count() > 1,
            "test premise: the enclosing tag must wrap at width 60\n{}", out_without
        );
        prop_assert_eq!(
            leading_with, leading_without,
            "an enclosing tag must wrap its own attributes identically whether or not an attribute value carries a nested element\nwith:\n{}\nwithout:\n{}",
            out_with, out_without
        );
    }
}

// ===========================================================================
// (b) Selective rewrite — Bug 2.1, 3.1, 3.2, 3.9.
//
// For an enclosing tag mixing non-element attribute values (generics, `<`
// operators, ordinary expressions, template literals, arrows, ternaries,
// spreads, regex literals) with ONE genuine element-bearing value, ONLY the
// genuine element is rewritten; every non-element attribute token is preserved
// byte-for-byte.
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn only_genuine_elements_are_rewritten_non_elements_preserved(
        bodies in prop::collection::vec(non_element_value_body_strategy(), 2..5),
        (inner_name, inner_tag) in genuine_tag_strategy(),
        path_idx in 0u8..2,
    ) {
        let cfg = config(60, true, false);
        let path = jsx_or_tsx_path(path_idx);

        // Assign each non-element value a UNIQUE name (`ne0`, `ne1`, …), except a
        // pure spread (`{...rest}`) which has no name. Record the exact token we
        // expect to find verbatim in the output.
        let non_element_tokens: Vec<String> = bodies
            .iter()
            .enumerate()
            .map(|(i, body)| {
                if body.starts_with("{...") {
                    body.clone()
                } else {
                    format!("ne{i}={body}")
                }
            })
            .collect();

        // Build the enclosing tag: a genuine element-bearing value plus the
        // generated non-element values, all under one `<Host …/>`.
        let element_value = format!("content={{ {inner_tag} }}");
        let mut parts = vec!["<Host".to_string(), "lead=\"x\"".to_string(), element_value];
        parts.extend(non_element_tokens.iter().cloned());
        parts.push("/>".to_string());
        let input = format!("{}\n", parts.join(" "));

        let output = assert_idempotent(path, &input, &cfg)?;

        // Every non-element attribute token survives byte-for-byte: the selective
        // rewrite never touches a generic, `<`-operator, template literal, arrow,
        // ternary, spread, or regex literal.
        for token in &non_element_tokens {
            prop_assert!(
                output.contains(token.as_str()),
                "non-element token {:?} must be preserved byte-for-byte\n  output: {:?}",
                token, output
            );
        }

        // The genuine inner element is still present (name survives) and the
        // rewrite is structure-safe + angle-bracket conserving.
        prop_assert!(
            output.contains(&format!("<{inner_name}")),
            "the genuine inner element <{}> must survive\n  output: {:?}",
            inner_name, output
        );
        assert_structure_safe(path, &input, &output, &cfg)?;
    }
}

// ===========================================================================
// (c) Idempotence over both levels — Bug 3.7; Requirement 8, 8.5.
//
// Inputs with in-attribute-value JSX at several depths reach a byte-for-byte
// fixed point. The shared `display_width` model is used at both levels in both
// self-check passes (Requirement 8.5); property (d) is the dedicated fixture for
// the rendered-geometry risk.
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn two_level_attribute_value_jsx_is_idempotent(
        (_n1, tag1) in genuine_tag_strategy(),
        (_n2, tag2) in genuine_tag_strategy(),
        depth in 1u8..4,
        sorting in any::<bool>(),
        width in 36u32..64,
        path_idx in 0u8..2,
    ) {
        let cfg = config(width, true, sorting);
        let path = jsx_or_tsx_path(path_idx);

        // Nest the generated element(s) inside attribute values to `depth`
        // levels. At depth 1 the enclosing tag carries one element-bearing value;
        // deeper levels put an element-bearing value inside the previous element.
        let input = match depth {
            1 => format!("<Host lead=\"x\" body={{ {tag1} }} tail=\"y\" />\n"),
            2 => format!(
                "<Host lead=\"x\" body={{ <Mid one=\"a\" inner={{ {tag1} }} two=\"b\" /> }} tail=\"y\" />\n",
            ),
            _ => format!(
                "<Host lead=\"x\" body={{ <Mid one=\"a\" inner={{ {tag1} }} nested={{ {tag2} }} two=\"b\" /> }} tail=\"y\" />\n",
            ),
        };

        // First pass must succeed and be a byte-for-byte fixed point under a
        // second pass (Requirement 8). A geometry taken from the source rather
        // than the rendered position would oscillate here.
        let _first = assert_idempotent(path, &input, &cfg)?;
    }
}

// ===========================================================================
// (d) Dedicated two-level re-wrap idempotence fixture — Finding 4; Bug 3.7.
//
// The enclosing tag has enough attributes to WRAP AND carries an element-bearing
// attribute value whose inner element ITSELF overflows the line width, so both
// levels re-wrap in the same pass and the inner element is spliced at a MOVED
// continuation column. This is the exact case that would oscillate if child
// geometry were read from the source rather than recomputed from the rendered
// position. LF and CRLF variants and a sorting-enabled variant are covered.
//
// This is a FIXED fixture (the highest idempotence risk), driven as a small
// proptest over the newline and sorting dimensions so a regression surfaces a
// minimal counterexample.
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    #[test]
    fn two_level_rewrap_moved_continuation_is_fixed_point(
        crlf in any::<bool>(),
        sorting in any::<bool>(),
        path_idx in 0u8..2,
    ) {
        let cfg = config(60, true, sorting);
        let path = jsx_or_tsx_path(path_idx);

        // Enclosing <Host> has 7 wide top-level attributes (wraps at width 60),
        // and its `content={ … }` value holds an <Inner> whose own attributes
        // also overflow width 60 — so BOTH levels re-wrap and <Inner> is spliced
        // at a moved continuation column.
        let lf = concat!(
            "<Host alpha=\"first-value\" beta=\"second-value\" gamma=\"third-value\" ",
            "content={ <Inner one=\"aaaaaaaaaa\" two=\"bbbbbbbbbb\" three=\"cccccccccc\" ",
            "four=\"dddddddddd\" five=\"eeeeeeeeee\" /> } ",
            "delta=\"fourth-value\" epsilon=\"fifth-value\" zeta=\"sixth-value\" />\n",
        );
        let input = if crlf { lf.replace('\n', "\r\n") } else { lf.to_string() };

        let first = assert_idempotent(path, &input, &cfg)?;

        // Premise: both levels actually wrapped (so this truly exercises the
        // moved-continuation splice, not a trivial single-line no-op).
        prop_assert!(
            attribute_line_groups(&first, "Inner").len() > 1,
            "test premise: inner element must wrap\n{}", first
        );
        // The enclosing tag wrapped too: more than one non-empty line.
        prop_assert!(
            first.lines().filter(|l| !l.trim().is_empty()).count() > 1,
            "test premise: enclosing tag must wrap\n{}", first
        );

        // CRLF line endings are preserved through the two-level rewrite.
        if crlf {
            prop_assert!(
                first.contains("\r\n") && !first.contains("\n\n"),
                "CRLF line endings must be preserved\n{:?}", first
            );
        }

        assert_structure_safe(path, &input, &first, &cfg)?;
    }
}

// ===========================================================================
// (e) Structure equivalence + angle-bracket conservation over children —
// Bug 3.8; Requirement 7.5, 7.8.
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn two_level_rewrite_is_structure_equivalent_and_conserves_brackets(
        (_n1, tag1) in genuine_tag_strategy(),
        (_n2, tag2) in genuine_tag_strategy(),
        two_children in any::<bool>(),
        sorting in any::<bool>(),
        width in 36u32..64,
        path_idx in 0u8..2,
    ) {
        let cfg = config(width, true, sorting);
        let path = jsx_or_tsx_path(path_idx);

        let input = if two_children {
            format!("<Host lead=\"x\" first={{ {tag1} }} second={{ {tag2} }} tail=\"y\" />\n")
        } else {
            format!("<Host lead=\"x\" only={{ {tag1} }} tail=\"y\" />\n")
        };

        let output = format_to_string(path, &input, &cfg);

        // Structure equivalence (fingerprint relation extended over `children`)
        // and angle-bracket conservation hold for every legitimate two-level
        // wrap/sort.
        assert_structure_safe(path, &input, &output, &cfg)?;
    }
}

// ===========================================================================
// (f) Arbitrary depth — Bug 2.2.
//
// `a={ <X b={ <Y c={ <Z …/> }/> }/> }` must produce a correctly nested
// `children` tree: one node per genuine element at each level, with correct,
// non-overlapping relative spans. Inspected via the public `scan_start_tags`
// API over the JSX/TSX profiles.
// ===========================================================================

/// Walk a `ScannedTag` tree and assert, for each node, that every child's span
/// lies within `[0, node_len)` (relative to the node), that children spans are
/// pairwise non-overlapping and source-ordered, and that each child's
/// intra-`raw` provenance resolves inside the owning attribute — then recurse
/// into each child.
fn assert_children_tree_well_formed(tag: &ScannedTag, node_len: usize) -> Result<(), TestCaseError> {
    let mut prev_end = 0usize;
    for NestedChild { owner_attr_index, offset_in_attr_raw, tag: child } in &tag.children {
        // Child span is relative to this node's `span.start` and lies within it.
        prop_assert!(
            child.span.start <= child.span.end && child.span.end <= node_len,
            "child span {:?} out of node bounds (len {})",
            child.span, node_len
        );
        // Non-overlapping and source-ordered.
        prop_assert!(
            child.span.start >= prev_end,
            "child spans overlap or are out of order: prev_end={}, child.span={:?}",
            prev_end, child.span
        );
        prev_end = child.span.end;

        // Provenance resolves inside the owning attribute's raw slice.
        let owner = tag
            .attributes
            .get(*owner_attr_index)
            .expect("owner_attr_index must index a real attribute");
        prop_assert!(
            *offset_in_attr_raw < owner.raw.len(),
            "offset_in_attr_raw {} must fall inside owner raw of len {}",
            offset_in_attr_raw, owner.raw.len()
        );
        prop_assert!(
            owner.raw.as_bytes()[*offset_in_attr_raw] == b'<',
            "owner.raw[offset_in_attr_raw] must be the child's `<`, got {:?}",
            &owner.raw[*offset_in_attr_raw..],
        );

        // Recurse: the child's own children are relative to the child's span.
        let child_len = child.span.end - child.span.start;
        assert_children_tree_well_formed(child, child_len)?;
    }
    Ok(())
}

/// Collect every element name in a `ScannedTag` tree, pre-order.
fn collect_names<'a>(tag: &ScannedTag<'a>, out: &mut Vec<&'a str>) {
    out.push(tag.name);
    for child in &tag.children {
        collect_names(&child.tag, out);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Arbitrary-depth attribute-value nesting records one `children` node per
    /// genuine element with correct, non-overlapping relative spans at every
    /// level. Generated to depths 1..=3 with distinctive element names so the
    /// tree shape can be asserted exactly.
    #[test]
    fn arbitrary_depth_builds_correct_children_tree(
        depth in 1usize..4,
        path_idx in 0u8..2,
    ) {
        // Distinctive names per level; the innermost element overflows.
        let leaf = "<Z fourth=\"value-four\" fifth=\"value-five\" sixth=\"value-six\" />";
        let input = match depth {
            1 => format!("<Root first={{ {leaf} }} />"),
            2 => format!("<Root first={{ <Y second=\"val\" third={{ {leaf} }} /> }} />"),
            _ => format!(
                "<Root first={{ <X a=\"val\" nested={{ <Y second=\"val\" third={{ {leaf} }} /> }} /> }} />",
            ),
        };

        let dialect = if path_idx.is_multiple_of(2) { Dialect::Jsx } else { Dialect::Tsx };
        let profile = dialect.scanner_profile();
        let tags = scan_start_tags(&input, &profile, 2)
            .expect("arbitrary-depth input scans without a parse failure");

        // Exactly one top-level enclosing tag (<Root>); the nested elements live
        // in its `children` tree, NOT in the flat top-level list.
        prop_assert_eq!(tags.len(), 1, "expected one top-level tag, got {:?}", tags.len());
        let root = &tags[0];
        prop_assert_eq!(root.name, "Root");

        // The children tree is well-formed at every level.
        let root_len = root.span.end - root.span.start;
        assert_children_tree_well_formed(root, root_len)?;

        // One node per genuine element: the expected names appear exactly once,
        // and no spurious element (generic / `<`-operator) was recorded.
        let mut names = Vec::new();
        collect_names(root, &mut names);
        let expected: Vec<&str> = match depth {
            1 => vec!["Root", "Z"],
            2 => vec!["Root", "Y", "Z"],
            _ => vec!["Root", "X", "Y", "Z"],
        };
        let mut sorted_got = names.clone();
        sorted_got.sort_unstable();
        let mut sorted_expected = expected.clone();
        sorted_expected.sort_unstable();
        prop_assert_eq!(
            sorted_got, sorted_expected,
            "children tree must have exactly one node per genuine element; got {:?}",
            names
        );

        // Every node in the tree is confidently rewritable (none of the
        // generated elements carries a complex-expression or generics-head).
        fn all_confident(tag: &ScannedTag) -> bool {
            tag.confidently_rewritable && tag.children.iter().all(|c| all_confident(&c.tag))
        }
        prop_assert!(all_confident(root), "every generated element must be confidently rewritable");
    }
}

/// A `.tsx` generics-head inside an attribute value records NO child and the
/// value is preserved — the generics guard holds inside attribute-value
/// containers (Bug 3.1, 3.9). A deterministic companion to the depth property.
#[test]
fn generics_head_in_attribute_value_records_no_child() {
    let input = "<Wrap item={ <EditableList<SearchEngineDomain> a=\"1\" b=\"2\" /> } x=\"1\" y=\"2\" />";
    let profile = Dialect::Tsx.scanner_profile();
    let tags = scan_start_tags(input, &profile, 2).expect("scans cleanly");
    assert_eq!(tags.len(), 1, "only the enclosing <Wrap> is a top-level tag");
    // The generics-head inner element is NOT confidently delimited, so it is not
    // recorded as a rewritable child (a per-tag no-op): the enclosing tag's
    // `item` value is preserved verbatim.
    let output = format_to_string("gen_head.tsx", input, &config(40, true, false));
    assert!(
        output.contains("<EditableList<SearchEngineDomain> a=\"1\" b=\"2\" />"),
        "generics-head value must be preserved byte-for-byte\n  output: {output:?}",
    );
}
