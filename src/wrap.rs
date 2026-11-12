//! Greedy attribute wrapping (the fluid packer).
//!
//! This module implements the *layout decision* for fluid (greedy) attribute
//! wrapping (Design Decision 3, Requirements 4.1–4.5, 4.7). It is a pure,
//! side-effect-free computation: given a [`ScannedTag`], the resolved line width
//! `W`, and the indentation unit width, it decides whether the start tag fits on
//! one line and, if not, which attributes land on which continuation line.
//!
//! The packer deliberately stops at the layout decision. It does **not** emit
//! any bytes — turning a [`TagLayout`] into canonical start-tag text (indentation
//! whitespace, single-space separators, the canonical terminator) is the job of
//! `render_start_tag` in task 8.2. Keeping the two concerns apart means the
//! wrapping policy can be unit-tested purely in terms of attribute indices and
//! column math, independent of whitespace emission.
//!
//! ## Column model
//!
//! All columns are measured under the shared tab = `indent_width` model (see
//! [`crate::model::display_width`]), so the packer's geometry always agrees with
//! how `open_col`, `base_indent_col`, and attribute widths were computed. The
//! packer never re-measures bytes; it consumes the pre-computed
//! [`Attribute::width`](crate::model::Attribute::width) and the tag's column
//! fields.
//!
//! ## Key geometry
//!
//! - `W` — the resolved `line_width` budget.
//! - `open_col` — the display column of the `<` character, used for the
//!   single-line fit decision and to classify a tag as mid-line
//!   (`open_col != base_indent_col`).
//! - `base_indent_col` — the leading-whitespace column of the tag's physical
//!   line. Continuation indentation of a line-leading wrapped tag is derived
//!   from this (Requirement 4.4). A **mid-line** tag (one whose `<` is not the
//!   first non-whitespace character of its line) is never wrapped — see
//!   [`plan_layout`] — because no continuation indent measured from the current
//!   buffer is pass-invariant once a sibling on the same line wraps, so wrapping
//!   one could not be made idempotent (Requirement 8).
//! - `cont_indent` = `base_indent_col + indent_width` — the column at which every
//!   continuation line's first attribute starts.
//! - `name_prefix` = `1 + width(name) + 1` — the `<`, the element name, and the
//!   single space before the first attribute on the first line.
//! - terminator width — the canonical closing bracket width: `3` for a
//!   self-closing tag (` />`) and `1` for a non-self-closing tag (`>`).

use crate::model::{display_width, ScannedTag};

/// Canonical terminator width for a self-closing tag: one space + `/>`.
const SELF_CLOSING_TERMINATOR_WIDTH: usize = 3;
/// Canonical terminator width for a non-self-closing tag: `>`.
const PLAIN_TERMINATOR_WIDTH: usize = 1;

/// One line of a wrapped start tag, expressed as a half-open range of attribute
/// indices into the tag's `attributes` vector.
///
/// A line always contains at least one attribute (`start < end`); the packer
/// never emits an empty continuation line. Indices are stable positions in the
/// (possibly already sorted) attribute list the packer was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutLine {
    /// Index of the first attribute on this line (inclusive).
    pub start: usize,
    /// Index one past the last attribute on this line (exclusive).
    pub end: usize,
}

impl LayoutLine {
    /// Number of attributes on this line (always `>= 1`).
    #[must_use]
    pub fn len(self) -> usize {
        self.end - self.start
    }

    /// Whether this line has no attributes. The packer never produces such a
    /// line; the method exists to satisfy the clippy `len`-without-`is_empty`
    /// lint and for defensive callers.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.start >= self.end
    }
}

/// The computed layout of a start tag.
///
/// `render_start_tag` (task 8.2) consumes this to emit bytes: when
/// [`single_line`](TagLayout::single_line) is `true` it emits one line, otherwise
/// it emits [`lines`](TagLayout::lines) with the first line at the tag's original
/// indentation and every continuation line indented to
/// [`cont_indent`](TagLayout::cont_indent).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagLayout {
    /// `true` when the whole start tag fits on one line (or has zero attributes).
    /// In that case [`lines`](TagLayout::lines) holds a single line covering all
    /// attributes (possibly empty) and continuation geometry is irrelevant.
    pub single_line: bool,
    /// The lines, in order. The first line is the `<name ...` line; any further
    /// entries are continuation lines. Each line holds at least one attribute,
    /// except the single line of a zero-attribute tag which is empty.
    pub lines: Vec<LayoutLine>,
    /// The column at which continuation lines' attributes begin,
    /// `base_indent_col + indent_width`. Meaningful only when
    /// [`single_line`](TagLayout::single_line) is `false`.
    pub cont_indent: usize,
}

impl TagLayout {
    /// Number of lines in the layout.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
}

/// Canonical terminator width for `tag` under the shared column model.
fn terminator_width(tag: &ScannedTag) -> usize {
    if tag.self_closing {
        SELF_CLOSING_TERMINATOR_WIDTH
    } else {
        PLAIN_TERMINATOR_WIDTH
    }
}

