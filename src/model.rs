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
        };
        assert_eq!(tag.attributes.len(), 1);
        assert_eq!(tag.attributes[0], attr);

        let fp = TagFingerprint {
            name: "div".to_string(),
            self_closing: false,
            attributes: vec![(Some("class".to_string()), "class=\"a\"".to_string())],
        };
        let doc = Fingerprint {
            items: vec![FingerprintItem::Tag(fp), FingerprintItem::Text("text".to_string())],
        };
        assert_eq!(doc.items.len(), 2);
    }
}
