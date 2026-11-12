//! JSX-in-child-containers regression corpora
//! (jsx-in-child-containers-not-wrapped spec tasks 5.2 and 5.3).
//!
//! These fixtures lock in the fix for the "JSX in child containers not wrapped"
//! regression. Before the fix, every child-position `{ … }` JSX expression
//! container was treated as fully opaque, so genuine JSX element Start_Tags
//! rendered from inside `.map()` callbacks, ternaries, `&&`/`||` conditionals,
//! and nested containers were never recognized, wrapped, or sorted. The fix
//! walks into such containers under the same expression-position gate, so the
//! inner JSX is now formatted exactly like the same tag outside a container,
//! while TypeScript generics inside those containers remain byte-for-byte
//! preserved.
//!
//! This corpus drives the PUBLIC `format_document` seam (as
//! `tests/tsx_generics_corpora.rs` and `tests/structure_equivalence_corpora.rs`
//! do) and reuses the plugin's own Requirement 7.5 fingerprint relation
//! (`structural_fingerprint` / `fingerprints_equivalent`).
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
/// own Requirement 7.5 fingerprint relation (tolerating wrapping whitespace).
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

/// Count `<` and `>` in `s` — the angle-bracket-conservation invariant must hold
/// across a legitimate wrap/sort (no bracket ever added or dropped).
fn angle_count(s: &str) -> usize {
    s.bytes().filter(|&b| b == b'<' || b == b'>').count()
}

/// Assert the start tag named `component` wrapped onto multiple lines in
/// `output`: its span from `<Name` to the terminating `>` contains a newline.
fn assert_component_wrapped(file_name: &str, component: &str, output: &str) {
    let open = format!("<{component}");
    let start = output.find(&open).unwrap_or_else(|| {
        panic!("component {component:?} missing from output for {file_name:?}\n  output: {output:?}")
    });
    let after = &output[start + open.len()..];
    let terminator = after.find('>').unwrap_or_else(|| {
        panic!("unterminated start tag for {component:?} in {file_name:?}\n  output: {output:?}")
    });
    assert!(
        after[..terminator].contains('\n'),
        "expected component {component:?} to wrap onto multiple lines for {file_name:?}\n  output: {output:?}",
    );
}

/// Assert `format_document` is idempotent on `input`: pass 2 over the pass-1
/// output is byte-for-byte identical.
fn assert_idempotent(file_name: &str, input: &str, config: &Configuration) -> String {
    let first = format_to_string(file_name, input, config);
    let second = format_to_string(file_name, &first, config);
    assert_eq!(
        second, first,
        "idempotence failed for {file_name:?}\n  first:  {first:?}\n  second: {second:?}",
    );
    first
}

// ---------------------------------------------------------------------------
// Real-world header snippet — Requirement 3.10.
//
// A `.tsx` header fragment combining a `.map()`, a ternary, an `&&` conditional,
// and nested containers rendering an overflowing <IconButton> one-attribute-
// per-line inside a `{ … }` container. The plugin must repack the one-per-line
// <IconButton> into fluid form, keep the output valid TSX, be idempotent, and
// preserve the document structure.
// ---------------------------------------------------------------------------

#[test]
fn real_world_header_snippet_is_repacked_and_safe() {
    let cfg = config(40, true, false);
    let input = "\
<header className=\"app-header\">
  {items.map(item => (
    <IconButton
      color=\"primary\"
      size=\"large\"
      edge=\"end\"
      onClick={item.onClick}
      aria-label={item.label}
      className=\"header-icon-button\"
    >
      {item.icon}
    </IconButton>
  ))}
  {open ? (
    <Drawer anchor=\"right\" variant=\"temporary\" elevation={8} keepMounted hideBackdrop>
      content
    </Drawer>
  ) : null}
  {loggedIn && (
    <Avatar userName=\"alice\" sizePx={32} rounded showStatus statusColor=\"green\" />
  )}
</header>
";
    let output = assert_idempotent("header.tsx", input, &cfg);

    // The one-attribute-per-line <IconButton> is repacked: at least two of its
    // attributes now share a continuation line.
    assert!(
        output.contains("color=\"primary\" size=\"large\"")
            || output.contains("size=\"large\" edge=\"end\""),
        "IconButton must be repacked to fluid form\n  output: {output:?}",
    );
    // The in-container <Drawer> (single line, overflowing) is wrapped.
    assert_component_wrapped("header.tsx", "Drawer", &output);

    // Valid-TSX structure preserved under the fingerprint relation, and no angle
    // bracket added or dropped (angle-bracket conservation never trips).
    assert_structure_equivalent("header.tsx", input, &output, &cfg);
    assert_eq!(
        angle_count(input),
        angle_count(&output),
        "no `<`/`>` may be added or dropped",
    );
}