/// Width of the first-line name prefix: `<` + element name + one trailing space.
fn name_prefix_width(tag: &ScannedTag, indent_width: usize) -> usize {
    // `<` (1) + name + separating space (1). The separating space is only
    // meaningful when the tag has attributes, but including it unconditionally
    // keeps the single-line budget conservative and matches `render_start_tag`,
    // which only emits the space when at least one attribute follows.
    1 + display_width(tag.name, indent_width) + 1
}

/// Compute the greedy fluid-wrapping layout for a start tag.
///
/// `line_width` is the resolved budget `W`; `indent_width` is one indentation
/// unit in columns (also the tab-expansion width in the shared column model).
///
/// The returned [`TagLayout`] describes *which* attributes land on *which* line;
/// it carries no bytes. See the module docs for the geometry.
///
/// # Algorithm
///
/// 1. **Single-line fit** (Requirements 4.1, 4.7): a zero-attribute tag is always
///    one line; otherwise the tag fits on one line when
///    `open_col + name_prefix + Σ(width + 1) + terminator ≤ W` (one space before
///    each attribute, including before the first, which sits after the
///    name-prefix space).
/// 2. **Greedy packing** (Requirements 4.2, 4.3, 4.5): otherwise, pack attributes
///    left to right. A new continuation line is started only when the current
///    line already holds at least one attribute and the next attribute would push
///    the line past `W`. This guarantees at least one attribute per line even
///    when a lone attribute (or `cont_indent` plus a lone oversized attribute)
///    exceeds `W`; such an over-width line is accepted as-is and re-packs to
///    itself, preserving idempotence.
#[must_use]
pub fn plan_layout(tag: &ScannedTag, line_width: usize, indent_width: usize) -> TagLayout {
    let w = line_width;
    let attr_count = tag.attributes.len();
    let name_prefix = name_prefix_width(tag, indent_width);
    let terminator = terminator_width(tag);
    let cont_indent = tag.base_indent_col + indent_width;

    // Mid-line tags are never wrapped (idempotence, Requirement 8). A start tag
    // whose `<` is NOT the first non-whitespace character of its physical line
    // (`open_col != base_indent_col`) sits after other content — for example a
    // JSX tag embedded mid-expression, or a sibling start tag that shares the
    // line. Its continuation indentation would have to be measured relative to
    // the physical line, but the only quantities available for that
    // (`base_indent_col`, `open_col`) are both computed from the current
    // buffer's newline landscape, which a sibling's wrapping shifts between
    // passes. There is therefore NO pass-invariant continuation indent for a
    // mid-line tag, so wrapping one cannot be made byte-for-byte idempotent.
    //
    // Rather than emit a layout that oscillates across passes (which the
    // `format_document` self-check would then surface as a Format_Error, leaving
    // the file unchanged), we keep a mid-line tag on a single line regardless of
    // width. This is also the correct behavior on the merits: this plugin is an
    // additive post-processor that runs AFTER the language formatter, so the
    // expression line a mid-line tag sits on is already laid out by the upstream
    // formatter that owns it; re-wrapping it is exactly the cross-plugin
    // oscillation Requirement 8.4 scopes out. Sorting (which happens before
    // render) still applies: the single line is emitted with the (possibly
    // reordered) attribute order and the canonical terminator.
    let is_mid_line = tag.open_col != tag.base_indent_col;

    // Zero attributes, or a mid-line tag: always a single line. A mid-line
    // single line covers every attribute (not the empty range a zero-attribute
    // tag uses).
    if attr_count == 0 {
        return TagLayout {
            single_line: true,
            lines: vec![LayoutLine { start: 0, end: 0 }],
            cont_indent,
        };
    }
    if is_mid_line {
        return TagLayout {
            single_line: true,
            lines: vec![LayoutLine { start: 0, end: attr_count }],
            cont_indent,
        };
    }

    // Single-line fit check (Requirements 4.1, 4.7). Each attribute is preceded
    // by exactly one space: the first by the name-prefix space, the rest by their
    // inter-attribute separator. So the attribute contribution is `Σ(width + 1)`.
    let attrs_single_line: usize = tag
        .attributes
        .iter()
        .map(|attr| attr.width + 1)
        .sum();
    let single_line_width = tag.open_col + name_prefix + attrs_single_line + terminator;
    if single_line_width <= w {
        return TagLayout {
            single_line: true,
            lines: vec![LayoutLine { start: 0, end: attr_count }],
            cont_indent,
        };
    }

    // Greedy packing. The first line's attributes begin at
    // `open_col + name_prefix`; continuation lines' attributes begin at
    // `cont_indent`.
    let mut lines: Vec<LayoutLine> = Vec::new();
    let mut line_start = 0usize;
    let mut current = tag.open_col + name_prefix; // column cursor for the open line
    let mut line_has_attr = false;

    for (idx, attr) in tag.attributes.iter().enumerate() {
        let sep = usize::from(line_has_attr); // single space only between attributes
        let projected = current + sep + attr.width;

        if line_has_attr && projected > w {
            // Flush the current line and start a continuation line. The
            // at-least-one-attribute-per-line guard (`line_has_attr`) means we
            // never break before the first attribute of a line, so an oversized
            // lone attribute still lands on its own line (Requirements 4.3, 4.5).
            lines.push(LayoutLine { start: line_start, end: idx });
            line_start = idx;
            current = cont_indent + attr.width;
            // `line_has_attr` stays true: the new line now holds this attribute.
        } else {
            current = projected;
            line_has_attr = true;
        }
    }

    // Flush the final open line.
    lines.push(LayoutLine { start: line_start, end: attr_count });

    TagLayout {
        single_line: false,
        lines,
        cont_indent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Attribute, AttributeKind};

    /// Build a `NameValue` attribute of a given raw text at `indent_width = 2`.
    fn attr(raw: &'static str) -> Attribute<'static> {
        Attribute {
            kind: AttributeKind::NameValue,
            name_key: None,
            raw,
            width: display_width(raw, 2),
        }
    }

    /// Build a tag with the given geometry and attributes.
    fn tag<'a>(
        name: &'a str,
        self_closing: bool,
        open_col: usize,
        base_indent_col: usize,
        attributes: Vec<Attribute<'a>>,
    ) -> ScannedTag<'a> {
        ScannedTag {
            span: 0..1,
            name,
            self_closing,
            attributes,
            confidently_rewritable: true,
            open_col,
            base_indent_col,
        }
    }

    #[test]
    fn zero_attribute_tag_is_single_line() {
        // A tag with no attributes is always one line, regardless of width
        // (Requirement 4.7).
        let t = tag("br", true, 0, 0, vec![]);
        let layout = plan_layout(&t, 80, 2);
        assert!(layout.single_line);
        assert_eq!(layout.line_count(), 1);
        assert_eq!(layout.lines[0], LayoutLine { start: 0, end: 0 });
        assert_eq!(layout.lines[0].len(), 0);
        assert!(layout.lines[0].is_empty());

        // Even a tiny width keeps a zero-attribute tag on one line.
        let narrow = plan_layout(&t, 1, 2);
        assert!(narrow.single_line);
    }

    #[test]
    fn single_line_when_it_fits() {
        // `<div class="a" id="b">` at open_col 0.
        // name_prefix = 1 + 3 + 1 = 5; attrs = (9+1) + (6+1) = 17; terminator = 1.
        // total = 0 + 5 + 17 + 1 = 23 <= 80 -> one line (Requirement 4.1).
        let t = tag(
            "div",
            false,
            0,
            0,
            vec![attr("class=\"a\""), attr("id=\"b\"")],
        );
        let layout = plan_layout(&t, 80, 2);
        assert!(layout.single_line);
        assert_eq!(layout.line_count(), 1);
        assert_eq!(layout.lines[0], LayoutLine { start: 0, end: 2 });
    }

    #[test]
    fn exact_boundary_fits_on_one_line() {
        // Construct a tag whose single-line width is exactly W, asserting the
        // `<= W` boundary keeps it on one line (Requirement 4.1).
        // name_prefix(div) = 5, terminator(non-self-closing) = 1.
        // Choose one attribute of width `k`: total = 0 + 5 + (k + 1) + 1 = k + 7.
        // For W = 20 pick k = 13 so total == 20.
        let a = attr("data-x=\"01234\""); // width: data-x="01234" = 14? verify below
        // Recompute precisely instead of guessing widths.
        let k = a.width;
        // open_col(0) + name_prefix(5) + (k + separator space) + terminator(1).
        let w = 5 + (k + 1) + 1;
        let t = tag("div", false, 0, 0, vec![a]);
        let at_boundary = plan_layout(&t, w, 2);
        assert!(at_boundary.single_line, "width == W must stay single line");

        // One column tighter forces a wrap (but a single attribute cannot split,
        // so it is one continuation line holding the lone attribute).
        let over = plan_layout(&t, w - 1, 2);
        assert!(!over.single_line);
    }

    #[test]
    fn oversized_single_attribute_gets_its_own_line() {
        // A single attribute wider than W is never split; it lands on one line
        // even though that line exceeds W (Requirements 4.3, 4.5).
        let big = attr("data-very-long-attribute=\"xxxxxxxxxxxxxxxxxxxxxxxxxx\"");
        assert!(big.width > 20, "precondition: attribute wider than W");
        let t = tag("div", false, 0, 0, vec![big]);
        let layout = plan_layout(&t, 20, 2);
        assert!(!layout.single_line);
        assert_eq!(layout.line_count(), 1);
        assert_eq!(layout.lines[0].len(), 1);
    }

    #[test]
    fn multi_line_greedy_packing_is_maximal() {
        // Four attributes of width 8 each ("a1=\"xxx\"" = 8 columns).
        // W = 24, open_col = 0, base_indent = 0, indent_width = 2.
        // name_prefix(p) = 1 + 1 + 1 = 3 (name "p").
        // Single-line = 3 + (8+1)*4 + 1 = 40 > 24 -> wrap.
        // Open line cursor starts at 3.
        //  a0: sep 0 -> 3 + 8 = 11 (<=24) place.
        //  a1: sep 1 -> 11 + 1 + 8 = 20 (<=24) place.
        //  a2: sep 1 -> 20 + 1 + 8 = 29 (>24) -> new line at cont_indent=2;
        //       cursor = 2 + 8 = 10.
        //  a3: sep 1 -> 10 + 1 + 8 = 19 (<=24) place.
        // => line 0: [0,2), line 1: [2,4).
        let attrs = vec![attr("a0=\"xxx\""), attr("a1=\"xxx\""), attr("a2=\"xxx\""), attr("a3=\"xxx\"")];
        for a in &attrs {
            assert_eq!(a.width, 8);
        }
        let t = tag("p", false, 0, 0, attrs);
        let layout = plan_layout(&t, 24, 2);
        assert!(!layout.single_line);
        assert_eq!(layout.line_count(), 2);
        assert_eq!(layout.lines[0], LayoutLine { start: 0, end: 2 });
        assert_eq!(layout.lines[1], LayoutLine { start: 2, end: 4 });
        // Maximality: adding a2 to line 0 would have exceeded W.
    }

    #[test]
    fn three_line_wrap() {
        // Six width-8 attributes, same geometry as above but more of them, so the
        // packer produces three lines of two attributes each.
        let attrs: Vec<Attribute> = (0..6)
            .map(|_| attr("ax=\"xxx\"")) // width 8
            .collect();
        for a in &attrs {
            assert_eq!(a.width, 8);
        }
        let t = tag("p", false, 0, 0, attrs);
        let layout = plan_layout(&t, 24, 2);
        assert!(!layout.single_line);
        // open line holds 2 (cursor 3->11->20), continuation lines hold 2 each
        // (cursor 2->10->19). 6 attrs / 2 per line = 3 lines.
        assert_eq!(layout.line_count(), 3);
        assert_eq!(layout.lines[0], LayoutLine { start: 0, end: 2 });
        assert_eq!(layout.lines[1], LayoutLine { start: 2, end: 4 });
        assert_eq!(layout.lines[2], LayoutLine { start: 4, end: 6 });
        // Every line holds at least one attribute (Requirement 4.3).
        assert!(layout.lines.iter().all(|l| !l.is_empty()));
    }

    #[test]
    fn mid_line_tag_is_never_wrapped() {
        // A tag embedded mid-expression: `const x = <div a b c>` style, where
        // `<` sits at a large column (open_col) while the physical line's own
        // indentation (base_indent_col) is small, so open_col != base_indent_col.
        // Such a mid-line tag is NEVER wrapped, regardless of width, because no
        // continuation indent measured from the buffer is pass-invariant once a
        // sibling on its line wraps — so wrapping it could not be idempotent
        // (Requirement 8). It is kept on one line covering every attribute;
        // sorting (applied before render) is unaffected.
        let open_col = 50;
        let base_indent_col = 4;
        let indent_width = 2;
        let attrs = vec![attr("alpha=\"1\""), attr("bravo=\"2\""), attr("charlie=\"3\"")];
        let t = tag("div", false, open_col, base_indent_col, attrs);
        // Even a width far smaller than the tag keeps a mid-line tag single-line.
        let layout = plan_layout(&t, 40, indent_width);
        assert!(layout.single_line, "a mid-line tag must never wrap");
        assert_eq!(layout.line_count(), 1);
        assert_eq!(layout.lines[0], LayoutLine { start: 0, end: 3 });

        // Tightening the width further does not change the decision.
        let narrow = plan_layout(&t, 1, indent_width);
        assert!(narrow.single_line, "a mid-line tag stays single-line at any width");
    }

    #[test]
    fn line_leading_tag_with_same_cols_still_wraps() {
        // The mid-line guard keys on open_col != base_indent_col. A line-leading
        // tag (open_col == base_indent_col) is unaffected and wraps normally when
        // it overflows, confirming the guard does not over-trigger.
        let attrs = vec![
            attr("a0=\"xxx\""),
            attr("a1=\"xxx\""),
            attr("a2=\"xxx\""),
            attr("a3=\"xxx\""),
        ];
        let t = tag("p", false, 4, 4, attrs); // open_col == base_indent_col == 4
        let layout = plan_layout(&t, 24, 2);
        assert!(!layout.single_line, "a line-leading overflowing tag must still wrap");
    }

    #[test]
    fn single_line_decision_accounts_for_open_col() {
        // The single-line fit decision uses open_col (the tag's true start
        // column). The same attributes that fit at open_col = 0 must wrap when
        // open_col is large, even though cont_indent stays bounded.
        // Both tags are LINE-LEADING (open_col == base_indent_col) so both are
        // eligible to wrap; the only difference is the column at which the line
        // begins. The single-line fit decision must account for that column: the
        // same attributes fit when the line starts at column 0 but overflow when
        // the line is deeply indented to column 60.
        let attrs = vec![attr("class=\"a\""), attr("id=\"b\"")];
        let at_start = tag("div", false, 0, 0, attrs.clone());
        let deep_indent = tag("div", false, 60, 60, attrs);
        // W = 30: fits at column 0, overflows at column 60.
        let a = plan_layout(&at_start, 30, 2);
        let b = plan_layout(&deep_indent, 30, 2);
        assert!(a.single_line);
        assert!(!b.single_line);
    }

    #[test]
    fn self_closing_terminator_width_is_three() {
        // The canonical self-closing terminator ` />` is width 3; a tag that
        // would fit with `>` (width 1) but not with ` />` must wrap. Build a tag
        // whose single-line width with `>` is exactly W but 2 more with ` />`.
        let a = attr("x=\"1\""); // width 5
        // name_prefix(i) = 1 + 1 + 1 = 3. attrs = 5 + 1 = 6.
        // non-self-closing width = 0 + 3 + 6 + 1 = 10.
        let w = 10;
        let non_self = tag("i", false, 0, 0, vec![a.clone()]);
        assert!(plan_layout(&non_self, w, 2).single_line);
        // self-closing width = 0 + 3 + 6 + 3 = 12 > 10 -> wraps.
        let self_closing = tag("i", true, 0, 0, vec![a]);
        assert!(!plan_layout(&self_closing, w, 2).single_line);
    }

    #[test]
    fn oversized_attr_on_continuation_line_still_alone() {
        // Even when cont_indent + width(attr) exceeds W, a lone oversized
        // attribute is emitted on its own continuation line (Requirements 4.3,
        // 4.5). Two attributes: a small one then a huge one.
        let small = attr("a=\"1\""); // width 5
        let huge = attr("data-huge=\"yyyyyyyyyyyyyyyyyyyyyyyyyyyyyy\"");
        let huge_width = huge.width;
        assert!(huge_width > 20);
        // Line-leading tag (open_col == base_indent_col == 2) so it is eligible
        // to wrap; cont_indent is base_indent_col + indent_width = 4.
        let t = tag("div", false, 2, 2, vec![small, huge]);
        let layout = plan_layout(&t, 20, 2);
        assert!(!layout.single_line);
        // The huge attribute must be alone on its continuation line.
        assert_eq!(layout.line_count(), 2);
        assert_eq!(layout.lines[0], LayoutLine { start: 0, end: 1 });
        assert_eq!(layout.lines[1], LayoutLine { start: 1, end: 2 });
        // cont_indent + huge width exceeds W, but the line is still emitted.
        assert!(layout.cont_indent + huge_width > 20);
    }

    #[test]
    fn all_attributes_are_covered_exactly_once() {
        // The union of line ranges must cover every attribute index exactly once,
        // in order, with no gaps or overlaps (attribute-set preservation at the
        // layout level).
        let attrs: Vec<Attribute> = (0..7).map(|_| attr("ax=\"xxx\"")).collect();
        let n = attrs.len();
        let t = tag("section", false, 0, 0, attrs);
        let layout = plan_layout(&t, 24, 2);
        let mut expected = 0usize;
        for line in &layout.lines {
            assert_eq!(line.start, expected, "lines must be contiguous");
            assert!(line.end > line.start, "no empty lines");
            expected = line.end;
        }
        assert_eq!(expected, n, "lines must cover all attributes");
    }
}

