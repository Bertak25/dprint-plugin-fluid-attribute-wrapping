//! JSX attribute-value-container regression corpora — the FLIPPED two-level
//! EXPECTED-BEHAVIOR corpus for the `jsx-nested-container-start-tag-not-wrapped`
//! spec (tasks.md task 4.5; same fixtures first written at task 2).
//!
//! # What this corpus asserts (the fixed behavior)
//!
//! An enclosing JSX/TSX element Start_Tag can carry an attribute whose **value**
//! is a braced expression container holding *another* genuine JSX element
//! (`primary={ <a …>…</a> }`, `secondary={ <Chip … /> }`,
//! `secondaryAction={ <Stack …><IconButton …/></Stack> }`). Correct behavior —
//! now implemented by the recursive two-level rewrite (task 4) — is **two-level
//! formatting**: the enclosing tag's own top-level attributes are fluid-wrapped
//! (and, where enabled, sorted) AND every genuine JSX element nested inside an
//! attribute value is *independently* fluid-wrapped/sorted on its own rendered
//! geometry, while all non-element bytes (template literals, call expressions,
//! arrow functions, ternaries, text children) are preserved byte-for-byte.
//!
//! This corpus runs on the **FIXED code** (Design Decision 0 — the `</` regex
//! guard in `delimit_braced` — AND the recursive children-tree fix of task 4
//! both applied). The assertions below encode F' behavior:
//!
//!   * Because Decision 0 makes an element-bearing attribute value delimit
//!     *confidently*, the enclosing tag is confidently rewritable, so its own
//!     top-level attributes fluid-wrap (and sort where enabled).
//!   * Because `delimit_start_tag` now records each genuine nested element as a
//!     relative-span child and `rewrite_tag` recurses into those children —
//!     mapping source→rendered by attribute identity and recomputing child
//!     geometry from the rendered position — the nested `<a>` / `<Chip>` /
//!     `<Stack>` / `<IconButton>` is *independently* fluid-wrapped on its own
//!     geometry. It is no longer a byte-for-byte no-op.
//!
//! Each fixture therefore asserts the two-level expectation: the enclosing tag's
//! own attributes span multiple lines AND the nested attribute-value element's
//! own start tag wraps when it overflows Line_Width. The template-literal /
//! call / arrow / ternary attribute values — which contain no genuine element —
//! stay preserved verbatim; only whitespace BETWEEN the enclosing top-level
//! attributes, and inside a rewritten nested element's own start tag, changes.
//! Input↔output structure equivalence (Requirement 7.5, its fingerprint
//! relation extended over `children`) and angle-bracket conservation hold for
//! every rewrite.
//!
//! # History
//!
//! These fixtures were first written at task 2 to FAIL on task-1 code (the
//! two-level no-op at the nested level), surfacing the bug. Task 4.5 (this
//! change) FLIPPED each nested-level assertion from "left unchanged" to "the
//! nested element wraps too", keeping the enclosing-tag-wrapped expectation —
//! the same fixtures (counterexamples), the opposite expectation. The task-3
//! PRESERVATION BASELINE module further down is unchanged and still passes.
//!
//! This corpus drives the PUBLIC `format_document` seam (as
//! `tests/jsx_in_child_containers_corpora.rs` does) and reuses the plugin's own
//! Requirement 7.5 fingerprint relation (`structural_fingerprint` /
//! `fingerprints_equivalent`).
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
/// Used for BOTH the enclosing tag and each nested attribute-value element —
/// under F' both levels are independently fluid-wrapped when they overflow.
fn assert_component_wrapped(file_name: &str, component: &str, output: &str) {
    assert!(
        start_tag_span_contains_newline(file_name, component, output),
        "expected component {component:?} to wrap onto multiple lines for {file_name:?}\n  output: {output:?}",
    );
}

/// Does the start tag named `component` (`<Name` … first following `>`) contain
/// a newline? Backs `assert_component_wrapped`.
fn start_tag_span_contains_newline(file_name: &str, component: &str, output: &str) -> bool {
    let open = format!("<{component}");
    let start = output.find(&open).unwrap_or_else(|| {
        panic!("component {component:?} missing from output for {file_name:?}\n  output: {output:?}")
    });
    let after = &output[start + open.len()..];
    let terminator = after.find('>').unwrap_or_else(|| {
        panic!("unterminated start tag for {component:?} in {file_name:?}\n  output: {output:?}")
    });
    after[..terminator].contains('\n')
}

