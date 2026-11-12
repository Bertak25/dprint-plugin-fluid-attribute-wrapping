//! Core data models: attributes, scanned tags, and fingerprints.
//!
//! These are the pure, borrow-friendly types shared across the scanner, sort,
//! wrap, render, and verify stages. They carry no behavior beyond construction
//! and the shared [`display_width`] column-width helper, which is the single
//! source of truth for column measurement used by scanning and width budgeting
//! alike (so `open_col`/`base_indent_col` derivation and line-fit decisions
//! always agree, per Requirement 8.5).

use std::ops::Range;

use unicode_width::UnicodeWidthChar;

/// Classification of a single [`Attribute`] within a start tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeKind {
    /// Boolean / plain attribute with no value, e.g. `disabled`.
    Plain,
    /// `name = value` attribute whose value is captured verbatim, whether the
    /// value is quoted (`"..."`, `'...'`), unquoted, or a braced expression
    /// (`class={x}`, `v-if="x"`, `on:click={...}`).
    NameValue,
    /// Spread or dynamic attribute captured verbatim, e.g. `{...props}` or
    /// `v-bind="obj"`. Its value is never parsed as an expression.
    DynamicSpread,
    /// A JSX/TSX braced attribute whose expression the scanner could not
    /// delimit with full confidence — the `Complex_Expression_Attribute` of
    /// Requirement 4.9 (for example an expression whose lexical state became
    /// ambiguous, or whose nesting could not be resolved with certainty). The
    /// attribute's `raw` slice still spans the scanner's best-effort delimited
    /// bytes, but the presence of this kind poisons the whole start tag:
    /// [`ScannedTag::confidently_rewritable`] is set to `false` so the tag is
    /// emitted byte-for-byte unchanged (a per-tag no-op) and never wrapped or
    /// sorted. Confidence is a property of the entire tag; one uncertain
    /// attribute is enough to mark the tag not-confidently-rewritable.
    ComplexExpression,
}

/// A single attribute parsed out of a start tag.
///
/// The [`raw`](Attribute::raw) slice is the single source of truth for the
/// attribute's content: rendering re-emits it byte-for-byte and never
/// reconstructs the attribute from parts, which makes value preservation
/// structural (Requirements 4.6, 5.3, 7.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute<'a> {
    /// The attribute's classification.
    pub kind: AttributeKind,
    /// Lowercased comparison name, used only for grouping and duplicate
    /// detection, never for output. `None` for pure spread attributes that have
    /// no name (e.g. `{...props}`).
    pub name_key: Option<String>,
    /// Exact source text of the attribute (name through end of value),
    /// preserved byte-for-byte.
    pub raw: &'a str,
    /// Display width of [`raw`](Attribute::raw) in columns, computed with the
    /// shared [`display_width`] helper.
    pub width: usize,
}