#[cfg(test)]
mod single_line_property_tests {
    //! Property 5: single-line tags that fit are kept on one line.
    //!
    //! This suite checks the single-line fit decision of [`plan_layout`] against
    //! an independent re-computation of the canonical single-line width. It lives
    //! in its own module (not `mod tests`) to avoid colliding with the example
    //! unit tests above while sharing the same production code under test.

    use super::*;
    use crate::model::{Attribute, AttributeKind};
    use proptest::prelude::*;

    /// A fixed indentation unit (and tab-expansion width) for the whole suite.
    /// The single-line fit decision is independent of `indent_width`, so pinning
    /// it keeps the generators and the reference width calculation simple while
    /// still exercising the real column model.
    const INDENT_WIDTH: usize = 2;

    /// The canonical single-line width of `tag`, recomputed independently of the
    /// production packer:
    /// `open_col + name_prefix(1 + name_width + 1) + Σ(width + 1) + terminator`,
    /// where the terminator is `3` for a self-closing tag (` />`) and `1`
    /// otherwise (`>`). This mirrors the design's Property 5 statement and the
    /// `plan_layout` geometry without reusing its helpers.
    fn reference_single_line_width(tag: &ScannedTag) -> usize {
        let name_prefix = 1 + display_width(tag.name, INDENT_WIDTH) + 1;
        let attrs: usize = tag.attributes.iter().map(|a| a.width + 1).sum();
        let terminator = if tag.self_closing { 3 } else { 1 };
        tag.open_col + name_prefix + attrs + terminator
    }