// ---------------------------------------------------------------------------
// One fixture per container shape — the in-container Start_Tag is fluid-wrapped
// (Requirements 2.1, 2.2, 2.3, 2.4).
// ---------------------------------------------------------------------------

#[test]
fn each_container_shape_wraps_its_inner_tag() {
    let cfg = config(40, true, false);

    // (name, source, the in-container component that must wrap).
    let fixtures = [
        // Map callback (2.1).
        (
            "map.tsx",
            "<ul>\n  {pages.map(p => (\n    <Button variant=\"ghost\" size=\"sm\" onClick={p.go} disabled={p.busy} aria-label=\"next\" />\n  ))}\n</ul>\n",
            "Button",
        ),
        // Ternary (2.2).
        (
            "ternary.tsx",
            "<nav>\n  {open ? (\n    <IconButton color=\"primary\" size=\"large\" edge=\"end\" onClick={toggle} aria-label=\"menu\" />\n  ) : null}\n</nav>\n",
            "IconButton",
        ),
        // `&&` conditional (2.3).
        (
            "and.tsx",
            "<div>\n  {loggedIn && (\n    <Panel title=\"Account\" variant=\"outlined\" elevation={2} square dense className=\"p\" />\n  )}\n</div>\n",
            "Panel",
        ),
        // Nested container (2.4): the inner <Cell> inside a nested `{ … }`.
        (
            "nested.tsx",
            "<table>\n  {rows.map(r => (\n    <Row key={r.id}>\n      {r.cells.map(c => (\n        <Cell align=\"center\" padding=\"none\" className=\"c\" layout=\"fixed\" scope=\"col\" />\n      ))}\n    </Row>\n  ))}\n</table>\n",
            "Cell",
        ),
    ];

    for (name, src, component) in fixtures {
        let output = assert_idempotent(name, src, &cfg);
        assert_component_wrapped(name, component, &output);
        assert_structure_equivalent(name, src, &output, &cfg);
        assert_eq!(angle_count(src), angle_count(&output), "angle brackets conserved for {name}");
    }
}

// ---------------------------------------------------------------------------
// Generics inside those same containers are byte-for-byte preserved
// (Requirement 3.1).
// ---------------------------------------------------------------------------

#[test]
fn generics_inside_containers_are_preserved() {
    let cfg = config(40, true, false);
    let corpus = [
        "map_generic.tsx",
        "ternary_generic.tsx",
        "and_generic.tsx",
        "nested_generic.tsx",
    ];
    let sources = [
        "<div>{items.map(x => someGeneric<T>())}</div>",
        "<div>{open ? useState<Array<Task>>() : null}</div>",
        "<div>{loggedIn && cfg.get<Array<Url>>()}</div>",
        "<div>{rows.map(r => r.cells.map(c => build<Cell<Data>>(c)))}</div>",
    ];
    for (name, input) in corpus.iter().zip(sources.iter()) {
        let output = format_to_string(name, input, &cfg);
        assert_eq!(
            &output, input,
            "in-container generic must be preserved byte-for-byte for {name:?}\n  output: {output:?}",
        );
    }
}