/// A start tag located by the scanner, with its attribute list and the column
/// geometry needed by the wrap/render stages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannedTag<'a> {
    /// Byte range of the full start tag within the source buffer.
    pub span: Range<usize>,
    /// Element name, borrowed from the source.
    pub name: &'a str,
    /// Whether the tag is self-closing (`<br />`).
    pub self_closing: bool,
    /// Attributes in source order.
    pub attributes: Vec<Attribute<'a>>,
    /// Whether the scanner delimited every attribute of this tag with full
    /// confidence. `false` when any attribute is a
    /// [`AttributeKind::ComplexExpression`] the scanner could not tokenize with
    /// certainty (Requirements 4.9, 7.4). Confidence is a property of the whole
    /// tag: a single uncertain attribute poisons it. A not-confidently-rewritable
    /// tag is emitted byte-for-byte unchanged — the wrap/render stages apply a
    /// per-tag no-op and never attempt wrapping or sorting on it. There is no
    /// file-level confidence flag; confidence is tracked and acted on per tag
    /// only, and divergence of tags that *were* rewritten is caught later by the
    /// per-tag fingerprint check.
    pub confidently_rewritable: bool,
    /// Display column of the `<` character, measured from the start of its
    /// physical line under the tab = `indent_width` column model. Used for the
    /// single-line fit decision.
    pub open_col: usize,
    /// Leading-whitespace column of the start tag's physical line — the display
    /// column of the first non-whitespace character of that line, under the same
    /// tab = `indent_width` model. Continuation indentation is derived from this,
    /// not from `open_col`, so mid-expression tags do not push continuation lines
    /// far to the right.
    pub base_indent_col: usize,
    /// Genuine JSX element start tags discovered inside this tag's attribute
    /// values, in source order, each paired with the provenance task 4.4's
    /// recursive `rewrite_tag` needs to map it from source coordinates to its
    /// position in the enclosing tag's *rendered* output (see [`NestedChild`]).
    ///
    /// Each child's [`tag`](NestedChild::tag)'s [`span`](ScannedTag::span) is
    /// expressed in SOURCE coordinates **relative to this (enclosing) tag's
    /// `span.start`** (not absolute buffer offsets, and not rendered offsets).
    ///
    /// The stored span is **never** used directly as a rendered offset: the
    /// source→rendered mapping is computed at rewrite time from the
    /// attribute-offset table (see `rewrite_tag`, task 4.4), keyed by the owning
    /// attribute's IDENTITY via [`NestedChild::owner_attr_index`] and the child's
    /// [`offset_in_attr_raw`](NestedChild::offset_in_attr_raw), because the
    /// enclosing wrap moves the attribute value to a new continuation column, so
    /// a source-measured position would not be a fixed point.
    ///
    /// Each child's [`tag`](NestedChild::tag) carries its own
    /// [`attributes`](ScannedTag::attributes) and
    /// [`confidently_rewritable`](ScannedTag::confidently_rewritable) flag. A
    /// child's [`open_col`](ScannedTag::open_col) and
    /// [`base_indent_col`](ScannedTag::base_indent_col) are **not** authoritative
    /// here: they are RECOMPUTED at rewrite time from the child's RENDERED
    /// position (Finding 4, task 4.4), because the source-measured geometry does
    /// not survive the enclosing wrap and would not be a fixed point.
    ///
    /// This field is empty in the overwhelming common case — any start tag with
    /// no element-bearing attribute value — and for every non-JSX/TSX dialect
    /// (HTML, Vue, Astro, Svelte), so non-nested tags and non-JSX dialects are
    /// byte-for-byte unaffected. A child's own
    /// [`tag.children`](ScannedTag::children) may itself be non-empty, so nesting
    /// can recurse to arbitrary depth.
    pub children: Vec<NestedChild<'a>>,
}

/// A genuine JSX element start tag discovered inside one of an enclosing
/// [`ScannedTag`]'s attribute values, paired with the provenance needed to
/// splice its own rewrite back into the enclosing tag's *rendered* output
/// without any text search (Finding 3, consumed by task 4.4's recursive
/// `rewrite_tag`).
///
/// # Why identity + intra-`raw` offset, not a rendered span
///
/// When the enclosing tag is wrapped, its attribute values move to new
/// continuation columns, so a child's *source* position is not its *rendered*
/// position. The scanner therefore records, per child, two **source-coordinate**
/// facts that survive the enclosing rewrite:
///
/// - [`owner_attr_index`](Self::owner_attr_index) — the index of the attribute
///   whose `raw` value contains this child, in the enclosing tag's
///   **source-order** [`attributes`](ScannedTag::attributes) vector. This is a
///   *stable identity* for the owning attribute: even when sorting reorders the
///   attributes before rendering, task 4.4 keys its attribute-offset table by
///   this same source index (the `Attribute` object a given source index names
///   is unchanged by sorting — sorting permutes the vector, it does not clone or
///   mutate the elements), so the child is located by attribute **identity**,
///   never by substring search over the rendered text.
/// - [`offset_in_attr_raw`](Self::offset_in_attr_raw) — the byte offset at which
///   this child's `<` begins, measured **relative to the start of the owning
///   attribute's `raw` slice** (so `attr.raw[offset_in_attr_raw..]` begins with
///   the child's `<`). Because each attribute's `raw` is re-emitted verbatim by
///   the renderer, this same intra-`raw` offset applies unchanged in the
///   rendered output: `child_rendered_start = attr_rendered_start +
///   offset_in_attr_raw` (task 4.4), with `attr_rendered_start` read from the
///   render stage's identity-keyed offset table.
///
/// [`tag`](Self::tag) is the child's own [`ScannedTag`], whose `span` is in
/// SOURCE coordinates relative to the enclosing tag's `span.start`. That span is
/// used only to recover geometry context, never as a rendered offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NestedChild<'a> {
    /// Index of the owning attribute in the enclosing tag's **source-order**
    /// [`attributes`](ScannedTag::attributes) vector. A stable identity for the
    /// attribute under sorting (see the type-level docs).
    pub owner_attr_index: usize,
    /// Byte offset of this child's `<` relative to the start of the owning
    /// attribute's [`raw`](Attribute::raw) slice. `attr.raw[offset_in_attr_raw..]`
    /// begins with the child's `<`; the same offset applies in the rendered
    /// output because `raw` is re-emitted verbatim.
    pub offset_in_attr_raw: usize,
    /// The child's own start tag, with a `span` in SOURCE coordinates relative
    /// to the enclosing tag's `span.start`.
    pub tag: ScannedTag<'a>,
}