    /// A single generated attribute: an ASCII token whose display width is the
    /// authoritative `display_width` of its raw bytes under the shared column
    /// model. Kinds are varied to exercise every `AttributeKind`, but the fit
    /// decision only depends on `width`, so the raw text is kept simple and
    /// collision-free.
    fn attr_strategy() -> impl Strategy<Value = Attribute<'static>> {
        // Leak short owned strings so the attribute can borrow a `'static` slice,
        // which keeps `ScannedTag<'static>` easy to build inside the property.
        (prop::sample::select(vec![
            AttributeKind::Plain,
            AttributeKind::NameValue,
            AttributeKind::DynamicSpread,
        ]), "[a-z][a-z0-9-]{0,11}")
            .prop_map(|(kind, raw)| {
                let leaked: &'static str = Box::leak(raw.into_boxed_str());
                Attribute {
                    kind,
                    name_key: None,
                    raw: leaked,
                    width: display_width(leaked, INDENT_WIDTH),
                }
            })
    }

    /// A generated `ScannedTag` with random name, self-closing flag, geometry,
    /// and 1..=8 attributes. `base_indent_col` is irrelevant to the single-line
    /// decision but is set to a plausible value.
    ///
    /// The attribute count is deliberately `>= 1`: zero-attribute tags are always
    /// single-line regardless of width (Requirement 4.7), so they would break the
    /// `single_line == (width <= line_width)` biconditional this strategy feeds.
    /// That zero-attribute guarantee is pinned separately by
    /// [`zero_attribute_tag_is_always_single_line`].
    ///
    /// The generated tags are **line-leading** (`open_col == base_indent_col`):
    /// that is the only class whose single-line decision is purely a width
    /// comparison. A mid-line tag (`open_col != base_indent_col`) is always kept
    /// single-line regardless of width (for idempotence), which would break the
    /// `single_line == (width <= line_width)` biconditional this strategy feeds;
    /// the mid-line rule is pinned separately by `mid_line_tag_is_never_wrapped`.
    fn tag_strategy() -> impl Strategy<Value = ScannedTag<'static>> {
        (
            "[a-z][a-z0-9]{0,7}",
            any::<bool>(),
            0usize..60,
            prop::collection::vec(attr_strategy(), 1..=8),
        )
            .prop_map(|(name, self_closing, col, attributes)| {
                let name: &'static str = Box::leak(name.into_boxed_str());
                ScannedTag {
                    span: 0..1,
                    name,
                    self_closing,
                    attributes,
                    confidently_rewritable: true,
                    // Line-leading: `<` is the first non-whitespace char, so its
                    // column equals the line's indentation.
                    open_col: col,
                    base_indent_col: col,
                }
            })
    }

    proptest! {
        /// Property 5 (Validates: Requirements 4.1, 4.7).
        ///
        /// For any generated start tag and line width, `plan_layout` keeps the
        /// tag on a single line exactly when its canonical single-line width is
        /// `<= line_width`. Zero-attribute tags are always single-line (4.7),
        /// which this biconditional also captures because their reference width
        /// is small — but a dedicated case below pins that guarantee for every
        /// width, including widths narrower than the empty tag itself.
        #[test]
        fn single_line_iff_fits(tag in tag_strategy(), line_width in 1usize..120) {
            let expected_fits = reference_single_line_width(&tag) <= line_width;
            let layout = plan_layout(&tag, line_width, INDENT_WIDTH);
            prop_assert_eq!(
                layout.single_line,
                expected_fits,
                "single_line must equal (single_line_width <= line_width); \
                 open_col={}, self_closing={}, attrs={}, ref_width={}, W={}",
                tag.open_col,
                tag.self_closing,
                tag.attributes.len(),
                reference_single_line_width(&tag),
                line_width
            );
            // When kept on one line, the layout is a single line covering every
            // attribute (Requirement 4.1).
            if layout.single_line {
                prop_assert_eq!(layout.line_count(), 1);
                prop_assert_eq!(layout.lines[0].start, 0);
                prop_assert_eq!(layout.lines[0].end, tag.attributes.len());
            }
        }

        /// Property 5, zero-attribute clause (Requirement 4.7).
        ///
        /// A start tag with no attributes is rendered on a single line for every
        /// line width, even widths too small to hold the tag itself.
        #[test]
        fn zero_attribute_tag_is_always_single_line(
            name in "[a-z][a-z0-9]{0,7}",
            self_closing in any::<bool>(),
            open_col in 0usize..60,
            line_width in 1usize..120,
        ) {
            let name: &'static str = Box::leak(name.into_boxed_str());
            let tag = ScannedTag {
                span: 0..1,
                name,
                self_closing,
                attributes: Vec::new(),
                confidently_rewritable: true,
                open_col,
                base_indent_col: 0,
            };
            let layout = plan_layout(&tag, line_width, INDENT_WIDTH);
            prop_assert!(layout.single_line, "zero-attribute tag must stay single line");
            prop_assert_eq!(layout.line_count(), 1);
            prop_assert!(layout.lines[0].is_empty());
        }
    }
}