/// Assert `needle` appears verbatim in `output` — used to lock in that a
/// non-element attribute value (template literal, call, arrow, ternary) is
/// preserved byte-for-byte by the no-op.
fn assert_contains_verbatim(file_name: &str, needle: &str, output: &str) {
    assert!(
        output.contains(needle),
        "expected {needle:?} preserved byte-for-byte in {file_name:?}\n  output: {output:?}",
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

/// Extract the per-line grouping of attribute tokens for the start tag whose
/// element name matches `component`, from `<Name` through the terminator. Used
/// to inspect the ENCLOSING tag's wrap shape independent of base indentation.
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

/// Shared assertions every fixture makes: the rewrite preserves document
/// structure under the Requirement 7.5 relation (fingerprint extended over
/// `children`), and conserves every angle bracket (none added or dropped by a
/// legitimate two-level wrap/sort).
fn assert_structure_safe(file_name: &str, input: &str, output: &str, config: &Configuration) {
    assert_structure_equivalent(file_name, input, output, config);
    assert_eq!(
        angle_count(input),
        angle_count(output),
        "no `<`/`>` may be added or dropped for {file_name}",
    );
}

// ===========================================================================
// Real-world Example 2 — `<TaskStatisticListItem>`.
//
// bugfix 2.1–2.8; main 3.2, 4, 4.2, 4.10, 7.5. The enclosing tag has eight
// attributes; `primary={ <a className=… href=… >…</a> }` holds a nested `<a>`
// WITH an end tag (directly exercises Decision 0). Other values exercise
// template literals (`key`, `href`), a call (`exists`), an arrow (`onRun`), and
// a multiline ternary arrow (`onDelete`).
//
// F' (FIXED): the enclosing <TaskStatisticListItem> own attributes fluid-wrap
// (Decision 0 made them confident) AND the nested <a> inside primary={ … } is
// INDEPENDENTLY fluid-wrapped on its own geometry. The key/href template
// literals, the exists call, the onRun arrow, and the onDelete ternary survive
// byte-for-byte — only whitespace between the enclosing top-level attributes,
// and inside the rewritten <a> start tag, changed.
// ===========================================================================

#[test]
fn task_statistic_list_item_formats_both_levels() {
    let cfg = config(80, true, false);
    // The user's FULL snippet, flattened to a single upstream line per tag so the
    // plugin is the one deciding where to break. The nested <a> start tag is well
    // over Line_Width 80 and wraps now that the engine recognizes it.
    let input = concat!(
        "<TaskStatisticListItem ",
        "key={`${studentTask.task}-${studentTask.user}`} ",
        "index={index} ",
        "status={studentTask.status} ",
        "exists={existStudentTasks(studentTask)} ",
        "primary={ ",
        "<a ",
        "className=\"text-base text-blue-600 hover:text-blue-500 no-underline hover:underline\" ",
        "href={`/dashboard?userId=${studentTask.user}&taskId=${studentTask.task}`} ",
        ">",
        "{`${tasksMap[studentTask.task]?.title} (${tasksMap[studentTask.task]?.lang})`}",
        "</a>",
        " } ",
        "onRun={() => handleStatisticRunClick(studentTask.task, studentTask.user)} ",
        "onDelete={ability.can(actionType.manage, subjectType.clazz) ",
        "? () => handleStatisticDeleteClick(studentTask.task, studentTask.user) ",
        ": undefined} ",
        "/>\n",
    );
    let output = assert_idempotent("task_statistic.tsx", input, &cfg);

    // Level 1 — the enclosing tag's OWN attributes fluid-wrap onto >1 line.
    assert_component_wrapped("task_statistic.tsx", "TaskStatisticListItem", &output);
    assert!(
        attribute_line_groups(&output, "TaskStatisticListItem").len() > 1,
        "enclosing <TaskStatisticListItem> must span multiple lines\n  output: {output:?}",
    );
    // Level 2 — the nested <a> (end-tag-bearing; Decision 0) wraps independently.
    assert_component_wrapped("task_statistic.tsx", "a", &output);

    // Non-element attribute values preserved byte-for-byte (no genuine element).
    assert_contains_verbatim(
        "task_statistic.tsx",
        "key={`${studentTask.task}-${studentTask.user}`}",
        &output,
    );
    assert_contains_verbatim(
        "task_statistic.tsx",
        "href={`/dashboard?userId=${studentTask.user}&taskId=${studentTask.task}`}",
        &output,
    );
    assert_contains_verbatim("task_statistic.tsx", "exists={existStudentTasks(studentTask)}", &output);
    assert_contains_verbatim(
        "task_statistic.tsx",
        "onRun={() => handleStatisticRunClick(studentTask.task, studentTask.user)}",
        &output,
    );
    assert_contains_verbatim(
        "task_statistic.tsx",
        "onDelete={ability.can(actionType.manage, subjectType.clazz) ? () => handleStatisticDeleteClick(studentTask.task, studentTask.user) : undefined}",
        &output,
    );

    assert_structure_safe("task_statistic.tsx", input, &output, &cfg);
}

// ===========================================================================
// Real-world Example 1 — Material UI `<ListItemText>` / `<ListItem>`.
//
// bugfix 2.1–2.8; main 3.2, 4, 4.10, 7.5. The inner `<span>…</span>` and
// `<Stack>…</Stack>` carry end tags (exercise Decision 0); the inner <Chip>,
// <Stack>, <IconButton> overflow Line_Width.
//
// F' (FIXED): the enclosing tag's own attributes wrap AND <Chip>, <Stack>, and
// <IconButton> are each independently fluid-wrapped (recognized as tags and
// formatted on their own geometry).
// ===========================================================================

#[test]
fn material_ui_span_chip_formats_both_levels() {
    let cfg = config(60, true, false);
    let input = concat!(
        "<ListItemText primary={primary} ",
        "secondary={ <span><Chip size={chipSize} variant={chipVariant} ",
        "color={longColorExpression} label={longLabelExpression} /></span> } ",
        "className=\"list-item-text-root\" dense>\n",
        "</ListItemText>\n",
    );
    let output = assert_idempotent("mui_span_chip.tsx", input, &cfg);

    // Level 1 — the enclosing <ListItemText> own attributes wrap onto >1 line.
    assert_component_wrapped("mui_span_chip.tsx", "ListItemText", &output);
    assert!(
        attribute_line_groups(&output, "ListItemText").len() > 1,
        "enclosing <ListItemText> must span multiple lines\n  output: {output:?}",
    );
    // Level 2 — <Chip> inside the <span> wrapper (end-tag-bearing) wraps.
    assert_component_wrapped("mui_span_chip.tsx", "Chip", &output);
    // Ordinary attribute value preserved byte-for-byte (no genuine element).
    assert_contains_verbatim("mui_span_chip.tsx", "primary={primary}", &output);

    assert_structure_safe("mui_span_chip.tsx", input, &output, &cfg);
}

#[test]
fn material_ui_stack_icon_button_formats_both_levels() {
    let cfg = config(60, true, false);
    let input = concat!(
        "<ListItem secondaryAction={ <Stack direction={stackDirection} spacing={1}>",
        "<IconButton loading={isLoading} onClick={onClick} color=\"primary\" /></Stack> } ",
        "className=\"list-item-root\" dense disableGutters divider>\n",
        "</ListItem>\n",
    );
    let output = assert_idempotent("mui_stack.tsx", input, &cfg);

    // Level 1 — the enclosing <ListItem> own attributes wrap onto >1 line.
    assert_component_wrapped("mui_stack.tsx", "ListItem", &output);
    assert!(
        attribute_line_groups(&output, "ListItem").len() > 1,
        "enclosing <ListItem> must span multiple lines\n  output: {output:?}",
    );
    // Level 2 — the attribute-value element <Stack> AND its child-position
    // descendant <IconButton> each wrap independently.
    assert_component_wrapped("mui_stack.tsx", "Stack", &output);
    assert_component_wrapped("mui_stack.tsx", "IconButton", &output);

    assert_structure_safe("mui_stack.tsx", input, &output, &cfg);
}

// ===========================================================================
// Direct attribute-value element — self-closing <Chip> directly in the value.
//
// bugfix 2.1. Overflowing enclosing tag AND overflowing <Chip>.
//
// F' (FIXED): enclosing <ListItemText> wraps its own attributes AND the
// self-closing <Chip> in `secondary={ … }` independently wraps.
// ===========================================================================

#[test]
fn direct_attribute_value_chip_formats_both_levels() {
    let cfg = config(60, true, false);
    let input = concat!(
        "<ListItemText primary={primary} ",
        "secondary={ <Chip size={\"small\"} variant={\"outlined\"} ",
        "color={longColorExpression} label={longLabelExpression} /> } ",
        "className=\"direct-chip-root\" dense disableTypography />\n",
    );
    let output = assert_idempotent("direct_chip.tsx", input, &cfg);

    // Level 1 — the enclosing tag's own attributes wrap onto >1 line.
    assert_component_wrapped("direct_chip.tsx", "ListItemText", &output);
    assert!(
        attribute_line_groups(&output, "ListItemText").len() > 1,
        "enclosing <ListItemText> must span multiple lines\n  output: {output:?}",
    );
    // Level 2 — the direct attribute-value <Chip> wraps.
    assert_component_wrapped("direct_chip.tsx", "Chip", &output);
    assert_contains_verbatim("direct_chip.tsx", "primary={primary}", &output);

    assert_structure_safe("direct_chip.tsx", input, &output, &cfg);
}

// ===========================================================================
// Attribute-value element with child-position descendants.
//
// bugfix 2.1, 2.2. `secondaryAction={ <Stack …><IconButton …/></Stack> }` —
// assert the enclosing tag, <Stack>, and <IconButton> each wrap (one
// independent rewrite per level, reusing child-position container discipline
// for the descendant).
//
// F' (FIXED): <Stack> and its child-position <IconButton> each wrap.
// ===========================================================================

#[test]
fn attribute_value_stack_with_child_descendant_formats_every_level() {
    let cfg = config(50, true, false);
    let input = concat!(
        "<Toolbar secondaryAction={ <Stack direction={\"row\"} spacing={1}>",
        "<IconButton loading={isLoading} onClick={onClick} color=\"primary\" /></Stack> } ",
        "className=\"toolbar-with-action\" />\n",
    );
    let output = assert_idempotent("av_stack_child.tsx", input, &cfg);

    // Level 1 — the enclosing <Toolbar> own attributes wrap onto >1 line.
    assert_component_wrapped("av_stack_child.tsx", "Toolbar", &output);
    assert!(
        attribute_line_groups(&output, "Toolbar").len() > 1,
        "enclosing <Toolbar> must span multiple lines\n  output: {output:?}",
    );
    // Level 2 — the attribute-value element <Stack> AND its child-position
    // descendant <IconButton> each wrap independently.
    assert_component_wrapped("av_stack_child.tsx", "Stack", &output);
    assert_component_wrapped("av_stack_child.tsx", "IconButton", &output);

    assert_structure_safe("av_stack_child.tsx", input, &output, &cfg);
}

// ===========================================================================
// Arbitrary depth — `a={ <X b={ <Y c={ <Z … /> } /> } />`.
//
// bugfix 2.1, 2.2. Assert every level formats — one independent recursive
// rewrite per genuine nested element.
//
// F' (FIXED): the enclosing <Root> own attributes wrap AND <X>, <Y>, <Z> each
// wrap on their own geometry (the engine descends into every attribute value
// at every depth, producing a correctly nested `children` tree).
// ===========================================================================

#[test]
fn arbitrary_depth_nested_elements_format_every_level() {
    let cfg = config(40, true, false);
    let input = concat!(
        "<Root first={firstValue} ",
        "a={ <X second={secondValue} ",
        "b={ <Y third={thirdValue} ",
        "c={ <Z fourth={fourthValue} fifth={fifthValue} /> }/> }/> } />\n",
    );
    let output = assert_idempotent("depth.tsx", input, &cfg);

    // Level 1 — the enclosing <Root> own attributes wrap onto >1 line.
    assert_component_wrapped("depth.tsx", "Root", &output);
    assert!(
        attribute_line_groups(&output, "Root").len() > 1,
        "enclosing <Root> must span multiple lines\n  output: {output:?}",
    );
    // Every nested level wraps — one independent rewrite per genuine element.
    for component in ["X", "Y", "Z"] {
        assert_component_wrapped("depth.tsx", component, &output);
    }

    assert_structure_safe("depth.tsx", input, &output, &cfg);
}

// ===========================================================================
// Both-levels observation.
//
// A focused check that BOTH the enclosing tag's OWN top-level attributes AND
// the nested attribute-value element wrap — documenting precisely that the
// two-level fix formats both levels. (At task 1 the nested <a> was a no-op;
// task 4.5 flips that expectation to wrapped.)
// ===========================================================================

#[test]
fn enclosing_tag_and_nested_element_both_wrap() {
    let cfg = config(80, true, false);
    let input = concat!(
        "<TaskStatisticListItem ",
        "key={`${studentTask.task}-${studentTask.user}`} ",
        "index={index} status={studentTask.status} ",
        "exists={existStudentTasks(studentTask)} ",
        "primary={ <a className=\"text-base text-blue-600 hover:text-blue-500 no-underline hover:underline\" ",
        "href={`/x?u=${studentTask.user}`} >{tasksMap[studentTask.task]?.title}</a> } ",
        "onRun={() => handleStatisticRunClick(studentTask.task, studentTask.user)} />\n",
    );
    let output = assert_idempotent("enclosing_obs.tsx", input, &cfg);

    // Enclosing tag: its own top-level attributes wrap onto multiple lines.
    assert_component_wrapped("enclosing_obs.tsx", "TaskStatisticListItem", &output);
    let groups = attribute_line_groups(&output, "TaskStatisticListItem");
    assert!(
        groups.len() > 1,
        "enclosing tag must span multiple lines\n  output: {output:?}",
    );

    // Nested <a>: wraps independently (the two-level fix formats both levels).
    assert_component_wrapped("enclosing_obs.tsx", "a", &output);

    assert_structure_safe("enclosing_obs.tsx", input, &output, &cfg);
}

// ===========================================================================
// Sorting-enabled two-level variant — bugfix 2.4; main 5.6.
//
// With Attribute_Sorting on, BOTH the enclosing tag's own attributes AND the
// nested attribute-value element's attributes are reordered by the `standard`
// scheme (id → class/className → other → event handlers), and each level is
// then fluid-wrapped. Behavior-affecting order is preserved (none of these
// tags carry duplicate names or a spread that overrides an earlier attribute).
// Finding 3: with sorting on, the child is still located at its correct
// rendered offset via the identity-keyed offset table — which this fixture
// exercises by reordering attributes on BOTH levels and still placing the
// nested element correctly.
// ===========================================================================

/// Index at which `needle` appears as a whole token within the start tag named
/// `component`, counting attribute tokens left-to-right across wrapped lines.
/// Used to assert sort order on a specific tag independent of line breaks.
fn attr_token_index(output: &str, component: &str, needle: &str) -> usize {
    let flat: Vec<String> = attribute_line_groups(output, component)
        .into_iter()
        .flatten()
        .collect();
    flat.iter()
        .position(|t| t == needle)
        .unwrap_or_else(|| panic!("token {needle:?} not found in <{component}> tokens {flat:?}"))
}

#[test]
fn sorting_enabled_sorts_and_wraps_both_levels() {
    let cfg = config(60, true, true);
    // Enclosing <ListItem>: source order onClick, id, secondary, className,
    // dense. `standard` moves id first, className before onClick, event handler
    // last. The nested <Chip> likewise: id, className before onClick.
    let input = concat!(
        "<ListItem onClick={handler} id=\"li-1\" ",
        "secondary={ <Chip onClick={chipClick} id=\"chip\" ",
        "className=\"chip-class\" label={longLabelExpression} variant={chipVariant} /> } ",
        "className=\"list-item-root\" dense />\n",
    );
    let output = assert_idempotent("sort_both.tsx", input, &cfg);

    // Level 1 — the enclosing tag wraps AND is sorted: id before className,
    // className before the onClick event handler. The enclosing tag's own
    // attributes bracket the nested `secondary={ <Chip … /> }` value, so assert
    // their order by rendered position in the whole output (the token helper
    // can't delimit the enclosing span past the inner element's `/>`).
    assert_component_wrapped("sort_both.tsx", "ListItem", &output);
    let pos_enc_id = output.find("id=\"li-1\"").expect("enclosing id present");
    let pos_enc_class =
        output.find("className=\"list-item-root\"").expect("enclosing className present");
    let pos_enc_onclick = output.find("onClick={handler}").expect("enclosing onClick present");
    assert!(
        pos_enc_id < pos_enc_class,
        "enclosing id must sort before className\n  output: {output:?}",
    );
    assert!(
        pos_enc_class < pos_enc_onclick,
        "enclosing className must sort before the onClick event handler\n  output: {output:?}",
    );

    // Level 2 — the nested <Chip> wraps AND is sorted the same way: id first,
    // className before onClick. (Finding 3: the child is located at its correct
    // rendered offset even though both levels reordered.)
    assert_component_wrapped("sort_both.tsx", "Chip", &output);
    assert!(
        attr_token_index(&output, "Chip", "id=\"chip\"")
            < attr_token_index(&output, "Chip", "className=\"chip-class\""),
        "nested id must sort before className\n  output: {output:?}",
    );
    assert!(
        attr_token_index(&output, "Chip", "className=\"chip-class\"")
            < attr_token_index(&output, "Chip", "onClick={chipClick}"),
        "nested className must sort before the onClick event handler\n  output: {output:?}",
    );

    // Non-element attribute values preserved byte-for-byte.
    assert_contains_verbatim("sort_both.tsx", "label={longLabelExpression}", &output);
    assert_contains_verbatim("sort_both.tsx", "variant={chipVariant}", &output);

    // Structure equivalence (sorting-aware multiset relation) + angle-bracket
    // conservation hold across the two-level sort+wrap.
    assert_structure_safe("sort_both.tsx", input, &output, &cfg);
}

// ===========================================================================
// In/out-of-container consistency — bugfix 2.5.
//
// A nested attribute-value element's wrapping SHAPE must not depend on sitting
// inside an attribute-value container. Place the SAME overflowing element both
// inside `prop={ … }` and outside any container at the SAME base column, and
// assert `attribute_line_groups` match. (`<Wrap prop={ ` is 13 columns wide, so
// the out-of-container copy is indented 13 spaces to share the base column.)
// ===========================================================================

#[test]
fn nested_element_wraps_identically_in_and_out_of_container() {
    let cfg = config(60, true, false);
    let tag = "<Banner title=\"welcome\" subtitle=\"hello there\" variant=\"info\" dismissible closable=\"yes\" />";

    // `<Wrap prop={ ` is 13 columns; the inner <Banner> therefore begins at
    // column 13. Match that exactly out of container with 13 leading spaces so
    // the comparison isolates the wrapping decision from the base-indent column.
    let in_container = format!("<Wrap prop={{ {tag} }} />\n");
    let out_of_container = format!("             {tag}\n");

    let wrapped_in = format_to_string("av_consistency_in.tsx", &in_container, &cfg);
    let wrapped_out = format_to_string("av_consistency_out.tsx", &out_of_container, &cfg);

    let groups_in = attribute_line_groups(&wrapped_in, "Banner");
    let groups_out = attribute_line_groups(&wrapped_out, "Banner");
    assert!(
        groups_out.len() > 1,
        "test premise: the tag must wrap at width 60\n{wrapped_out}",
    );
    assert_eq!(
        groups_in, groups_out,
        "a nested element must wrap identically in and out of an attribute-value container\nin:\n{wrapped_in}\nout:\n{wrapped_out}",
    );
}

// ===========================================================================
// Enclosing-tag consistency — bugfix 2.8 / Design Decision 0 (task 5).
//
// Carrying an element-bearing attribute value must NOT change how the enclosing
// tag's OWN top-level attributes wrap. Format the SAME enclosing tag — same
// element name, same base column, same set of top-level attributes in the same
// order — once WITH an element-bearing attribute value (`body={ <Chip … /> }`)
// and once with that attribute value replaced by a plain expression of
// equivalent rendered width (`body={plainBodyExpression______________}`), and
// assert the enclosing tag's own `attribute_line_groups` match.
//
// This is the direct assertion that Decision 0 (the `</` regex guard making an
// element-bearing value delimit confidently) leaves the enclosing tag
// confidently rewritable and wrapping exactly as it would for any other tag —
// the enclosing tag is NOT a no-op just because a nested element rides inside
// one of its attribute values.
//
// The nested element's inner `>`/`/>` would corrupt a naive whole-span token
// grouping (it closes the scan early and leaks the inner element's own tokens),
// so this check inspects the enclosing tag's wrap shape via the sequence of
// top-level attribute NAMES that BEGIN each line of the enclosing tag. That
// line-leading-attribute sequence is exactly the enclosing greedy-wrap decision
// (which attribute each continuation line starts with) and is unaffected by the
// bytes inside an attribute value. Both variants keep the SAME top-level
// attributes in the SAME order; the element-bearing variant uses a SHORT,
// single-line nested element so it does not itself wrap, and the plain
// placeholder is sized to the SAME rendered width as the element-bearing value,
// so the enclosing greedy-wrap decision is identical iff the fix leaves the
// enclosing tag confidently rewritable.
// ===========================================================================

/// The ordered sequence of top-level attribute NAMES that begin each line of
/// the enclosing start tag `component`, over the whole output. Captures the
/// enclosing greedy-wrap shape (which attribute each continuation line starts
/// with) without being confused by `>`/`/>` inside an attribute value: it keys
/// off the known top-level attribute names rather than tokenizing the whole
/// span. For each line, records the name whose `name=` / boolean form appears as
/// the first recognized top-level attribute on that line (after stripping an
/// optional leading `<Component` on the open line).
fn enclosing_line_leading_attrs(output: &str, component: &str, attr_names: &[&str]) -> Vec<String> {
    let open = format!("<{component}");
    output
        .lines()
        .filter_map(|line| {
            // On the tag's open line, skip the `<Component` prefix so the first
            // real top-level attribute is examined.
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

#[test]
fn enclosing_tag_wraps_identically_with_and_without_element_bearing_value() {
    let cfg = config(60, true, false);

    // A short, non-wrapping nested element so only the ENCLOSING tag's own
    // attribute layout is under test.
    let element_value = "body={ <Chip label=\"ok\" /> }";
    // A plain expression of the SAME rendered width as `element_value` (28
    // bytes, all ASCII), so the enclosing greedy-wrap decision is identical.
    let plain_value = "body={plain_body_expression}";
    assert_eq!(
        element_value.len(),
        plain_value.len(),
        "test premise: both `body` attribute values must have equal rendered width",
    );

    let with_element = format!(
        "<ListPanel header=\"welcome\" footer=\"goodbye\" variant=\"info\" {element_value} dense divider />\n",
    );
    let without_element = format!(
        "<ListPanel header=\"welcome\" footer=\"goodbye\" variant=\"info\" {plain_value} dense divider />\n",
    );

    let out_with = format_to_string("enc_with_element.tsx", &with_element, &cfg);
    let out_without = format_to_string("enc_without_element.tsx", &without_element, &cfg);

    let names = ["header", "footer", "variant", "body", "dense", "divider"];
    let leading_with = enclosing_line_leading_attrs(&out_with, "ListPanel", &names);
    let leading_without = enclosing_line_leading_attrs(&out_without, "ListPanel", &names);

    // Premise: the enclosing tag actually wraps (more than one line leads).
    assert!(
        out_without.lines().filter(|l| !l.trim().is_empty()).count() > 1,
        "test premise: the enclosing tag must wrap at width 60\n{out_without}",
    );
    assert_eq!(
        leading_with, leading_without,
        "an enclosing tag must wrap its own attributes identically whether or not an attribute value carries a nested element\nwith element:\n{out_with}\nwithout element:\n{out_without}",
    );

    // Both variants are safe rewrites (structure + angle-bracket conservation).
    assert_structure_safe("enc_with_element.tsx", &with_element, &out_with, &cfg);
    assert_structure_safe("enc_without_element.tsx", &without_element, &out_without, &cfg);
}

// ===========================================================================
// PRESERVATION BASELINE (task 3) — Property 2.
//
// Observation-first baseline captured on **task-1 code** (Design Decision 0 of
// tasks 1.1/1.2 applied; the children-tree fix of task 4 NOT applied), driving
// the PUBLIC `format_document` seam. Each fixture pins F's EXACT output so task
// 4.6 can prove `F'(X) = F(X)` byte-for-byte after the children-tree fix lands.
//
// Scope (per task 1.3): the `NOT isBugCondition` set only — inputs with NO
// `</`-bearing genuine element inside an attribute value. These inputs are
// unaffected by Decision 0 (its `</` regex guard only ever suppresses a
// spurious regex read for a `/` immediately after a code-position `<`, which
// none of these inputs contain), so they are byte-for-byte identical to the
// pre-fix plugin and must stay identical after task 4.
//
// Element-bearing enclosing tags (`primary={<a>…</a>}`, `secondary={<span>…`)
// are IN `isBugCondition` — they are the bug being fixed — and are
// INTENTIONALLY EXCLUDED here; their post-fix behavior (two-level formatting)
// is asserted by the flipped task-2 corpus at task 4.5, not by this
// byte-for-byte-vs-pre-fix invariant.
//
// This mirrors (at the integration seam) the crate-internal
// `attribute_value_preservation_baseline` module in `src/engine.rs` (task 1.3).
// The outputs below were recorded by observing `format_document` on task-1 code
// and are asserted exactly.
//
// bugfix 3.1, 3.2, 3.3, 3.4, 3.6, 3.9; main 3.6, 4.9, 7.4, 8.
// ===========================================================================

/// Pin F's observed output for `src` to exactly `expected` at the public
/// `format_document` seam, AND assert the pass is a byte-for-byte fixed point
/// (idempotence baseline, Requirement 8) and a safe no-op on structure +
/// angle-bracket conservation.
fn assert_preservation_baseline(
    file: &str,
    src: &str,
    cfg: &Configuration,
    expected: &str,
) {
    let first = format_to_string(file, src, cfg);
    assert_eq!(first, expected, "post-Decision-0 F baseline drifted for {src:?}");
    let second = format_to_string(file, &first, cfg);
    assert_eq!(
        second, first,
        "idempotence baseline failed: second pass differs for {src:?}",
    );
    assert_structure_equivalent(file, src, &first, cfg);
    assert_eq!(
        angle_count(src),
        angle_count(&first),
        "no `<`/`>` may be added or dropped for {src:?}",
    );
}

// ----- Generics / `<`-operators in attribute values (bugfix 3.1) ----------
//
// design Preservation Test Case 1. The enclosing <X> wraps its OWN attributes,
// but each generic / `<`-operator attribute value is preserved byte-for-byte:
// no nested child is recorded for it and the generics guard is NOT
// reintroduced-as-broken inside an attribute-value container.

#[test]
fn preservation_generic_arrow_attribute_value() {
    let src = "<X onClick={() => arr.map<T>(x => x)} a=\"1\" b=\"2\" c=\"3\" d=\"4\" e=\"5\" />";
    let expected = "<X onClick={() => arr.map<T>(x => x)}\n  a=\"1\" b=\"2\" c=\"3\" d=\"4\" e=\"5\" />";
    assert_preservation_baseline("gen_arrow.tsx", src, &config(40, true, false), expected);
}

#[test]
fn preservation_less_than_operator_attribute_value() {
    let src = "<X disabled={a < b} a=\"1\" b=\"2\" c=\"3\" d=\"4\" e=\"5\" f=\"6\" />";
    let expected = "<X disabled={a < b} a=\"1\" b=\"2\" c=\"3\"\n  d=\"4\" e=\"5\" f=\"6\" />";
    assert_preservation_baseline("lt_op.tsx", src, &config(40, true, false), expected);
}

#[test]
fn preservation_nested_generic_attribute_value() {
    let src = "<X value={useState<Array<Task>>()} a=\"1\" b=\"2\" c=\"3\" d=\"4\" />";
    let expected = "<X value={useState<Array<Task>>()} a=\"1\"\n  b=\"2\" c=\"3\" d=\"4\" />";
    assert_preservation_baseline("nested_gen.tsx", src, &config(40, true, false), expected);
}

#[test]
fn preservation_member_call_generic_attribute_value() {
    let src = "<X cfg={axios.get<Array<Url>>(u)} a=\"1\" b=\"2\" c=\"3\" d=\"4\" />";
    let expected = "<X cfg={axios.get<Array<Url>>(u)} a=\"1\"\n  b=\"2\" c=\"3\" d=\"4\" />";
    assert_preservation_baseline("member_gen.tsx", src, &config(40, true, false), expected);
}

// ----- Regex-literal attribute value (bugfix 3.9; Finding 1) ---------------
//
// A genuine regex literal is consumed by `delimit_braced`'s pre-existing regex
// scanner; the `</` guard only suppresses a regex read for a `/` IMMEDIATELY
// after a `<`, so a `/` not preceded by `<` is unaffected and the value
// delimits confidently and is preserved byte-for-byte.

#[test]
fn preservation_regex_literal_attribute_value() {
    let src = "<X test={re.test(/a}b/)} a=\"1\" b=\"2\" c=\"3\" d=\"4\" e=\"5\" />";
    let expected = "<X test={re.test(/a}b/)} a=\"1\" b=\"2\"\n  c=\"3\" d=\"4\" e=\"5\" />";
    assert_preservation_baseline("regex_val.tsx", src, &config(40, true, false), expected);
}

// ----- Non-element attribute content (design Preservation Test Case 2) -----
//
// The Example-2 template literals (key, href), the `exists={…}` call, the
// `onRun={() => …}` arrow, and the multiline `onDelete={… ? … : undefined}`
// ternary stay byte-for-byte unchanged — only whitespace BETWEEN the enclosing
// tag's top-level attributes may change.

#[test]
fn preservation_template_literal_key_attribute_value() {
    let src = "<X key={`${a.task}-${a.user}`} a=\"1\" b=\"2\" c=\"3\" d=\"4\" />";
    let expected = "<X key={`${a.task}-${a.user}`} a=\"1\"\n  b=\"2\" c=\"3\" d=\"4\" />";
    assert_preservation_baseline("tpl_key.tsx", src, &config(40, true, false), expected);
}

#[test]
fn preservation_arrow_call_attribute_value() {
    let src =
        "<X onRun={() => handleStatisticRunClick(studentTask.task, studentTask.user)} a=\"1\" b=\"2\" />";
    let expected =
        "<X onRun={() => handleStatisticRunClick(studentTask.task, studentTask.user)}\n  a=\"1\" b=\"2\" />";
    assert_preservation_baseline("arrow_call.tsx", src, &config(80, true, false), expected);
}

#[test]
fn preservation_multiline_ternary_attribute_value() {
    let src = concat!(
        "<X onDelete={ability.can(actionType.manage, subjectType.clazz) ",
        "? () => handleStatisticDeleteClick(studentTask.task, studentTask.user) ",
        ": undefined} a=\"1\" b=\"2\" />",
    );
    let expected = concat!(
        "<X onDelete={ability.can(actionType.manage, subjectType.clazz) ",
        "? () => handleStatisticDeleteClick(studentTask.task, studentTask.user) ",
        ": undefined}\n  a=\"1\" b=\"2\" />",
    );
    assert_preservation_baseline("ternary.tsx", src, &config(80, true, false), expected);
}

// ----- Ordinary attribute expressions (bugfix 3.2; Preservation Test Case 3)
//
// `primary={primary}`, `index={index}`, `onClick={onRun}`, `spacing={1}` stay
// byte-for-byte unchanged and tokenize as today.

#[test]
fn preservation_ordinary_expression_attribute_values() {
    let src = "<X primary={primary} index={index} onClick={onRun} spacing={1} />";
    let expected = "<X primary={primary} index={index}\n  onClick={onRun} spacing={1} />";
    assert_preservation_baseline("ordinary.tsx", src, &config(40, true, false), expected);
}

// ----- Child-container preservation (bugfix 3.3; Preservation Test Case 4) --
//
// The full child-position-container corpus lives in
// `tests/jsx_in_child_containers_corpora.rs` and MUST continue to pass
// unchanged after this spec's fix (that corpus is NOT duplicated here — this is
// a dependency note). The one focused fixture below mirrors the in-container
// `<Button>` from that corpus (and from the `src/engine.rs` task-1.3 baseline)
// so this file independently pins that the already-fixed child-position
// behavior is unaffected by Decision 0 and by the children-tree fix.

#[test]
fn preservation_child_position_container_unchanged() {
    let src = "<ul>\n  {cond && (\n    <Button variant=\"ghost\" size=\"sm\" onClick={p.go} disabled={p.busy} aria-label=\"next\" />\n  )}\n</ul>\n";
    let expected = "<ul>\n  {cond && (\n    <Button variant=\"ghost\" size=\"sm\"\n      onClick={p.go} disabled={p.busy}\n      aria-label=\"next\" />\n  )}\n</ul>\n";
    assert_preservation_baseline("child_container.tsx", src, &config(40, true, false), expected);
}

// ----- Component-with-generics head (bugfix 3.4; Preservation Test Case 5) --
//
// `<EditableList<SearchEngineDomain>` and `<Select<Array<number>> …>` stay
// not-confidently-rewritable byte-for-byte no-ops, both as the ENCLOSING tag
// and as a nested child inside an attribute-value container (where, at task 1,
// the enclosing tag still wraps its own attributes and the whole generics-head
// value is preserved verbatim because no descent is attempted yet).

#[test]
fn preservation_component_with_generics_head_enclosing_noop() {
    let src = "<EditableList<SearchEngineDomain> a=\"1\" b=\"2\" c=\"3\" d=\"4\" e=\"5\" />";
    assert_preservation_baseline("gen_head1.tsx", src, &config(40, true, false), src);
}

#[test]
fn preservation_component_with_nested_generics_head_enclosing_noop() {
    let src = "<Select<Array<number>> labelId=\"x\" value={x} onChange={on} className=\"w\" />";
    assert_preservation_baseline("gen_head2.tsx", src, &config(40, true, false), src);
}

#[test]
fn preservation_generics_head_nested_in_attribute_value() {
    // The nested component-with-generics head inside `item={ … }` is preserved
    // verbatim (no descent at task 1); the enclosing <Wrap> wraps its OWN
    // attributes. Task 4 keeps the head a not-confidently-rewritable no-op even
    // once descent is enabled (bugfix 3.4).
    let src = "<Wrap item={ <EditableList<SearchEngineDomain> a=\"1\" b=\"2\" /> } x=\"1\" y=\"2\" z=\"3\" />";
    let expected =
        "<Wrap item={ <EditableList<SearchEngineDomain> a=\"1\" b=\"2\" /> }\n  x=\"1\" y=\"2\" z=\"3\" />";
    assert_preservation_baseline("gen_head_nested.tsx", src, &config(40, true, false), expected);
}

// ----- Dynamic/spread + complex-expression (bugfix 3.9; Preservation 6) ----
//
// A pure spread `{...props}` is never an attribute value and is never probed —
// it tokenizes as DynamicSpread and is preserved byte-for-byte. A genuinely
// ambiguous braced value (an unresolvable lexical state — a `)` closing an open
// `[` — NOT merely a `</…>` tag, which Decision 0 resolves) poisons the whole
// enclosing tag, suppresses descent, and leaves it byte-for-byte unchanged.

#[test]
fn preservation_pure_spread_attribute() {
    let src = "<X {...props} a=\"1\" b=\"2\" c=\"3\" d=\"4\" e=\"5\" f=\"6\" />";
    let expected = "<X {...props} a=\"1\" b=\"2\" c=\"3\" d=\"4\"\n  e=\"5\" f=\"6\" />";
    assert_preservation_baseline("spread.tsx", src, &config(40, true, false), expected);
}

#[test]
fn preservation_ambiguous_complex_expression_poisons_tag_noop() {
    // `bad={ [a, b) }` — a `)` closing an open `[` is an unresolvable lexical
    // state, so the value is a genuine Complex_Expression_Attribute: the whole
    // tag is not-confidently-rewritable and stays a byte-for-byte no-op even at
    // width 40 (no attribute wraps).
    let src = "<X bad={ [a, b) } b=\"1\" c=\"2\" d=\"3\" e=\"4\" f=\"5\" g=\"6\" h=\"7\" />";
    assert_preservation_baseline("ambiguous.tsx", src, &config(40, true, false), src);
}

// ----- Other-dialect preservation (bugfix 3.6; Preservation Test Case 7) ---
//
// Representative HTML/Vue/Astro/Svelte start tags are unchanged — `children` is
// always empty for non-JSX/TSX dialects, so this JSX/TSX-only fix cannot touch
// them. (These fit on one line, so F == input; the point is that no dialect's
// scanner ever records an attribute-value child.)

#[test]
fn preservation_other_dialects_unchanged() {
    let cfg = config(80, true, false);
    let fixtures = [
        ("dialect.html", "<div class=\"a\" id=\"b\">x</div>"),
        ("dialect.vue", "<MyComp class=\"a\" id=\"b\" />"),
        ("dialect.astro", "<Comp class=\"a\" id=\"b\" />"),
        ("dialect.svelte", "<Comp class=\"a\" id=\"b\" />"),
    ];
    for (file, src) in fixtures {
        assert_preservation_baseline(file, src, &cfg, src);
    }
}