/// Tag-local structural fingerprint: element name, self-closing flag, and the
/// ordered attribute `name → raw-value` pairs. Captured before and after a tag's
/// rewrite to drive the per-tag structure-equivalence check (Requirement 7.6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagFingerprint {
    /// Element name.
    pub name: String,
    /// Whether the tag is self-closing.
    pub self_closing: bool,
    /// Ordered attribute identity pairs: `(name_key, raw-value)`. `name_key` is
    /// `None` for pure spreads; `raw` is carried so value identity is captured.
    pub attributes: Vec<(Option<String>, String)>,
    /// Structural fingerprints of the genuine JSX elements nested inside this
    /// tag's attribute values, in source order (task 4.4, Finding 5). Empty in
    /// the common case and for every non-JSX/TSX dialect. Recursing the
    /// fingerprint over `children` lets the structure-equivalence check see the
    /// full two-level (and arbitrarily deep) markup structure, so a nested
    /// element that is dropped, duplicated, or structurally changed by a
    /// two-level rewrite is caught just like a top-level tag.
    pub children: Vec<TagFingerprint>,
}

/// A single element in a whole-document [`Fingerprint`]: either a start tag's
/// structural signature or a verbatim run of inter-tag text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FingerprintItem {
    /// A start tag's tag-local fingerprint.
    Tag(TagFingerprint),
    /// Verbatim text lying between start tags, preserved for the document-level
    /// equivalence comparison.
    Text(String),
}

/// Whole-document structural fingerprint: the ordered sequence of start-tag
/// signatures interleaved with the verbatim text between them. Compared between
/// input and output as the document-level structure-equivalence backstop
/// (Requirement 7.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    /// Ordered items making up the document structure.
    pub items: Vec<FingerprintItem>,
}

/// Compute the display width, in columns, of `text` under the shared column
/// model used across scanning and width budgeting.
///
/// Width is the sum of each code point's display width via the `unicode-width`
/// crate (East Asian Width aware), so wide CJK glyphs count as two columns and
/// most other characters as one. Tab characters count as `indent_width` columns.
/// `unicode-width` operates on code points, not grapheme clusters, so complex
/// grapheme clusters (ZWJ emoji sequences, combining marks) are approximated by
/// summing the display widths of their component code points — the accepted,
/// pragmatic choice for a line-fitting formatter that avoids a
/// grapheme-segmentation dependency.
#[must_use]
pub fn display_width(text: &str, indent_width: usize) -> usize {
    text.chars()
        .map(|ch| char_width(ch, indent_width))
        .sum()
}