// ---------------------------------------------------------------------------
// In-container == out-of-container consistency — Requirement 2.6.
//
// The SAME genuine JSX Start_Tag, placed at column 0, wraps to the identical
// per-attribute line grouping whether it is inside a child container or at a
// top-level child position (modulo container-relative base indent, isolated
// here by placing both at the same base column).
// ---------------------------------------------------------------------------

/// Extract the per-line grouping of attribute tokens for the start tag whose
/// element name matches `component`, from `<Name` through the terminator. Used
/// to compare wrapping SHAPE independent of base indentation.
fn attribute_line_groups(output: &str, component: &str) -> Vec<Vec<String>> {
    let open = format!("<{component}");
    let start = output.find(&open).expect("output must contain the start tag");
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

#[test]
fn same_tag_in_and_out_of_container_wraps_identically() {
    let cfg = config(40, true, false);
    let tag = "<Banner title=\"welcome\" subtitle=\"hello there\" variant=\"info\" dismissible closable=\"yes\" />";

    // Both at column 0 so the comparison isolates the wrapping decision from the
    // base-indent column.
    let in_container = format!("<root>\n{{cond && (\n{tag}\n)}}\n</root>\n");
    let out_of_container = format!("<root>\n{tag}\n</root>\n");

    let wrapped_in = format_to_string("consistency_in.tsx", &in_container, &cfg);
    let wrapped_out = format_to_string("consistency_out.tsx", &out_of_container, &cfg);

    let groups_in = attribute_line_groups(&wrapped_in, "Banner");
    let groups_out = attribute_line_groups(&wrapped_out, "Banner");
    assert!(groups_out.len() > 1, "test premise: the tag must wrap at width 40\n{wrapped_out}");
    assert_eq!(
        groups_in, groups_out,
        "in-container and out-of-container wrapping must match\nin:\n{wrapped_in}\nout:\n{wrapped_out}",
    );
}

// ---------------------------------------------------------------------------
// Mixed whole-file fixture — Requirements 2.8, 3.1.
//
// Interleaves generics (preserved), out-of-container components (wrapped), and
// in-container components in map/ternary/`&&`/nested positions (now wrapped):
// output is valid TSX, generics byte-for-byte intact, every genuine element
// wrapped, idempotent, structure preserved.
// ---------------------------------------------------------------------------

#[test]
fn mixed_generics_and_in_container_jsx() {
    let cfg = config(40, true, false);
    let input = "\
function Page() {
  const [tasks, setTasks] = useState<Array<Task>>([]);
  const stats = axiosConfig.get<Array<UrlStats>>(url);
  return (
    <main>
      <PageHeading title=\"Dashboard\" subtitle=\"overview\" sticky theme=\"dark\" compact />
      <ul>
        {tasks.map(t => (
          <TaskRow key={t.id} label={t.label} status=\"open\" priority=\"high\" selectable />
        ))}
      </ul>
      {ready ? (
        <Summary total={tasks.length} pending={pending} done={done} variant=\"card\" bordered />
      ) : null}
      {error && (
        <Alert severity=\"error\" title=\"Oops\" dismissible closable=\"yes\" variant=\"filled\" />
      )}
    </main>
  );
}
";
    let output = assert_idempotent("mixed.tsx", input, &cfg);

    // Generics preserved verbatim.
    assert!(
        output.contains("useState<Array<Task>>"),
        "the statement generic must survive verbatim\n  output: {output:?}",
    );
    assert!(
        output.contains("axiosConfig.get<Array<UrlStats>>"),
        "the member-call generic must survive verbatim\n  output: {output:?}",
    );

    // Every genuine element — out-of-container and in-container — wraps.
    for component in ["PageHeading", "TaskRow", "Summary", "Alert"] {
        assert_component_wrapped("mixed.tsx", component, &output);
    }

    // Structure preserved and no angle bracket added/dropped.
    assert_structure_equivalent("mixed.tsx", input, &output, &cfg);
    assert_eq!(angle_count(input), angle_count(&output), "angle brackets conserved");
}