#[cfg(test)]
mod greedy_maximality_property_tests {
    //! Property 6: greedy wrapping is maximal with at least one attribute per
    //! line.
    //!
    //! This suite exercises [`plan_layout`] over randomly generated start tags
    //! and asserts the three facets of greedy fluid wrapping on every layout that
    //! actually wrapped (`single_line == false`):
    //!
    //! - **At least one per line (Requirement 4.3).** Every emitted line holds at
    //!   least one attribute; the packer never produces an empty continuation
    //!   line.
    //! - **Greedy maximality (Requirement 4.2).** For every line except the last,
    //!   appending the first attribute of the *next* line to the current line
    //!   would push the current line past the line width `W` — i.e. the packer
    //!   packed as many attributes as could fit, never leaving room it could have
    //!   used. The sole exception is a line already holding exactly one attribute
    //!   that by itself overflows `W` (see below).
    //! - **Oversized lone attribute (Requirement 4.5).** A single attribute whose
    //!   own width places its line past `W` still lands on its own line, never
    //!   split or truncated; such a line is permitted to exceed `W`.
    //!
    //! All column arithmetic mirrors [`plan_layout`]'s exactly under the shared
    //! tab = `indent_width` model: the first (open) line's attributes begin at
    //! `open_col + name_prefix` where `name_prefix = 1 + width(name) + 1`, every
    //! continuation line's attributes begin at `cont_indent = base_indent_col +
    //! indent_width`, and attributes on a line are joined with single-space
    //! separators, so a line that starts at column `c0` and holds attribute widths
    //! `w[start..end]` occupies `c0 + Σ w + (count - 1)` columns. The terminator
    //! width only affects whether the whole tag fits on one line (the single-line
    //! decision), not per-line maximality, which is driven by attribute widths and
    //! separators alone — matching `plan_layout`.