/// Display width of a single code point under the shared column model.
///
/// Tabs count as `indent_width` columns; all other code points use their
/// `unicode-width` display width, with control characters (width `None`) counted
/// as zero columns.
#[must_use]
pub fn char_width(ch: char, indent_width: usize) -> usize {
    if ch == '\t' {
        indent_width
    } else {
        UnicodeWidthChar::width(ch).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_width_is_one_per_char() {
        // One column per ASCII code point, including spaces; indent_width is
        // irrelevant when no tabs are present (Requirement 4.1).
        assert_eq!(display_width("hello", 2), 5);
        assert_eq!(display_width("", 2), 0);
        assert_eq!(display_width("a b c", 4), 5);
        // A realistic attribute token measures its exact column span.
        assert_eq!(display_width("class=\"btn\"", 2), 11);
        // The indent_width argument must not affect tab-free ASCII text.
        assert_eq!(display_width("id=main", 2), display_width("id=main", 8));
    }

    #[test]
    fn wide_cjk_glyphs_count_as_two_columns() {
        // Each of these ideographs is East-Asian-Wide (2 columns).
        assert_eq!(display_width("中", 2), 2);
        assert_eq!(display_width("中文", 2), 4);
        // Mixed ASCII + wide: "a中b" -> 1 + 2 + 1.
        assert_eq!(display_width("a中b", 2), 4);
        // A wide attribute value: `title="中文"` -> 8 ASCII columns
        // (`title="` plus the closing `"`) + 2 wide glyphs (4) = 12 columns,
        // which the line-fit budget must account for.
        assert_eq!(display_width("title=\"中文\"", 2), 12);
        // Hiragana / full-width forms are also East-Asian-Wide.
        assert_eq!(display_width("あ", 2), 2);
        assert_eq!(display_width("Ａ", 2), 2);
    }

    #[test]
    fn tabs_count_as_indent_width_columns() {
        assert_eq!(display_width("\t", 4), 4);
        assert_eq!(display_width("\t", 2), 2);
        // The same tab yields a different width purely from indent_width.
        assert_eq!(display_width("\t", 8), 8);
        // Two tabs plus two spaces at indent_width = 4.
        assert_eq!(display_width("\t\t  ", 4), 10);
        // A tab mixed with ASCII text.
        assert_eq!(display_width("\tab", 8), 10);
        // Tab width scales linearly with indent_width for the same input.
        assert_eq!(display_width("\t\tx", 2), 5);
        assert_eq!(display_width("\t\tx", 4), 9);
    }

    #[test]
    fn char_width_handles_tab_and_control() {
        assert_eq!(char_width('\t', 4), 4);
        assert_eq!(char_width('\t', 2), 2);
        assert_eq!(char_width('a', 4), 1);
        assert_eq!(char_width('中', 4), 2);
        // Control characters have no display width and count as zero.
        assert_eq!(char_width('\u{0}', 4), 0);
        assert_eq!(char_width('\n', 4), 0);
        // A zero-width code point (ZWJ) also contributes zero columns.
        assert_eq!(char_width('\u{200D}', 4), 0);
    }

    #[test]
    fn complex_grapheme_clusters_are_approximated_per_code_point() {
        // `unicode-width` measures code points, not grapheme clusters, so both
        // ZWJ emoji sequences and combining marks are approximated by summing
        // their component code-point widths rather than being measured as a
        // single cluster. This documents the accepted approximation called out
        // by the task note (Requirements 4.1, 4.2).

        // ZWJ family emoji: a sequence of several code points joined by U+200D.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let family_expected: usize = family.chars().map(|c| char_width(c, 2)).sum();
        assert_eq!(display_width(family, 2), family_expected);

        // Combining marks: base letter + combining acute accent. A grapheme
        // segmenter would treat "e\u{0301}" as one cluster, but we sum the base
        // (1 column) and the zero-width combining mark (0 columns) -> 1 column.
        let combined = "e\u{0301}";
        assert_eq!(char_width('\u{0301}', 2), 0);
        assert_eq!(display_width(combined, 2), 1);
        // The approximation equals the per-code-point sum by construction.
        let combined_expected: usize = combined.chars().map(|c| char_width(c, 2)).sum();
        assert_eq!(display_width(combined, 2), combined_expected);
    }

    #[test]
    fn attribute_and_tag_models_construct() {
        let attr = Attribute {
            kind: AttributeKind::NameValue,
            name_key: Some("class".to_string()),
            raw: "class=\"a\"",
            width: display_width("class=\"a\"", 2),
        };
        assert_eq!(attr.width, 9);

        let tag = ScannedTag {
            span: 0..10,
            name: "div",
            self_closing: false,
            attributes: vec![attr.clone()],
            confidently_rewritable: true,
            open_col: 0,
            base_indent_col: 0,
            children: Vec::new(),
        };
        assert_eq!(tag.attributes.len(), 1);
        assert_eq!(tag.attributes[0], attr);

        let fp = TagFingerprint {
            name: "div".to_string(),
            self_closing: false,
            attributes: vec![(Some("class".to_string()), "class=\"a\"".to_string())],
            children: Vec::new(),
        };
        let doc = Fingerprint {
            items: vec![FingerprintItem::Tag(fp), FingerprintItem::Text("text".to_string())],
        };
        assert_eq!(doc.items.len(), 2);
    }
}