    use super::*;
    use crate::model::{Attribute, AttributeKind};
    use proptest::prelude::*;

    /// Canonical self-closing terminator width (` />`), mirrored from
    /// [`super::SELF_CLOSING_TERMINATOR_WIDTH`] for the single-line reference.
    const SELF_CLOSING_TERMINATOR: usize = 3;
    /// Canonical non-self-closing terminator width (`>`).
    const PLAIN_TERMINATOR: usize = 1;

    /// A generated start-tag shape: everything `plan_layout` needs, with
    /// attribute *widths* generated directly (the raw text is irrelevant to the
    /// packer, which consumes pre-computed widths only).
    #[derive(Clone, Debug)]
    struct TagShape {
        name: String,
        self_closing: bool,
        open_col: usize,
        base_indent_col: usize,
        indent_width: usize,
        line_width: usize,
        attr_widths: Vec<usize>,
    }

    /// Build the attribute `raw` filler strings for a [`TagShape`]. Each filler
    /// is an ASCII string whose display width equals the generated width (ASCII
    /// keeps `display_width == len`), so the tag handed to `plan_layout` is
    /// byte-consistent with the widths the reference arithmetic uses. The caller
    /// owns these strings; the borrowed [`ScannedTag`] built from them must not
    /// outlive this vector (hence it is constructed in the test body).
    fn raws_from_shape(shape: &TagShape) -> Vec<String> {
        shape.attr_widths.iter().map(|&w| "a".repeat(w)).collect()
    }

    /// Build a [`ScannedTag`] borrowing from `name` and the `raws` fillers.
    fn tag_from_raws<'a>(shape: &'a TagShape, raws: &'a [String]) -> ScannedTag<'a> {
        let attributes = raws
            .iter()
            .zip(&shape.attr_widths)
            .map(|(raw, &w)| Attribute {
                kind: AttributeKind::NameValue,
                name_key: None,
                raw: raw.as_str(),
                width: w,
            })
            .collect();
        ScannedTag {
            span: 0..1,
            name: shape.name.as_str(),
            self_closing: shape.self_closing,
            attributes,
            confidently_rewritable: true,
            open_col: shape.open_col,
            base_indent_col: shape.base_indent_col,
        }
    }

    /// Column the first attribute of a given line occupies, mirroring
    /// `plan_layout`: the open line (index 0) starts at `open_col + name_prefix`;
    /// every continuation line starts at `cont_indent`.
    fn line_start_col(shape: &TagShape, line_index: usize) -> usize {
        let name_prefix = 1 + shape.name.chars().count() + 1; // ASCII name
        if line_index == 0 {
            shape.open_col + name_prefix
        } else {
            shape.base_indent_col + shape.indent_width
        }
    }

    /// End column (exclusive cursor) of a line holding `attr_widths[start..end]`
    /// beginning at `start_col`, with single-space separators between attributes:
    /// `start_col + Σ width + (count - 1)`.
    fn line_end_col(start_col: usize, widths: &[usize]) -> usize {
        debug_assert_ne!(widths.len(), 0, "a packed line always holds >= 1 attribute");
        let sum: usize = widths.iter().sum();
        start_col + sum + (widths.len() - 1)
    }

    prop_compose! {
        /// Generate a start-tag shape biased toward overflowing layouts: small
        /// line widths, several attributes, and a mix of ordinary and
        /// occasionally oversized attribute widths so both the maximality facet
        /// and the lone-oversized-attribute facet get exercised.
        //
        // The generated tags are **line-leading** (`open_col == base_indent_col`),
        // because that is the only class `plan_layout` ever wraps: a mid-line tag
        // (`open_col != base_indent_col`) is kept single-line for idempotence, so
        // generating one here would only be skipped by the `!single_line`
        // assumption below and waste cases. Line-leading tags at a range of
        // indentation columns still exercise the full packing geometry.
        fn tag_shape()(
            name_len in 1usize..6,
            self_closing in any::<bool>(),
            base_indent_col in 0usize..40,
            indent_width in 1usize..5,
            line_width in 8usize..40,
            attr_widths in prop::collection::vec(1usize..30, 1..9),
        ) -> TagShape {
            TagShape {
                name: "d".repeat(name_len),
                self_closing,
                // Line-leading: the `<` is the first non-whitespace char of its
                // line, so its display column equals the line's indentation.
                open_col: base_indent_col,
                base_indent_col,
                indent_width,
                line_width,
                attr_widths,
            }
        }
    }

    proptest! {
        // Feature: fluid-attribute-wrapping-plugin, Property 6: Greedy wrapping is maximal with at least one attribute per line

        /// Property 6 (Validates: Requirements 4.2, 4.3, 4.5).
        ///
        /// For every generated start tag that wraps, assert the three facets of
        /// greedy fluid wrapping: at-least-one-per-line, greedy maximality, and
        /// the oversized-lone-attribute allowance.
        #[test]
        fn greedy_wrapping_is_maximal_with_one_per_line(shape in tag_shape()) {
            let raws = raws_from_shape(&shape);
            let tag = tag_from_raws(&shape, &raws);
            let w = shape.line_width;
            let layout = plan_layout(&tag, w, shape.indent_width);

            // This property only constrains layouts that actually wrapped. A
            // single-line layout is Property 5's concern; skip it here.
            prop_assume!(!layout.single_line);

            // The layout must have at least one line, and the packed ranges must
            // cover every attribute contiguously in order.
            prop_assert!(!layout.lines.is_empty(), "wrapped layout has no lines");
            let n = shape.attr_widths.len();
            let mut expected_start = 0usize;
            for line in &layout.lines {
                prop_assert_eq!(line.start, expected_start, "lines are not contiguous");
                // Facet (a) — at least one attribute per line (Requirement 4.3).
                prop_assert!(line.end > line.start, "empty line emitted");
                expected_start = line.end;
            }
            prop_assert_eq!(expected_start, n, "lines do not cover all attributes");

            // Walk each line and check the two width-driven facets.
            for (i, line) in layout.lines.iter().enumerate() {
                let start_col = line_start_col(&shape, i);
                let widths = &shape.attr_widths[line.start..line.end];
                let end_col = line_end_col(start_col, widths);

                let is_last = i + 1 == layout.lines.len();
                let count = line.end - line.start;

                if is_last {
                    // The last line carries the remaining attributes; maximality
                    // has no "next attribute" to test against. Nothing to assert
                    // beyond the non-empty check already done above.
                    continue;
                }

                // There is a next line; its first attribute is at index `line.end`.
                let next_width = shape.attr_widths[line.end];

                if count == 1 && end_col > w {
                    // Facet (c) — oversized lone attribute (Requirement 4.5).
                    // This line holds exactly one attribute that by itself
                    // overflows `W`. Greedy maximality does not apply: the packer
                    // was forced to break after a single over-width attribute, so
                    // we do not require that the next attribute would overflow.
                    // We only require it genuinely stands alone and is not split.
                    prop_assert_eq!(
                        count, 1,
                        "oversized attribute must occupy its line alone"
                    );
                } else {
                    // Facet (b) — greedy maximality (Requirement 4.2). The current
                    // line is within `W`, so appending the next line's first
                    // attribute (with its separating space) MUST exceed `W`;
                    // otherwise the packer left usable room and was not maximal.
                    prop_assert!(
                        end_col <= w,
                        "a non-oversized packed line must fit within W: \
                         end_col={end_col} W={w} shape={shape:?}"
                    );
                    let with_next = end_col + 1 + next_width;
                    prop_assert!(
                        with_next > w,
                        "line {i} is not maximal: adding the next attribute \
                         (width {next_width}) stays within W \
                         (would end at {with_next} <= {w}); shape={shape:?}"
                    );
                }
            }
        }

        /// Property 6 supporting facet: the single-line decision and the wrap
        /// decision are consistent. When `plan_layout` reports a wrapped layout,
        /// the tag genuinely does not fit on one line under the same column model
        /// the packer uses (open_col + name_prefix + Σ(width+1) + terminator > W).
        /// This pins the precondition of Property 6 ("start tags that overflow
        /// the line width") to the packer's own fit arithmetic.
        #[test]
        fn wrapped_layouts_genuinely_overflow(shape in tag_shape()) {
            let raws = raws_from_shape(&shape);
            let tag = tag_from_raws(&shape, &raws);
            let w = shape.line_width;
            let layout = plan_layout(&tag, w, shape.indent_width);
            prop_assume!(!layout.single_line);

            let name_prefix = 1 + shape.name.chars().count() + 1;
            let terminator = if shape.self_closing {
                SELF_CLOSING_TERMINATOR
            } else {
                PLAIN_TERMINATOR
            };
            let attrs_single_line: usize =
                shape.attr_widths.iter().map(|wd| wd + 1).sum();
            let single_line_width =
                shape.open_col + name_prefix + attrs_single_line + terminator;
            prop_assert!(
                single_line_width > w,
                "a wrapped layout must not have fit on one line: \
                 single_line_width={single_line_width} W={w} shape={shape:?}"
            );
        }
    }
}
