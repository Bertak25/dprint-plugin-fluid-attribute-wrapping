//! Canonical start-tag rendering (task 8.2).
//!
//! This module turns a (possibly already sorted) [`ScannedTag`] into the exact
//! replacement text for its source span. It is the only stage that emits
//! whitespace bytes: the greedy packer in [`crate::wrap`] decides *which*
//! attribute lands on *which* line, and `render_start_tag` turns that layout
//! decision into canonical text.
//!
//! ## What "canonical" means
//!
//! Every byte of whitespace this module emits when wrapping is enabled is
//! derived purely from configuration and geometry, never copied from the scanned
//! spacing:
//!
//! - the first line is `<name` at the tag's original indentation, followed by a
//!   single space before the first attribute;
//! - adjacent attributes on the same line are separated by exactly one space;
//! - every continuation line begins with exactly `cont_indent` columns of
//!   indentation, built from tabs or spaces per [`Configuration::use_tabs`] and
//!   [`Configuration::indent_width`];
//! - the terminator is normalized: a self-closing tag ends with the canonical
//!   ` />` (exactly one space before `/>`, width 3) and a non-self-closing tag
//!   ends with `>` (width 1).
//!
//! Because the emitted whitespace is a pure function of config + geometry,
//! re-rendering the output reproduces it byte-for-byte, which is what makes the
//! wrapping transform idempotent (Requirement 8.1). The attribute `raw` slices
//! themselves are always re-emitted verbatim, so attribute value identity is
//! preserved (Requirements 4.6, 7.4).
//!
//! ## Wrapping disabled (Requirement 4.8)
//!
//! When [`Configuration::fluid_wrapping`] is `false`, the renderer does not
//! repack. If sorting is also disabled there is nothing to do and the original
//! tag bytes are returned verbatim. If only sorting is enabled, the renderer
//! re-emits the attributes in their (already sorted) order while reusing the
//! upstream whitespace that separated the attributes in the source — so the line
//! breaks the upstream plugin produced are left unchanged.
//!
//! ## Newline kind (Requirement 8.1)
//!
//! Continuation-line breaks are emitted with the [`Newline`] kind detected from
//! the surrounding buffer, so a CRLF file keeps CRLF endings and an LF file
//! keeps LF endings.

use crate::config::Configuration;
use crate::model::ScannedTag;
use crate::wrap::plan_layout;

/// The line-ending convention used when emitting continuation-line breaks.
///
/// Detected from the surrounding buffer so the renderer's output matches the
/// file's existing convention (Requirement 8.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Newline {
    /// Unix line endings (`\n`).
    Lf,
    /// Windows line endings (`\r\n`).
    CrLf,
}

impl Newline {
    /// The byte sequence this newline kind emits.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Newline::Lf => "\n",
            Newline::CrLf => "\r\n",
        }
    }

    /// Detect the newline convention of `buffer`.
    ///
    /// The first line ending in the buffer decides the convention: a `\r\n`
    /// yields [`Newline::CrLf`], a lone `\n` yields [`Newline::Lf`]. A buffer
    /// with no line ending at all defaults to [`Newline::Lf`], the dominant
    /// convention for the source files this plugin targets.
    #[must_use]
    pub fn detect(buffer: &str) -> Newline {
        let bytes = buffer.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                if i > 0 && bytes[i - 1] == b'\r' {
                    return Newline::CrLf;
                }
                return Newline::Lf;
            }
        }
        Newline::Lf
    }
}

/// Build an indentation string of `columns` display columns under the shared
/// tab = `indent_width` column model.
///
/// With `use_tabs == true`, indentation is emitted as `columns / indent_width`
/// tab characters followed by `columns % indent_width` spaces, so a column count
/// that is an exact multiple of `indent_width` is pure tabs and any remainder is
/// padded with spaces (the remainder only arises for mid-expression tags whose
/// `base_indent_col` is not a clean multiple of the indent unit). With
/// `use_tabs == false`, indentation is `columns` spaces.
///
/// `indent_width` of `0` is treated as `1` to avoid a divide-by-zero; a zero
/// indent width is not a valid resolved configuration, but guarding here keeps
/// the function total and panic-free.
#[must_use]
pub fn build_indent(columns: usize, use_tabs: bool, indent_width: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    if !use_tabs {
        return " ".repeat(columns);
    }
    let unit = indent_width.max(1);
    let tabs = columns / unit;
    let spaces = columns % unit;
    let mut out = String::with_capacity(tabs + spaces);
    for _ in 0..tabs {
        out.push('\t');
    }
    for _ in 0..spaces {
        out.push(' ');
    }
    out
}

/// The canonical terminator for `tag`: ` />` for a self-closing tag, `>`
/// otherwise. This normalizes any amount of whitespace that preceded `/>` in the
/// source to exactly one space, which is what keeps the self-closing form
/// idempotent.
fn terminator(tag: &ScannedTag) -> &'static str {
    if tag.self_closing {
        " />"
    } else {
        ">"
    }
}

/// Render the replacement text for `tag`'s source span.
///
/// `src_tag` is the exact source slice of the start tag (the bytes of
/// `tag.span`); it is used only when wrapping is disabled, to preserve the
/// upstream line breaks (and to return the tag verbatim when both wrapping and
/// sorting are disabled). When wrapping is enabled the output is built purely
/// from the tag's attributes and geometry plus the resolved config, so no
/// scanned whitespace leaks into the canonical form.
///
/// `newline` is the detected line-ending kind used for continuation breaks.
#[must_use]
pub fn render_start_tag(
    tag: &ScannedTag,
    config: &Configuration,
    newline: Newline,
    src_tag: &str,
) -> String {
    render_start_tag_with_offsets(tag, config, newline, src_tag).0
}

/// Render the replacement text for `tag`'s source span, additionally returning
/// an **attribute-offset table** that records, for each of `tag`'s attributes
/// (in the exact order they appear in `tag.attributes`), the byte offset within
/// the returned string at which that attribute's
/// [`raw`](crate::model::Attribute::raw) slice begins (Finding 3, task 4.4).
///
/// The renderer emits each `attr.raw` verbatim in a single left-to-right pass,
/// so the offset recorded for entry `i` is exactly the start of
/// `tag.attributes[i].raw` in the output. The table is indexed positionally in
/// lockstep with `tag.attributes`, so a caller that holds the SAME (possibly
/// sorted) `tag.attributes` vector can look up an attribute's rendered start by
/// the attribute's **identity** — never by a substring search over the output,
/// which could mis-locate an attribute whose `raw` recurs elsewhere.
///
/// On the byte-for-byte no-op paths (a not-confidently-rewritable tag, or
/// wrapping-and-sorting-disabled) the returned string is `src_tag` verbatim and
/// the offset table is empty: those paths never rewrite the tag, so no caller
/// needs to re-locate an attribute within the output (and the child-splice
/// recursion in [`crate::engine`] only runs for confidently-rewritable tags).
#[must_use]
pub fn render_start_tag_with_offsets(
    tag: &ScannedTag,
    config: &Configuration,
    newline: Newline,
    src_tag: &str,
) -> (String, Vec<usize>) {
    // A tag the scanner could not delimit with full confidence is emitted
    // byte-for-byte unchanged (Requirement 4.9): never wrap or reorder it, and
    // emit no offsets (the engine never splices children into a no-op tag).
    if !tag.confidently_rewritable {
        return (src_tag.to_string(), Vec::new());
    }

    if !config.fluid_wrapping {
        return render_wrapping_disabled(tag, config, src_tag);
    }

    render_wrapped(tag, config, newline)
}

/// Render with fluid wrapping enabled: compute the greedy layout and emit
/// canonical whitespace. Returns the rendered text plus the attribute-offset
/// table (rendered start byte offset per attribute, in `tag.attributes` order).
fn render_wrapped(tag: &ScannedTag, config: &Configuration, newline: Newline) -> (String, Vec<usize>) {
    let indent_width = usize::from(config.indent_width);
    let line_width = config.line_width as usize;
    let layout = plan_layout(tag, line_width, indent_width);

    let mut out = String::new();
    let mut offsets: Vec<usize> = Vec::with_capacity(tag.attributes.len());
    out.push('<');
    out.push_str(tag.name);

    if layout.single_line {
        // One line: `<name` then each attribute preceded by a single space.
        for attr in &tag.attributes {
            out.push(' ');
            offsets.push(out.len());
            out.push_str(attr.raw);
        }
        out.push_str(terminator(tag));
        return (out, offsets);
    }

    // Multi-line: the first line holds the name prefix and the attributes of the
    // first layout line; every continuation line is broken to `cont_indent`.
    let cont_indent = build_indent(layout.cont_indent, config.use_tabs, indent_width);
    for (line_idx, line) in layout.lines.iter().enumerate() {
        if line_idx == 0 {
            // First line continues after `<name` with single-space separators.
            for attr in &tag.attributes[line.start..line.end] {
                out.push(' ');
                offsets.push(out.len());
                out.push_str(attr.raw);
            }
        } else {
            out.push_str(newline.as_str());
            out.push_str(&cont_indent);
            for (offset, attr) in tag.attributes[line.start..line.end].iter().enumerate() {
                if offset > 0 {
                    out.push(' ');
                }
                offsets.push(out.len());
                out.push_str(attr.raw);
            }
        }
    }
    out.push_str(terminator(tag));
    (out, offsets)
}

/// Render with fluid wrapping disabled (Requirement 4.8).
///
/// With sorting also disabled there is nothing to change, so the original tag
/// bytes are returned verbatim. With only sorting enabled, the attributes are
/// re-emitted in their (already sorted) order while reusing the upstream
/// whitespace that separated them in the source, so the upstream line breaks are
/// left unchanged. The terminator is still normalized to its canonical form so
/// the self-closing space stays idempotent.
fn render_wrapping_disabled(
    tag: &ScannedTag,
    config: &Configuration,
    src_tag: &str,
) -> (String, Vec<usize>) {
    // Both wrapping and sorting disabled: the tag is unchanged. Return it
    // verbatim so the document is byte-for-byte preserved. No offsets: this is a
    // pure no-op, and the engine does not splice children into it.
    if !config.attribute_sorting {
        return (src_tag.to_string(), Vec::new());
    }

    // Sorting enabled, wrapping disabled (Requirements 4.8, 5): emit the
    // attributes in their new (sorted) order while preserving upstream line
    // breaks. The break information is bound to each ATTRIBUTE, not to its
    // positional slot: an attribute whose source occurrence was preceded by a
    // line break keeps a break before it wherever sorting places it; every other
    // attribute is separated by a single canonical space.
    //
    // Binding breaks to attribute identity (rather than to the positional slot
    // the way an earlier implementation did) is what makes this path
    // idempotent under reordering. With positional separators, reordering could
    // move a break to sit before a different attribute on pass 1, and pass 2 —
    // now seeing a different layout — would place it differently again, so the
    // output was not a fixed point (it oscillated between a break and a space).
    // With attribute-bound breaks, the same attribute carries the same break on
    // every pass, so pass 2 reproduces pass 1 byte-for-byte.
    let had_leading_break = leading_breaks_by_attribute(src_tag, tag);

    let mut out = String::new();
    let mut offsets: Vec<usize> = Vec::with_capacity(tag.attributes.len());
    out.push('<');
    out.push_str(tag.name);
    for (idx, attr) in tag.attributes.iter().enumerate() {
        // The first emitted attribute always follows `<name` with a single
        // space (a leading break before the very first attribute is not
        // meaningful once sorting has chosen a new first attribute, and a space
        // keeps the tag's head canonical and idempotent). Subsequent attributes
        // keep a break iff the attribute they resolve to had one in the source.
        let sep = if idx > 0 && had_leading_break.get(idx).copied().unwrap_or(false) {
            // Preserve the upstream break as a lone newline; the renderer does
            // not re-indent in the wrapping-disabled path (Requirement 4.8),
            // matching the existing newline-preservation behavior.
            "\n"
        } else {
            " "
        };
        out.push_str(sep);
        offsets.push(out.len());
        out.push_str(attr.raw);
    }
    out.push_str(terminator(tag));
    (out, offsets)
}

/// For each attribute of `tag` (in the tag's current, possibly sorted order),
/// decide whether its source occurrence was immediately preceded by a line
/// break. Identical-`raw` attributes are matched to distinct source occurrences
/// left-to-right, so duplicates are classified by their own occurrence rather
/// than all collapsing onto the first.
///
/// The result is indexed in lockstep with `tag.attributes`, so
/// `render_wrapping_disabled` can look up break-vs-space per attribute as it
/// emits them in sorted order.
///
/// Char-boundary audit for the `src_tag[last_nonspace..rel]` slice: `rel` comes
/// from `find_from` -> `str::find`, which always returns a UTF-8 char boundary;
/// `0` (the `map_or` default when the prefix is all whitespace) is a boundary;
/// and `last_nonspace` is computed as `i + c.len_utf8()`, i.e. one past the END
/// of the matched character, which is a boundary by construction for any valid
/// UTF-8. So both ends of the slice are boundary-safe for any valid UTF-8.
fn leading_breaks_by_attribute(src_tag: &str, tag: &ScannedTag) -> Vec<bool> {
    let mut claimed: Vec<usize> = Vec::with_capacity(tag.attributes.len());
    let mut breaks: Vec<bool> = Vec::with_capacity(tag.attributes.len());
    for attr in &tag.attributes {
        let mut from = 0usize;
        let mut resolved: Option<usize> = None;
        while let Some(rel) = find_from(src_tag, attr.raw, from) {
            if claimed.contains(&rel) {
                from = rel + attr.raw.len().max(1);
            } else {
                claimed.push(rel);
                resolved = Some(rel);
                break;
            }
        }
        // Whether a line break appears in the whitespace immediately before this
        // occurrence (between the previous non-whitespace byte and `rel`).
        let has_break = resolved.is_some_and(|rel| {
            let prefix = &src_tag[..rel];
            let last_nonspace = prefix
                .char_indices()
                .rev()
                .find(|(_, c)| !c.is_whitespace())
                .map_or(0, |(i, c)| i + c.len_utf8());
            src_tag[last_nonspace..rel].contains('\n')
        });
        breaks.push(has_break);
    }
    breaks
}

/// Find `needle` in `haystack` starting at byte offset `from`, returning the
/// absolute byte offset of the match, or `None`.
fn find_from(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    haystack.get(from..)?.find(needle).map(|rel| from + rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Configuration, SchemeId};
    use crate::model::{display_width, Attribute, AttributeKind, ScannedTag};

    /// Build a `NameValue` attribute with width measured at `indent_width = 2`.
    fn attr(raw: &'static str) -> Attribute<'static> {
        Attribute {
            kind: AttributeKind::NameValue,
            name_key: None,
            raw,
            width: display_width(raw, 2),
        }
    }

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
            children: Vec::new(),
        }
    }

    /// A config with wrapping enabled and the given width/indent/tabs.
    fn config(line_width: u32, indent_width: u8, use_tabs: bool) -> Configuration {
        Configuration {
            line_width,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width,
            use_tabs,
            vue: false,
            astro: false,
            svelte: false,
        }
    }

    // ---- Newline detection ---------------------------------------------

    #[test]
    fn detect_lf_newline() {
        assert_eq!(Newline::detect("a\nb"), Newline::Lf);
        assert_eq!(Newline::detect("line one\nline two\n"), Newline::Lf);
    }

    #[test]
    fn detect_crlf_newline() {
        assert_eq!(Newline::detect("a\r\nb"), Newline::CrLf);
        assert_eq!(Newline::detect("<div>\r\n</div>"), Newline::CrLf);
    }

    #[test]
    fn detect_defaults_to_lf_without_newline() {
        assert_eq!(Newline::detect(""), Newline::Lf);
        assert_eq!(Newline::detect("no newline here"), Newline::Lf);
    }

    #[test]
    fn detect_first_newline_wins() {
        // A lone LF appearing before any CRLF decides the convention.
        assert_eq!(Newline::detect("a\nb\r\nc"), Newline::Lf);
        // A CRLF first decides CRLF even if a later line uses a bare LF.
        assert_eq!(Newline::detect("a\r\nb\nc"), Newline::CrLf);
    }

    #[test]
    fn newline_as_str() {
        assert_eq!(Newline::Lf.as_str(), "\n");
        assert_eq!(Newline::CrLf.as_str(), "\r\n");
    }

    // ---- build_indent --------------------------------------------------

    #[test]
    fn build_indent_spaces() {
        assert_eq!(build_indent(0, false, 2), "");
        assert_eq!(build_indent(4, false, 2), "    ");
        assert_eq!(build_indent(3, false, 4), "   ");
    }

    #[test]
    fn build_indent_tabs_exact_multiple() {
        // 4 columns at indent_width 2 -> two tabs, no remainder.
        assert_eq!(build_indent(4, true, 2), "\t\t");
        // 8 columns at indent_width 4 -> two tabs.
        assert_eq!(build_indent(8, true, 4), "\t\t");
    }

    #[test]
    fn build_indent_tabs_with_remainder() {
        // 5 columns at indent_width 2 -> two tabs + one space.
        assert_eq!(build_indent(5, true, 2), "\t\t ");
        // 6 columns at indent_width 4 -> one tab + two spaces.
        assert_eq!(build_indent(6, true, 4), "\t  ");
    }

    #[test]
    fn build_indent_guards_zero_indent_width() {
        // indent_width 0 is coerced to 1, so columns map to that many tabs.
        assert_eq!(build_indent(3, true, 0), "\t\t\t");
    }

    // ---- single-line rendering -----------------------------------------

    #[test]
    fn single_line_non_self_closing() {
        let t = tag(
            "div",
            false,
            0,
            0,
            vec![attr("class=\"a\""), attr("id=\"b\"")],
        );
        let out = render_start_tag(&t, &config(80, 2, false), Newline::Lf, "");
        assert_eq!(out, "<div class=\"a\" id=\"b\">");
    }

    #[test]
    fn single_line_self_closing_canonical_terminator() {
        // A self-closing tag emits the canonical ` />` with exactly one space.
        let t = tag("br", true, 0, 0, vec![attr("class=\"a\"")]);
        let out = render_start_tag(&t, &config(80, 2, false), Newline::Lf, "");
        assert_eq!(out, "<br class=\"a\" />");
    }

    #[test]
    fn zero_attribute_tag() {
        let t = tag("hr", false, 0, 0, vec![]);
        let out = render_start_tag(&t, &config(80, 2, false), Newline::Lf, "");
        assert_eq!(out, "<hr>");

        let sc = tag("hr", true, 0, 0, vec![]);
        let out_sc = render_start_tag(&sc, &config(80, 2, false), Newline::Lf, "");
        assert_eq!(out_sc, "<hr />");
    }

    // ---- multi-line rendering ------------------------------------------

    #[test]
    fn multi_line_wrap_spaces() {
        // Four width-8 attributes, W = 24 (matches the wrap.rs packing test):
        // line 0: a0 a1, line 1: a2 a3. cont_indent = base_indent(0) + 2 = 2.
        let attrs = vec![
            attr("a0=\"xxx\""),
            attr("a1=\"xxx\""),
            attr("a2=\"xxx\""),
            attr("a3=\"xxx\""),
        ];
        let t = tag("p", false, 0, 0, attrs);
        let out = render_start_tag(&t, &config(24, 2, false), Newline::Lf, "");
        let expected = "<p a0=\"xxx\" a1=\"xxx\"\n  a2=\"xxx\" a3=\"xxx\">";
        assert_eq!(out, expected);
    }

    #[test]
    fn multi_line_wrap_crlf() {
        // Same layout but CRLF continuation breaks.
        let attrs = vec![
            attr("a0=\"xxx\""),
            attr("a1=\"xxx\""),
            attr("a2=\"xxx\""),
            attr("a3=\"xxx\""),
        ];
        let t = tag("p", false, 0, 0, attrs);
        let out = render_start_tag(&t, &config(24, 2, false), Newline::CrLf, "");
        let expected = "<p a0=\"xxx\" a1=\"xxx\"\r\n  a2=\"xxx\" a3=\"xxx\">";
        assert_eq!(out, expected);
    }

    #[test]
    fn multi_line_wrap_tabs_indentation() {
        // use_tabs: continuation indent of 2 columns at indent_width 2 is one tab.
        let attrs = vec![
            attr("a0=\"xxx\""),
            attr("a1=\"xxx\""),
            attr("a2=\"xxx\""),
            attr("a3=\"xxx\""),
        ];
        let t = tag("p", false, 0, 0, attrs);
        let out = render_start_tag(&t, &config(24, 2, true), Newline::Lf, "");
        let expected = "<p a0=\"xxx\" a1=\"xxx\"\n\ta2=\"xxx\" a3=\"xxx\">";
        assert_eq!(out, expected);
    }

    #[test]
    fn multi_line_self_closing_terminator_on_last_line() {
        // A wrapped self-closing tag ends with the canonical ` />` on the final
        // continuation line.
        let attrs = vec![
            attr("a0=\"xxx\""),
            attr("a1=\"xxx\""),
            attr("a2=\"xxx\""),
            attr("a3=\"xxx\""),
        ];
        let t = tag("input", true, 0, 0, attrs);
        let out = render_start_tag(&t, &config(28, 2, false), Newline::Lf, "");
        assert!(out.ends_with(" />"), "self-closing terminator: {out}");
        assert!(!out.ends_with("  />"), "exactly one space before />");
    }

    #[test]
    fn mid_line_tag_renders_on_a_single_line() {
        // open_col large, base_indent small (open_col != base_indent_col): this
        // is a mid-line tag, which is never wrapped (Requirement 8 idempotence).
        // Even at a width far smaller than the tag it renders on one line with
        // single-space separators and the canonical terminator — a form with no
        // base_indent/cont_indent dependence, hence trivially pass-invariant.
        let attrs = vec![attr("alpha=\"1\""), attr("bravo=\"2\""), attr("charlie=\"3\"")];
        let t = tag("div", false, 50, 4, attrs);
        let out = render_start_tag(&t, &config(40, 2, false), Newline::Lf, "");
        assert!(!out.contains('\n'), "a mid-line tag must not wrap: {out:?}");
        assert_eq!(out, "<div alpha=\"1\" bravo=\"2\" charlie=\"3\">");

        // Re-rendering the output reproduces it byte-for-byte (idempotent).
        let again = render_start_tag(&t, &config(40, 2, false), Newline::Lf, &out);
        assert_eq!(again, out);
    }

    // ---- idempotent terminator ------------------------------------------

    #[test]
    fn rendering_is_idempotent_for_self_closing() {
        // Render once, feed the output back as src_tag with the same tag, and the
        // canonical terminator must reproduce byte-identically.
        let t = tag("br", true, 0, 0, vec![attr("class=\"a\"")]);
        let cfg = config(80, 2, false);
        let first = render_start_tag(&t, &cfg, Newline::Lf, "");
        let second = render_start_tag(&t, &cfg, Newline::Lf, &first);
        assert_eq!(first, second);
        assert!(first.ends_with(" />"));
    }

    // ---- wrapping disabled ---------------------------------------------

    #[test]
    fn wrapping_and_sorting_disabled_returns_verbatim() {
        let mut cfg = config(80, 2, false);
        cfg.fluid_wrapping = false;
        cfg.attribute_sorting = false;
        let src = "<div  class=\"a\"\n   id=\"b\" >";
        let t = tag(
            "div",
            false,
            0,
            0,
            vec![attr("class=\"a\""), attr("id=\"b\"")],
        );
        let out = render_start_tag(&t, &cfg, Newline::Lf, src);
        // Byte-for-byte identical to the source slice.
        assert_eq!(out, src);
    }

    #[test]
    fn wrapping_disabled_sorting_enabled_binds_break_to_attribute() {
        // With wrapping disabled and sorting enabled, an upstream line break is
        // bound to the ATTRIBUTE it preceded, not to a positional slot, so it
        // travels with that attribute under sorting. This is what makes the path
        // idempotent (Requirements 4.8, 5).
        //
        // Here the source break precedes `data-x` (the source-second attribute).
        // Sorting places `id` first and `data-x` second, so `data-x` keeps its
        // break: the output is `<div id="b"\ndata-x="a">`.
        let mut cfg = config(80, 2, false);
        cfg.fluid_wrapping = false;
        cfg.attribute_sorting = true;
        // Source order is `id` then `data-x`, with the break before `data-x`
        // (the source-second attribute). Sorting keeps `id` first and `data-x`
        // second, so `data-x` retains the break bound to it.
        let src = "<div id=\"b\"\n  data-x=\"a\"";
        // Attributes provided in sorted order: id (grouped first) before data-x.
        let t = tag(
            "div",
            false,
            0,
            0,
            vec![attr("id=\"b\""), attr("data-x=\"a\"")],
        );
        let out = render_start_tag(&t, &cfg, Newline::Lf, src);
        // The break bound to `data-x` is preserved (it is not the first attr).
        assert!(out.contains('\n'), "break bound to data-x is preserved: {out:?}");
        assert!(out.contains("id=\"b\""));
        assert!(out.contains("data-x=\"a\""));
        // Idempotence: re-rendering the output reproduces it byte-for-byte. The
        // attribute order is now fixed, so the second pass sees the same break
        // before the same attribute and emits identically.
        let again = render_start_tag(&t, &cfg, Newline::Lf, &out);
        assert_eq!(again, out, "wrapping-disabled sorting path must be idempotent");
    }

    #[test]
    fn wrapping_disabled_break_before_first_sorted_attr_is_dropped() {
        // When the break was bound to the attribute that sorting moves to FIRST
        // position, it is dropped: a leading break before the first attribute
        // (right after `<name`) is not meaningful and would not be idempotent.
        // The first attribute always follows `<name` with a single space. This
        // is the correct, idempotent behavior — verified by re-rendering.
        let mut cfg = config(80, 2, false);
        cfg.fluid_wrapping = false;
        cfg.attribute_sorting = true;
        // Source break precedes `id` (source-second); sorting moves `id` first.
        let src = "<div class=\"a\"\n  id=\"b\"";
        let t = tag(
            "div",
            false,
            0,
            0,
            vec![attr("id=\"b\""), attr("class=\"a\"")],
        );
        let out = render_start_tag(&t, &cfg, Newline::Lf, src);
        // `id` is now first, so its leading break is dropped; `class` had no
        // break, so the whole tag is single-line and canonical.
        assert_eq!(out, "<div id=\"b\" class=\"a\">");
        // Idempotent.
        let again = render_start_tag(&t, &cfg, Newline::Lf, &out);
        assert_eq!(again, out);
    }

    #[test]
    fn not_confidently_rewritable_tag_is_verbatim() {
        let src = "<div a={x > y} b />";
        let mut t = tag("div", true, 0, 0, vec![attr("b")]);
        t.confidently_rewritable = false;
        let out = render_start_tag(&t, &config(80, 2, false), Newline::Lf, src);
        assert_eq!(out, src);
    }
}

#[cfg(test)]
mod wrap_render_e2e_tests {
    //! End-to-end wrap + render coverage for task 8.3.
    //!
    //! The existing `mod tests` module unit-tests the packer (`plan_layout`) and
    //! the renderer (`render_start_tag`) in isolation against hand-built
    //! `ScannedTag`s. These tests instead drive the full pipeline — scan real
    //! source with the dialect scanner, then render the located tag — and assert
    //! the exact output string. This proves the scanned column geometry
    //! (`open_col`, `base_indent_col`) and the attribute `raw` slices wire
    //! through wrap and render to the canonical text the way the task describes,
    //! and that re-rendering the scanned output reproduces it byte-for-byte.

    use super::*;
    use crate::config::{Configuration, SchemeId};
    use crate::dialect::Dialect;
    use crate::model::ScannedTag;
    use crate::scanner::scan_start_tags;
    use crate::wrap::plan_layout;

    /// A config with wrapping enabled and the given width/indent/tabs, sorting
    /// off (the wrap/render path under test).
    fn config(line_width: u32, indent_width: u8, use_tabs: bool) -> Configuration {
        Configuration {
            line_width,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width,
            use_tabs,
            vue: false,
            astro: false,
            svelte: false,
        }
    }

    /// Scan `src` as HTML and return the single start tag it contains.
    fn scan_one_html(src: &str, indent_width: usize) -> ScannedTag<'_> {
        let profile = Dialect::Html.scanner_profile();
        let mut tags = scan_start_tags(src, &profile, indent_width)
            .expect("scan should succeed");
        assert_eq!(tags.len(), 1, "expected exactly one start tag in {src:?}");
        tags.remove(0)
    }

    /// Scan `src` with a specific dialect and return the single start tag.
    fn scan_one(src: &str, dialect: Dialect, indent_width: usize) -> ScannedTag<'_> {
        let profile = dialect.scanner_profile();
        let mut tags = scan_start_tags(src, &profile, indent_width)
            .expect("scan should succeed");
        assert_eq!(tags.len(), 1, "expected exactly one start tag in {src:?}");
        tags.remove(0)
    }

    /// Render the tag located in `src` under `cfg`, detecting the newline from
    /// the source buffer and passing the tag's own source slice as `src_tag`.
    fn render_scanned(src: &str, tag: &ScannedTag, cfg: &Configuration) -> String {
        let newline = Newline::detect(src);
        let src_tag = &src[tag.span.clone()];
        render_start_tag(tag, cfg, newline, src_tag)
    }

    // ---- zero-attribute tag --------------------------------------------

    #[test]
    fn e2e_zero_attribute_tag_non_self_closing() {
        let src = "<hr>";
        let tag = scan_one_html(src, 2);
        let out = render_scanned(src, &tag, &config(80, 2, false));
        assert_eq!(out, "<hr>");
    }

    #[test]
    fn e2e_zero_attribute_tag_self_closing_canonicalizes_terminator() {
        // Scanned as `<br/>` (no space); the renderer emits the canonical ` />`.
        let src = "<br/>";
        let tag = scan_one_html(src, 2);
        let out = render_scanned(src, &tag, &config(80, 2, false));
        assert_eq!(out, "<br />");
    }

    // ---- exact-fit boundary --------------------------------------------

    #[test]
    fn e2e_exact_fit_boundary_two_attrs_stays_single_line() {
        // Two attributes so the boundary is exercised by the wrap/no-wrap split.
        // Rather than hard-code the exact tipping column (which depends on the
        // packer's internal budget), derive the single-line budget `fit` from
        // the scanned tag via `plan_layout`: the smallest width at which the tag
        // still fits on one line. At `fit` the tag stays single-line; at
        // `fit - 1` it must wrap onto two lines, each attribute kept whole.
        let src = "<a href=\"x\" id=\"y\">";
        let tag = scan_one_html(src, 2);

        assert_eq!(tag.attributes.len(), 2, "attrs: {:?}", tag.attributes);

        // Find the greedy-pack boundary: the smallest width at which both
        // attributes still land on the first line (no continuation line).
        // Below that width the second attribute is pushed to a continuation
        // line. We locate it by the rendered line count rather than hard-coding
        // the column, so the test tracks the packer's real geometry.
        let fit: u32 = (1u32..=80)
            .rev()
            .take_while(|&w| plan_layout(&tag, w as usize, 2).line_count() == 1)
            .last()
            .expect("there is a width at which both attributes fit on one line");

        // At `fit` the two attributes render on one line.
        let at_boundary = render_scanned(src, &tag, &config(fit, 2, false));
        assert_eq!(at_boundary, "<a href=\"x\" id=\"y\">");
        assert!(!at_boundary.contains('\n'), "W == fit keeps both on one line");

        // One column tighter forces the second attribute onto a continuation
        // line, each attribute kept whole, indented one unit (2 spaces).
        let over = render_scanned(src, &tag, &config(fit - 1, 2, false));
        assert_eq!(over, "<a href=\"x\"\n  id=\"y\">");
        assert!(over.contains('\n'), "W one tighter must wrap: {over:?}");
    }

    // ---- oversized single attribute ------------------------------------

    #[test]
    fn e2e_oversized_single_attribute_stays_on_name_line() {
        // A lone attribute far wider than W is never split and has nothing to be
        // pushed below it, so the whole tag stays on the `<name ...>` line even
        // though that line exceeds W. This over-width line is deterministic and
        // re-packs to itself, preserving idempotence.
        let src = "<div data-very-long=\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\">";
        let tag = scan_one_html(src, 2);
        let out = render_scanned(src, &tag, &config(20, 2, false));
        // No continuation break: one over-width line with the whole tag.
        assert!(!out.contains('\n'), "lone attribute is not broken out: {out:?}");
        assert_eq!(out, src);
        assert!(out.ends_with('>'));
    }

    #[test]
    fn e2e_oversized_attr_after_a_small_one_drops_to_its_own_line() {
        // With a preceding small attribute, the oversized one DOES move to its
        // own continuation line (the at-least-one-per-line rule), even though
        // that line still exceeds W.
        let src = "<div a=\"1\" data-huge=\"yyyyyyyyyyyyyyyyyyyyyyyyyyyyyy\">";
        let tag = scan_one_html(src, 2);
        let out = render_scanned(src, &tag, &config(20, 2, false));
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines.len(), 2, "name+small line, then huge line: {out:?}");
        assert_eq!(lines[0], "<div a=\"1\"");
        assert!(lines[1].trim_start().starts_with("data-huge="));
        assert!(out.ends_with('>'));
    }

    // ---- two-line wrap with known widths -------------------------------

    #[test]
    fn e2e_two_line_wrap_spaces() {
        // Four width-8 attributes at W = 24, indent_width 2, spaces.
        // Matches the unit-level packing: line 0 = a0 a1, line 1 = a2 a3,
        // cont_indent = 2 spaces.
        let src = "<p a0=\"xxx\" a1=\"xxx\" a2=\"xxx\" a3=\"xxx\">";
        let tag = scan_one_html(src, 2);
        let out = render_scanned(src, &tag, &config(24, 2, false));
        let expected = "<p a0=\"xxx\" a1=\"xxx\"\n  a2=\"xxx\" a3=\"xxx\">";
        assert_eq!(out, expected);
    }

    // ---- three-line wrap with known widths -----------------------------

    #[test]
    fn e2e_three_line_wrap_spaces() {
        // Six width-8 attributes at W = 24: three lines of two attributes each.
        let src = "<p ax=\"xxx\" bx=\"xxx\" cx=\"xxx\" dx=\"xxx\" ex=\"xxx\" fx=\"xxx\">";
        let tag = scan_one_html(src, 2);
        let out = render_scanned(src, &tag, &config(24, 2, false));
        let expected = concat!(
            "<p ax=\"xxx\" bx=\"xxx\"\n",
            "  cx=\"xxx\" dx=\"xxx\"\n",
            "  ex=\"xxx\" fx=\"xxx\">"
        );
        assert_eq!(out, expected);
    }

    // ---- tabs vs spaces indentation ------------------------------------

    #[test]
    fn e2e_two_line_wrap_tabs_vs_spaces() {
        // Identical layout, differing only in the continuation indentation:
        // spaces -> two spaces, tabs -> one tab at indent_width 2.
        let src = "<p a0=\"xxx\" a1=\"xxx\" a2=\"xxx\" a3=\"xxx\">";
        let tag = scan_one_html(src, 2);

        let spaces = render_scanned(src, &tag, &config(24, 2, false));
        assert_eq!(spaces, "<p a0=\"xxx\" a1=\"xxx\"\n  a2=\"xxx\" a3=\"xxx\">");

        let tabs = render_scanned(src, &tag, &config(24, 2, true));
        assert_eq!(tabs, "<p a0=\"xxx\" a1=\"xxx\"\n\ta2=\"xxx\" a3=\"xxx\">");
    }

    // ---- mid-expression start tag: bounded continuation indent ----------

    #[test]
    fn e2e_mid_expression_tag_is_not_wrapped_and_is_idempotent() {
        // A JSX tag starting mid-expression: `const x = <div ...>`. The `<` sits
        // at a large column (open_col) while the physical line's own indentation
        // (base_indent_col) is 0, so open_col != base_indent_col — a mid-line
        // tag. Such a tag is NEVER wrapped (Requirement 8 idempotence): there is
        // no continuation indent measured from the buffer that stays invariant
        // once a sibling on its line wraps, so wrapping it could oscillate across
        // passes. It is also the behavior the merits demand — the upstream
        // language formatter owns this expression line (Requirement 8.4). The tag
        // is emitted on one line even at a narrow width, and re-rendering the
        // output reproduces it byte-for-byte.
        let src =
            "const x = <div alpha=\"1\" bravo=\"2\" charlie=\"3\" delta=\"4\" echo=\"5\">;";
        let tag = scan_one(src, Dialect::Jsx, 2);

        // Sanity on the scanned geometry: `<` is far to the right, the line's own
        // indentation is at column 0, so this is unambiguously a mid-line tag.
        assert!(tag.open_col >= 10, "open_col should be large: {}", tag.open_col);
        assert_eq!(tag.base_indent_col, 0, "line indentation is column 0");
        assert_ne!(tag.open_col, tag.base_indent_col, "this must be a mid-line tag");

        // A narrow width that WOULD have forced a wrap under the old rule.
        let out = render_scanned(src, &tag, &config(40, 2, false));
        assert!(!out.contains('\n'), "a mid-line tag must not wrap: {out:?}");
        // The whole start tag is reproduced on one line, attributes verbatim.
        assert_eq!(out, "<div alpha=\"1\" bravo=\"2\" charlie=\"3\" delta=\"4\" echo=\"5\">");

        // Idempotence at the render level: rendering the output again is a no-op.
        let again = render_start_tag(&tag, &config(40, 2, false), Newline::detect(src), &out);
        assert_eq!(again, out, "mid-line single-line render must be idempotent");
    }

    // ---- self-closing terminator: canonical and idempotent --------------

    #[test]
    fn e2e_self_closing_terminator_is_canonical_space_slash_gt() {
        // Source uses no space (or several) before `/>`; the renderer normalizes
        // to exactly one space.
        let tight = "<img src=\"a\"/>";
        let tag = scan_one_html(tight, 2);
        let out = render_scanned(tight, &tag, &config(80, 2, false));
        assert_eq!(out, "<img src=\"a\" />");
        assert!(out.ends_with(" />"));
        assert!(!out.ends_with("  />"), "exactly one space before />");
    }

    #[test]
    fn e2e_self_closing_terminator_re_render_is_byte_identical() {
        // Full idempotence of the terminator through the scanner: render once,
        // feed the rendered output back through scan + render, and the terminator
        // (and the whole tag) must reproduce byte-for-byte.
        let src = "<input type=\"text\"  disabled   />";
        let cfg = config(80, 2, false);

        let first = {
            let tag = scan_one_html(src, 2);
            render_scanned(src, &tag, &cfg)
        };
        assert_eq!(first, "<input type=\"text\" disabled />");
        assert!(first.ends_with(" />"));

        // Re-scan the canonical output and render again: a true fixed point.
        let second = {
            let tag = scan_one_html(&first, 2);
            render_scanned(&first, &tag, &cfg)
        };
        assert_eq!(second, first, "re-rendering the output must be byte-identical");
    }

    #[test]
    fn e2e_wrapped_self_closing_terminator_idempotent() {
        // A self-closing tag that wraps across lines: the canonical ` />` lands on
        // the final continuation line and survives a re-scan + re-render.
        let src = "<input a0=\"xxx\" a1=\"xxx\" a2=\"xxx\" a3=\"xxx\"/>";
        let cfg = config(26, 2, false);

        let first = {
            let tag = scan_one_html(src, 2);
            render_scanned(src, &tag, &cfg)
        };
        assert!(first.contains('\n'), "should wrap at W = 26: {first:?}");
        assert!(first.ends_with(" />"), "canonical self-closing terminator: {first:?}");

        let second = {
            let tag = scan_one_html(&first, 2);
            render_scanned(&first, &tag, &cfg)
        };
        assert_eq!(second, first, "wrapped output must re-render byte-identically");
    }
}

#[cfg(test)]
mod continuation_indent_property_tests {
    //! Property 7: continuation lines are indented by exactly one extra unit.
    //!
    //! This suite exercises the full render path (`render_start_tag` with fluid
    //! wrapping enabled) over randomly generated start tags and asserts the
    //! continuation-indentation guarantee of Requirement 4.4: every continuation
    //! line of a wrapped start tag begins with exactly `base_indent_col +
    //! indent_width` columns of indentation — derived from the physical line's
    //! own leading-whitespace column (`base_indent_col`), **never** from
    //! `open_col`, which can be large for a mid-expression tag.
    //!
    //! It lives in its own module (not `mod tests`) so it does not collide with
    //! the example unit tests above while sharing the same production code under
    //! test.
    //!
    //! ## Why the render level
    //!
    //! Testing rendered output is the stronger end-to-end guarantee: it proves
    //! not only that the layout decision carries the right `cont_indent`
    //! (`plan_layout(...).cont_indent == base_indent_col + indent_width`), but
    //! that the renderer actually emits that many columns of indentation, and
    //! that both the spaces and tabs encodings map back to the same column count.
    //!
    //! ## Column accounting of an emitted indent
    //!
    //! Under the shared tab = `indent_width` column model, the leading
    //! indentation of a continuation line is a run of tabs and/or spaces. Its
    //! column count is `tabs * indent_width + spaces` (spaces mode is the special
    //! case `tabs == 0`). The helper [`leading_indent_columns`] recomputes that
    //! from the raw emitted bytes, independent of `build_indent`, so the property
    //! checks the renderer's output rather than trusting its own indent builder.

    use super::*;
    use crate::config::{Configuration, SchemeId};
    use crate::model::{display_width, Attribute, AttributeKind, ScannedTag};
    use proptest::prelude::*;

    /// Measure the display-column count of the leading indentation of `line`
    /// under the shared tab = `indent_width` model, recomputed independently of
    /// [`build_indent`]. A tab counts as `indent_width` columns, a space as one;
    /// measurement stops at the first non-indentation byte.
    fn leading_indent_columns(line: &str, indent_width: usize) -> usize {
        let unit = indent_width.max(1);
        let mut columns = 0usize;
        for ch in line.chars() {
            match ch {
                '\t' => columns += unit,
                ' ' => columns += 1,
                _ => break,
            }
        }
        columns
    }

    /// A single generated attribute borrowing a leaked `'static` ASCII token so
    /// the owning `ScannedTag<'static>` is trivial to build inside the property.
    /// Every `AttributeKind` is exercised; only `width` drives layout, so the raw
    /// text is kept simple and collision-free.
    fn attr_strategy() -> impl Strategy<Value = Attribute<'static>> {
        (
            prop::sample::select(vec![
                AttributeKind::Plain,
                AttributeKind::NameValue,
                AttributeKind::DynamicSpread,
            ]),
            "[a-z][a-z0-9-]{0,11}",
        )
            .prop_map(|(kind, raw)| {
                let leaked: &'static str = Box::leak(raw.into_boxed_str());
                Attribute {
                    kind,
                    name_key: None,
                    raw: leaked,
                    width: display_width(leaked, 2),
                }
            })
    }

    /// A generated start tag with random name, self-closing flag, geometry, and
    /// 2..=8 attributes.
    ///
    /// The attribute count is `>= 2` so that, paired with a tight line width in
    /// the property below, the tag is forced to wrap onto at least one
    /// continuation line — a single attribute can never be split off onto its
    /// own continuation line unless another precedes it, and the property is
    /// vacuous for tags that stay single-line.
    ///
    /// The generated tags are **line-leading** (`open_col == base_indent_col`),
    /// generated over a wide range of indentation columns. Only line-leading
    /// tags are wrapped (a mid-line tag is kept single-line for idempotence), so
    /// this is exactly the class whose continuation indent this property
    /// constrains: every continuation line is `base_indent_col + indent_width`
    /// columns — which, for a line-leading tag, is also `open_col + indent_width`
    /// since the two columns coincide. The old wide-open_col/small-base_indent
    /// shape (a mid-expression tag) is no longer wrapped and is covered instead
    /// by the dedicated `mid_line_tag_renders_on_a_single_line` /
    /// `e2e_mid_expression_tag_is_not_wrapped_and_is_idempotent` cases.
    fn tag_strategy() -> impl Strategy<Value = ScannedTag<'static>> {
        (
            "[a-z][a-z0-9]{0,7}",
            any::<bool>(),
            0usize..80,
            prop::collection::vec(attr_strategy(), 2..=8),
        )
            .prop_map(
                |(name, self_closing, col, attributes)| {
                    let name: &'static str = Box::leak(name.into_boxed_str());
                    ScannedTag {
                        span: 0..1,
                        name,
                        self_closing,
                        attributes,
                        confidently_rewritable: true,
                        // Line-leading: `<` is the line's first non-whitespace
                        // char, so its column equals the line indentation.
                        open_col: col,
                        base_indent_col: col,
                        children: Vec::new(),
                    }
                },
            )
    }

    /// Build a wrapping-enabled, sorting-disabled config with the given
    /// indentation settings.
    fn config(line_width: u32, indent_width: u8, use_tabs: bool) -> Configuration {
        Configuration {
            line_width,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width,
            use_tabs,
            vue: false,
            astro: false,
            svelte: false,
        }
    }

    proptest! {
        /// Property 7 (Validates: Requirements 4.4).
        ///
        /// For any generated start tag rendered with fluid wrapping enabled, at a
        /// line width tight enough to force a wrap, every continuation line (every
        /// line after the first) is indented by exactly `base_indent_col +
        /// indent_width` columns — in both spaces and tabs modes, and regardless
        /// of how large `open_col` is. The expected indent is derived solely from
        /// `base_indent_col` and `indent_width`, never from `open_col`.
        #[test]
        fn continuation_lines_indented_by_exactly_one_unit(
            tag in tag_strategy(),
            indent_width in 1u8..=8,
            use_tabs in any::<bool>(),
        ) {
            let indent_unit = usize::from(indent_width);
            let expected_indent = tag.base_indent_col + indent_unit;

            // Force a wrap: a line width of 1 cannot fit any multi-attribute tag
            // on a single line, so the packer must break onto continuation lines.
            let cfg = config(1, indent_width, use_tabs);
            let newline = Newline::Lf;

            // The layout must agree at the layout level, too: cont_indent is the
            // base indentation plus exactly one unit, independent of open_col.
            let layout = plan_layout(&tag, cfg.line_width as usize, indent_unit);
            prop_assert!(
                !layout.single_line,
                "width 1 with >=2 attributes must wrap: open_col={}, attrs={}",
                tag.open_col,
                tag.attributes.len()
            );
            prop_assert_eq!(
                layout.cont_indent,
                expected_indent,
                "cont_indent must be base_indent_col + indent_width, not open_col-derived"
            );

            // src_tag is unused on the wrapping-enabled path; pass an empty slice.
            let out = render_start_tag(&tag, &cfg, newline, "");

            let lines: Vec<&str> = out.split('\n').collect();
            prop_assert!(
                lines.len() >= 2,
                "a wrapped tag must emit at least one continuation line: {:?}",
                out
            );

            // Every continuation line (all lines after the first) is indented to
            // exactly base_indent_col + one unit under the shared column model.
            for line in lines.iter().skip(1) {
                let columns = leading_indent_columns(line, indent_unit);
                prop_assert_eq!(
                    columns,
                    expected_indent,
                    "continuation indent must equal base_indent_col({}) + indent_width({}) = {}, \
                     got {} columns in line {:?} (open_col={}, use_tabs={})",
                    tag.base_indent_col,
                    indent_unit,
                    expected_indent,
                    columns,
                    line,
                    tag.open_col,
                    use_tabs
                );
            }
        }
    }
}

#[cfg(test)]
mod attribute_set_property_tests {
    //! Property 8: the transformation preserves the attribute set.
    //!
    //! *For all* start tags, the multiset of attribute raw byte slices present
    //! after wrapping and/or sorting equals the multiset present in the input —
    //! no attribute is added, removed, split, or truncated — and dynamic/spread
    //! attribute slices appear verbatim in the output (Validates: Requirements
    //! 4.6, 5.3, 7.4).
    //!
    //! This suite exercises the real output stage, [`render_start_tag`], across
    //! the full configuration cross-product (wrapping on/off × sorting on/off ×
    //! tabs/spaces × a range of line widths), and when sorting is enabled it
    //! first runs the attributes through the production [`sort_attributes`] so
    //! the sort stage is part of the transformation under test. It then parses
    //! the attribute tokens back out of the rendered text and asserts the
    //! recovered multiset equals the input multiset exactly.
    //!
    //! ## Why token recovery is exact here
    //!
    //! The renderer only ever emits whitespace (single spaces, newlines, and
    //! continuation indentation) *between* attributes and in the `<name`/`>`
    //! scaffolding — never inside an attribute's `raw` slice, which is copied
    //! verbatim. The generators below produce attribute raws that contain **no
    //! ASCII whitespace** and that are **not** a prefix/suffix touching the tag
    //! scaffolding ambiguously, so stripping the `<name` prefix and the
    //! canonical terminator and then splitting the remainder on whitespace runs
    //! recovers exactly one token per attribute. Comparing the recovered tokens
    //! as a multiset (via a sorted `Vec`) therefore detects any add, drop,
    //! split, or truncation.

    use super::*;
    use crate::config::{Configuration, SchemeId};
    use crate::model::{display_width, Attribute, AttributeKind};
    use crate::sort::{sort_attributes, AttributeOrderScheme};
    use proptest::prelude::*;

    /// Fixed indentation unit used across the suite; the attribute-set identity
    /// is independent of the indent model, so pinning it keeps the generators
    /// simple while still exercising real column widths.
    const INDENT_WIDTH: usize = 2;
    /// The same indentation unit as [`INDENT_WIDTH`], typed for the `u8` config
    /// field so no lossy cast is needed.
    const INDENT_WIDTH_U8: u8 = 2;

    /// A generated attribute carried as an owned raw string plus its kind. The
    /// borrowed [`Attribute`] handed to the production code is built per-case
    /// from these so lifetimes stay simple.
    #[derive(Clone, Debug)]
    struct GenAttr {
        raw: String,
        kind: AttributeKind,
    }

    /// Strategy for one attribute. The raw text is whitespace-free ASCII so the
    /// rendered output can be split back into exactly one token per attribute.
    /// A `#` prefix index is woven in by the caller to make each slice distinct
    /// for the distinct-set case; the multiset case leaves raws free to collide.
    ///
    /// Kinds span every structurally meaningful variant — including
    /// [`AttributeKind::DynamicSpread`], whose slice must appear verbatim
    /// (Requirement 7.4) — but never [`AttributeKind::ComplexExpression`], since
    /// a single such attribute poisons the whole tag
    /// (`confidently_rewritable == false`) and the renderer then emits the tag
    /// byte-for-byte, which is a different property (Requirement 4.9).
    fn kind_strategy() -> impl Strategy<Value = AttributeKind> {
        prop::sample::select(vec![
            AttributeKind::Plain,
            AttributeKind::NameValue,
            AttributeKind::DynamicSpread,
        ])
    }

    /// Build a `GenAttr` from a raw body and kind, forcing the raw to be
    /// whitespace-free and non-empty so token recovery is unambiguous.
    fn gen_attr(raw: String, kind: AttributeKind) -> GenAttr {
        // Guard: a generated body could in principle be empty; make it non-empty
        // so an attribute never vanishes into the inter-attribute whitespace.
        let raw = if raw.is_empty() { "x".to_string() } else { raw };
        GenAttr { raw, kind }
    }

    /// A list of attributes with **distinct** raw slices. Distinctness is
    /// guaranteed by prefixing each raw with its index, so the sorted-token
    /// comparison is a clean set equality and no two tokens can alias.
    fn distinct_attrs_strategy() -> impl Strategy<Value = Vec<GenAttr>> {
        prop::collection::vec(("[a-z][a-z0-9_=.\"'-]{0,10}", kind_strategy()), 0..=8)
            .prop_map(|items| {
                items
                    .into_iter()
                    .enumerate()
                    .map(|(i, (body, kind))| gen_attr(format!("a{i}_{body}"), kind))
                    .collect()
            })
    }

    /// A list of attributes whose raws may **collide**, exercising true multiset
    /// semantics: the same raw slice may legitimately appear more than once
    /// (e.g. duplicate attributes), and the count of each must be preserved.
    fn multiset_attrs_strategy() -> impl Strategy<Value = Vec<GenAttr>> {
        // A small vocabulary of whitespace-free raws so collisions are frequent.
        let vocab = prop::sample::select(vec![
            "class=\"a\"",
            "class=\"b\"",
            "id=\"x\"",
            "disabled",
            "hidden",
            "data-k=\"v\"",
            "{...props}",
            "v-if=\"c\"",
        ]);
        prop::collection::vec((vocab, kind_strategy()), 0..=8).prop_map(|items| {
            items
                .into_iter()
                .map(|(raw, kind)| gen_attr(raw.to_string(), kind))
                .collect()
        })
    }

    /// A resolved configuration across the transformation cross-product.
    fn config(
        line_width: u32,
        fluid_wrapping: bool,
        attribute_sorting: bool,
        use_tabs: bool,
    ) -> Configuration {
        Configuration {
            line_width,
            fluid_wrapping,
            attribute_sorting,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: INDENT_WIDTH_U8,
            use_tabs,
            vue: false,
            astro: false,
            svelte: false,
        }
    }

    /// Build the borrowed `Attribute` list for a `ScannedTag`, measuring widths
    /// under the shared column model so `plan_layout` sees consistent geometry.
    fn attributes_from(gen: &[GenAttr]) -> Vec<Attribute<'_>> {
        gen.iter()
            .map(|g| Attribute {
                kind: g.kind,
                // `name_key` is only used by the sorter for grouping; a stable
                // lowercased key derived from the leading name chars is enough
                // and never affects the preserved `raw`.
                name_key: Some(name_key_of(&g.raw)),
                raw: g.raw.as_str(),
                width: display_width(&g.raw, INDENT_WIDTH),
            })
            .collect()
    }

    /// Derive a lowercased comparison name from a raw attribute slice: the
    /// leading run of name characters up to `=`, a space (none here), or `/`.
    /// Only used to give the sorter a plausible grouping key.
    fn name_key_of(raw: &str) -> String {
        let name: String = raw
            .chars()
            .take_while(|c| *c != '=' && *c != '/' && !c.is_whitespace())
            .collect();
        name.to_lowercase()
    }

    /// The multiset of raw slices, represented as a sorted `Vec` for comparison.
    fn raw_multiset(attrs: &[Attribute]) -> Vec<String> {
        let mut v: Vec<String> = attrs.iter().map(|a| a.raw.to_string()).collect();
        v.sort();
        v
    }

    /// Recover the attribute tokens from a rendered start tag.
    ///
    /// The rendered text is `<name`, then attribute tokens separated by single
    /// spaces / newlines / continuation indentation, then a terminator (`>` or
    /// ` />`). We strip the `<name` prefix and the terminator and split the
    /// remainder on ASCII whitespace; because every generated attribute raw is
    /// whitespace-free, each surviving token is exactly one attribute's `raw`.
    fn tokens_from_output(out: &str, name: &str, self_closing: bool) -> Vec<String> {
        // Strip the leading `<name`.
        let prefix = format!("<{name}");
        let body = out
            .strip_prefix(&prefix)
            .expect("rendered tag must start with `<name`");

        // Strip the canonical terminator from the end.
        let body = if self_closing {
            body.strip_suffix(" />")
                .or_else(|| body.strip_suffix("/>"))
                .expect("self-closing tag must end with the canonical terminator")
        } else {
            body.strip_suffix('>')
                .expect("non-self-closing tag must end with `>`")
        };

        let mut tokens: Vec<String> =
            body.split_whitespace().map(str::to_string).collect();
        tokens.sort();
        tokens
    }

    /// Build a `ScannedTag` from a name, self-closing flag, geometry, and the
    /// borrowed attribute list.
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
            children: Vec::new(),
        }
    }

    proptest! {
        // Feature: fluid-attribute-wrapping-plugin, Property 8: Transformation
        // preserves the attribute set.

        /// Distinct-raw case: the recovered token set equals the input set
        /// exactly — nothing added, dropped, split, or truncated — across the
        /// full wrapping/sorting/tabs/width cross-product. When sorting is on
        /// the attributes first pass through the production sorter, so the sort
        /// stage is part of the transformation under test (Requirement 5.3).
        #[test]
        fn render_preserves_distinct_attribute_set(
            gen in distinct_attrs_strategy(),
            name in "[a-z][a-z0-9]{0,6}",
            self_closing in any::<bool>(),
            open_col in 0usize..40,
            base_indent_col in 0usize..12,
            line_width in 1u32..60,
            fluid_wrapping in any::<bool>(),
            attribute_sorting in any::<bool>(),
            use_tabs in any::<bool>(),
        ) {
            let name: &str = &name;
            let cfg = config(line_width, fluid_wrapping, attribute_sorting, use_tabs);

            // The attributes as they enter rendering: sorted first when sorting
            // is enabled (so the sort stage participates), else source order.
            let input_attrs = attributes_from(&gen);
            let pipeline_attrs: Vec<Attribute> = if attribute_sorting {
                sort_attributes(&input_attrs, &AttributeOrderScheme::standard())
            } else {
                input_attrs.clone()
            };

            // The input multiset is the generated set (order-independent).
            let expected = raw_multiset(&input_attrs);
            // Sorting must itself preserve the multiset (Requirement 5.3).
            prop_assert_eq!(
                raw_multiset(&pipeline_attrs),
                expected.clone(),
                "sorting changed the attribute multiset"
            );

            let t = tag(name, self_closing, open_col, base_indent_col, pipeline_attrs);
            // `src_tag` only matters when wrapping is disabled; build a faithful
            // single-line source so the wrapping-disabled path can recover the
            // upstream separators from it.
            let src_tag = render_src(&t);
            let out = render_start_tag(&t, &cfg, Newline::Lf, &src_tag);

            let recovered = tokens_from_output(&out, name, self_closing);
            prop_assert_eq!(
                recovered,
                expected,
                "rendered attribute set must equal the input set; out={:?}",
                out
            );
        }

        /// Multiset case: raws may legitimately repeat (duplicates), and the
        /// count of each distinct raw must be preserved exactly. This is the
        /// true multiset formulation of Property 8 — a dropped or duplicated
        /// attribute changes a count and fails the sorted-token comparison.
        #[test]
        fn render_preserves_attribute_multiset_with_duplicates(
            gen in multiset_attrs_strategy(),
            name in "[a-z][a-z0-9]{0,6}",
            self_closing in any::<bool>(),
            open_col in 0usize..40,
            base_indent_col in 0usize..12,
            line_width in 1u32..60,
            fluid_wrapping in any::<bool>(),
            use_tabs in any::<bool>(),
        ) {
            let name: &str = &name;
            // Sorting is left off here: the stable sorter could legitimately
            // reorder duplicates, but the multiset (sorted tokens) is identical
            // either way, so this case focuses purely on wrap/render preserving
            // counts. The distinct case above already covers sorting on.
            let cfg = config(line_width, fluid_wrapping, false, use_tabs);

            let input_attrs = attributes_from(&gen);
            let expected = raw_multiset(&input_attrs);

            let t = tag(name, self_closing, open_col, base_indent_col, input_attrs);
            let src_tag = render_src(&t);
            let out = render_start_tag(&t, &cfg, Newline::Lf, &src_tag);

            let recovered = tokens_from_output(&out, name, self_closing);
            prop_assert_eq!(
                recovered,
                expected,
                "rendered multiset must equal the input multiset; out={:?}",
                out
            );
        }

        /// Dynamic/spread slices appear verbatim (Requirement 7.4): a tag
        /// containing a spread attribute renders that spread's exact bytes as a
        /// contiguous substring of the output, never reconstructed or split.
        #[test]
        fn dynamic_spread_slice_appears_verbatim(
            gen in distinct_attrs_strategy(),
            name in "[a-z][a-z0-9]{0,6}",
            self_closing in any::<bool>(),
            open_col in 0usize..40,
            base_indent_col in 0usize..12,
            line_width in 1u32..60,
            fluid_wrapping in any::<bool>(),
            use_tabs in any::<bool>(),
        ) {
            let name: &str = &name;
            let cfg = config(line_width, fluid_wrapping, false, use_tabs);

            // Ensure at least one spread attribute with a recognizable slice.
            let mut gen = gen;
            gen.push(gen_attr("{...spread0}".to_string(), AttributeKind::DynamicSpread));

            let input_attrs = attributes_from(&gen);
            let t = tag(name, self_closing, open_col, base_indent_col, input_attrs);
            let src_tag = render_src(&t);
            let out = render_start_tag(&t, &cfg, Newline::Lf, &src_tag);

            prop_assert!(
                out.contains("{...spread0}"),
                "spread slice must appear verbatim in output; out={:?}",
                out
            );
        }
    }

    /// Build a faithful single-line source slice for a tag, used as `src_tag`
    /// for the wrapping-disabled path. It mirrors what the scanner would have
    /// produced: `<name` then single-space-separated raws then the terminator.
    /// Because the generated raws are whitespace-free and distinct (or drawn
    /// from a whitespace-free vocabulary), the renderer's upstream-separator
    /// recovery locates each slice unambiguously.
    fn render_src(t: &ScannedTag) -> String {
        let mut s = String::new();
        s.push('<');
        s.push_str(t.name);
        for a in &t.attributes {
            s.push(' ');
            s.push_str(a.raw);
        }
        s.push_str(if t.self_closing { " />" } else { ">" });
        s
    }
}

#[cfg(test)]
mod multibyte_boundary_exploration {
    //! Bugfix expected-behavior tests (render-multibyte-char-boundary-panic,
    //! task 1, flipped by task 3.2).
    //!
    //! These started life as task-1 *exploratory reproducers* that PANICKED on
    //! the UNFIXED code (`byte index N is not a char boundary` in
    //! [`leading_breaks_by_attribute`] on the **sorting-on / wrapping-off**
    //! render path). Task 3.1 applied the one-line char-boundary fix (`i + 1`
    //! replaced by `i + c.len_utf8()`), and task 3.2 flips each of these SAME
    //! constructs from a panic-assertion (`#[should_panic(expected = "char
    //! boundary")]`) to an assertion that the drive call now **completes** and
    //! returns a well-formed `String` with the leading break classified
    //! correctly. The constructs (tags, attribute `raw` slices, `src_tag`
    //! strings, and the sorting-on / wrapping-off `config`) are unchanged — only
    //! the attributes and bodies are flipped to the corrected (F') behavior.
    //!
    //! ## Root cause (now fixed)
    //!
    //! `leading_breaks_by_attribute` computes the end offset of the last
    //! non-whitespace character before a resolved attribute occurrence. The
    //! UNFIXED code used:
    //!
    //! ```ignore
    //! let last_nonspace = prefix
    //!     .rfind(|c: char| !c.is_whitespace())
    //!     .map_or(0, |i| i + 1);
    //! src_tag[last_nonspace..rel].contains('\n')
    //! ```
    //!
    //! `str::rfind` with a closure returns the **start byte** of the matched
    //! `char`, so `i + 1` is "one past the start", which equals "one past the
    //! end" only for a single-byte (ASCII) char. When the last non-whitespace
    //! char before the attribute was multibyte (e.g. `¡`, 2 bytes), `i + 1`
    //! landed inside that char and the slice `src_tag[last_nonspace..rel]`
    //! crossed a non-char-boundary and panicked. The fixed code computes
    //! `i + c.len_utf8()` — one past the END of the matched char, a UTF-8
    //! boundary by construction — so the slice is boundary-safe for any valid
    //! UTF-8 and no longer panics.
    //!
    //! ## Construction requirement (from the task's CRITICAL note)
    //!
    //! For the original panic to fire, the resolved attribute occurrence had to
    //! be preceded — through a whitespace run — by a multibyte **non-whitespace**
    //! char that is the LAST non-whitespace char before the attribute. The
    //! illustrative literal strings in the spec (`<a¡b c="1" d="2">`,
    //! `<div dáta="x" id="y">`) do not themselves trip the bug, because the last
    //! non-whitespace char before the first resolved attribute is ASCII (`b`,
    //! `a`). These tests therefore honor the binding CRITICAL construction note
    //! rather than the illustrative example strings: each `src_tag` places a
    //! multibyte char as the last non-whitespace byte immediately before the
    //! whitespace run preceding the attribute.
    //!
    //! ## Recorded corrected outputs (observed on FIXED code, F')
    //!
    //! Each drive call below now completes and returns a well-formed `String`.
    //! The sorting-on / wrapping-off path separates attributes with a single
    //! canonical space unless the attribute's source occurrence was preceded by
    //! a `\n`-containing whitespace run; the first emitted attribute always
    //! follows `<name` with a single space. For these constructs:
    //!
    //! * `<a¡ c="1" d="2">` — `c="1"` and `d="2"` are each preceded only by a
    //!   plain space (no `\n`), so both are separated by a single space:
    //!   output `<a c="1" d="2">`.
    //! * `<div datá id="y">` — the single attribute `id="y"` is first and
    //!   preceded only by a plain space: output `<div id="y">`.
    //! * `<a ñ\n  c="1">` — the single attribute `c="1"` is first, and a leading
    //!   break before the first attribute is dropped (the head stays canonical),
    //!   so even though its source whitespace run contains `\n` the attribute
    //!   follows `<name` with a single space: output `<a c="1">`. The fixed slice
    //!   `src_tag[last_nonspace..rel]` is now boundary-safe past the two-byte
    //!   `ñ`, so the `.contains('\n')` classification runs without panicking.

    use super::*;
    use crate::config::{Configuration, SchemeId};
    use crate::model::{display_width, Attribute, AttributeKind, ScannedTag};

    /// Build a `NameValue` attribute whose `raw` slice the renderer will match
    /// against the custom `src_tag`. Width is measured at `indent_width = 2`.
    fn attr(raw: &'static str) -> Attribute<'static> {
        Attribute {
            kind: AttributeKind::NameValue,
            name_key: None,
            raw,
            width: display_width(raw, 2),
        }
    }

    /// Build a `ScannedTag` on the sorting-on / wrapping-off path. The `span` is
    /// unused by `render_wrapping_disabled` (which takes `src_tag` directly), so
    /// a placeholder range is fine.
    fn tag(name: &'static str, attributes: Vec<Attribute<'static>>) -> ScannedTag<'static> {
        ScannedTag {
            span: 0..1,
            name,
            self_closing: false,
            attributes,
            confidently_rewritable: true,
            open_col: 0,
            base_indent_col: 0,
            children: Vec::new(),
        }
    }

    /// A config that forces the buggy path: sorting ENABLED, wrapping DISABLED,
    /// and a tiny `line_width` so the renderer reaches
    /// `leading_breaks_by_attribute` rather than short-circuiting.
    fn config() -> Configuration {
        Configuration {
            line_width: 1,
            fluid_wrapping: false,
            attribute_sorting: true,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: false,
            astro: false,
            svelte: false,
        }
    }

    /// Primary construct — multibyte head immediately before an attribute.
    ///
    /// `src_tag = "<a¡ c=\"1\" d=\"2\">"`: the two-byte `¡` is the last
    /// non-whitespace char before the whitespace preceding `c="1"`. On UNFIXED
    /// code, `leading_breaks_by_attribute` computed `last_nonspace = i + 1`,
    /// which landed inside `¡`, and `src_tag[last_nonspace..rel]` panicked. Under
    /// the FIXED code (F') the drive call completes: neither `c="1"` nor `d="2"`
    /// is preceded by a `\n`-containing whitespace run, so both attributes are
    /// separated by a single canonical space.
    #[test]
    fn multibyte_head_before_attribute_renders_without_panic() {
        let t = tag("a", vec![attr("c=\"1\""), attr("d=\"2\"")]);
        let src_tag = "<a¡ c=\"1\" d=\"2\">";
        // Completes (no panic) on FIXED code and classifies both leading breaks
        // as spaces.
        let out = render_start_tag(&t, &config(), Newline::Lf, src_tag);
        assert_eq!(out, "<a c=\"1\" d=\"2\">");
    }

    /// Multibyte in the prior token — the previous token ends in a multibyte
    /// char.
    ///
    /// `src_tag = "<div datá id=\"y\">"`: the token `datá` ends in the two-byte
    /// `á`, the last non-whitespace char before the whitespace preceding
    /// `id="y"`. `i + 1` landed inside `á` and panicked on UNFIXED code. Under
    /// the FIXED code the drive call completes: the single attribute `id="y"` is
    /// first and preceded only by a plain space, so it follows `<name` with a
    /// single space.
    #[test]
    fn multibyte_in_prior_token_renders_without_panic() {
        let t = tag("div", vec![attr("id=\"y\"")]);
        let src_tag = "<div datá id=\"y\">";
        let out = render_start_tag(&t, &config(), Newline::Lf, src_tag);
        assert_eq!(out, "<div id=\"y\">");
    }

    /// Multibyte before a newline-separated attribute — exercises the exact
    /// `src_tag[last_nonspace..rel].contains('\n')` slice.
    ///
    /// `src_tag = "<a ñ\n  c=\"1\">"`: the two-byte `ñ` precedes a whitespace run
    /// that contains a `\n` before `c="1"`. On UNFIXED code the off-boundary
    /// slice panicked before the `.contains('\n')` newline classification could
    /// run. Under the FIXED code the slice is boundary-safe past `ñ`, so the
    /// classification runs: `c="1"` is the first (and only) attribute, and a
    /// leading break before the first attribute is dropped to keep the head
    /// canonical, so it follows `<name` with a single space.
    #[test]
    fn multibyte_before_newline_separated_attribute_renders_without_panic() {
        let t = tag("a", vec![attr("c=\"1\"")]);
        let src_tag = "<a ñ\n  c=\"1\">";
        let out = render_start_tag(&t, &config(), Newline::Lf, src_tag);
        assert_eq!(out, "<a c=\"1\">");
    }
}

#[cfg(test)]
mod preservation_baseline {
    //! Bugfix preservation baseline (render-multibyte-char-boundary-panic, task 2).
    //!
    //! Written BEFORE the one-line fix in [`leading_breaks_by_attribute`], this
    //! module pins the UNFIXED output `F` for *non-buggy* inputs (`¬C`) so that
    //! task 4 can prove `F'(X) = F(X)` byte-for-byte after the fix lands. These
    //! tests MUST PASS on the UNFIXED code — they capture the behavior the fix
    //! must preserve, not behavior it must change.
    //!
    //! The fix only ever changes the end-offset arithmetic for the *last
    //! non-whitespace char before an attribute* from `i + 1` to
    //! `i + <that char>.len_utf8()`. For ASCII that char is one byte wide, so
    //! `i + 1 == i + len_utf8()` and every recorded string below is unaffected
    //! (Bug 3.1). Config paths that never call `leading_breaks_by_attribute`
    //! (fluid wrapping enabled, or sorting disabled — Bug 3.4) and the
    //! spread / not-confidently-rewritable verbatim paths (Bug 3.5) are also
    //! recorded here as a guard, since the fix must leave them untouched.
    //!
    //! ## Recorded UNFIXED outputs (observed on this code)
    //!
    //! For the ASCII tag `<div id="b"\n  data-x="a"` with attributes supplied in
    //! sorted order `[id="b", data-x="a"]`:
    //!
    //! * sorting on / wrapping off  -> `<div id="b"\ndata-x="a">`
    //!   (the break bound to `data-x`, the non-first sorted attribute, is kept;
    //!   the first attribute follows `<name` with a single space). This is the
    //!   exact `i + 1 == i + len_utf8()` case the fix must not disturb.
    //! * fluid wrapping enabled      -> `<div id="b"\n  data-x="a">`
    //!   (canonical wrapped path; does not call `leading_breaks_by_attribute`).
    //! * attribute sorting disabled  -> `<div id="b"\n  data-x="a"`
    //!   (verbatim `src_tag`; does not call `leading_breaks_by_attribute`).

    use super::*;
    use crate::config::{Configuration, SchemeId};
    use crate::model::{display_width, Attribute, AttributeKind, ScannedTag};

    /// Build a `NameValue` attribute with width measured at `indent_width = 2`.
    fn attr(raw: &'static str) -> Attribute<'static> {
        Attribute {
            kind: AttributeKind::NameValue,
            name_key: None,
            raw,
            width: display_width(raw, 2),
        }
    }

    /// Build a `DynamicSpread` attribute whose raw bytes must be preserved verbatim.
    fn spread_attr(raw: &'static str) -> Attribute<'static> {
        Attribute {
            kind: AttributeKind::DynamicSpread,
            name_key: None,
            raw,
            width: display_width(raw, 2),
        }
    }

    /// Build a `ScannedTag`. The `span` is unused on the wrapping-disabled path
    /// (which takes `src_tag` directly), so a placeholder range is fine.
    fn tag(
        name: &'static str,
        self_closing: bool,
        attributes: Vec<Attribute<'static>>,
    ) -> ScannedTag<'static> {
        ScannedTag {
            span: 0..1,
            name,
            self_closing,
            attributes,
            confidently_rewritable: true,
            open_col: 0,
            base_indent_col: 0,
            children: Vec::new(),
        }
    }

    /// The buggy-path config: sorting ENABLED, wrapping DISABLED, tiny width so
    /// `leading_breaks_by_attribute` is reached. Mirrors the task-1 module's
    /// `config()`.
    fn config() -> Configuration {
        Configuration {
            line_width: 1,
            fluid_wrapping: false,
            attribute_sorting: true,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: false,
            astro: false,
            svelte: false,
        }
    }

    // ---- Bug 3.1 / 3.2: ASCII sorting-on / wrapping-off is the preserved
    // `i + 1 == i + len_utf8()` case ------------------------------------------

    /// ASCII tag, sorting on / wrapping off, with an upstream break bound to the
    /// non-first sorted attribute (`data-x`). This is exactly the code path the
    /// fix touches, but with ASCII input the computed offset is identical before
    /// and after the fix, so the recorded output must stay byte-for-byte stable.
    #[test]
    fn ascii_sorting_on_wrapping_off_is_byte_for_byte_recorded() {
        let t = tag(
            "div",
            false,
            vec![attr("id=\"b\""), attr("data-x=\"a\"")],
        );
        // Source order id then data-x; the break precedes data-x (non-first).
        let src = "<div id=\"b\"\n  data-x=\"a\"";
        let out = render_start_tag(&t, &config(), Newline::Lf, src);
        // Recorded UNFIXED output: break bound to data-x preserved as a lone
        // newline; the first attribute follows `<name` with a single space.
        assert_eq!(out, "<div id=\"b\"\ndata-x=\"a\">");
    }

    // ---- Bug 3.3: idempotence baseline for the ASCII sorting-on path ---------

    /// `render_start_tag(render_start_tag(x)) == render_start_tag(x)` on the
    /// ASCII sorting-on / wrapping-off path: re-rendering the first output (now
    /// a fixed layout) reproduces it byte-for-byte.
    #[test]
    fn ascii_sorting_on_wrapping_off_is_idempotent() {
        let t = tag(
            "div",
            false,
            vec![attr("id=\"b\""), attr("data-x=\"a\"")],
        );
        let src = "<div id=\"b\"\n  data-x=\"a\"";
        let first = render_start_tag(&t, &config(), Newline::Lf, src);
        let second = render_start_tag(&t, &config(), Newline::Lf, &first);
        assert_eq!(second, first, "sorting-on / wrapping-off path must be a fixed point");
        assert_eq!(first, "<div id=\"b\"\ndata-x=\"a\">");
    }

    // ---- Bug 3.4: config paths that never call leading_breaks_by_attribute ---

    /// Same ASCII tag with `fluid_wrapping = true` takes the wrapped render path,
    /// which does NOT call `leading_breaks_by_attribute`; its canonical output is
    /// recorded here so the fix is shown to leave it untouched.
    #[test]
    fn fluid_wrapping_enabled_path_output_recorded_unchanged() {
        let t = tag(
            "div",
            false,
            vec![attr("id=\"b\""), attr("data-x=\"a\"")],
        );
        let src = "<div id=\"b\"\n  data-x=\"a\"";
        let mut cfg = config();
        cfg.fluid_wrapping = true;
        let out = render_start_tag(&t, &cfg, Newline::Lf, src);
        // Canonical wrapped output (2-space continuation indent); independent of
        // leading_breaks_by_attribute.
        assert_eq!(out, "<div id=\"b\"\n  data-x=\"a\">");
    }

    /// Same ASCII tag with `attribute_sorting = false` takes the verbatim path,
    /// which returns `src_tag` unchanged and never calls
    /// `leading_breaks_by_attribute`.
    #[test]
    fn attribute_sorting_disabled_path_is_verbatim() {
        let t = tag(
            "div",
            false,
            vec![attr("id=\"b\""), attr("data-x=\"a\"")],
        );
        let src = "<div id=\"b\"\n  data-x=\"a\"";
        let mut cfg = config();
        cfg.attribute_sorting = false;
        let out = render_start_tag(&t, &cfg, Newline::Lf, src);
        assert_eq!(out, src, "sorting-disabled path returns src_tag verbatim");
    }

    // ---- Bug 3.5: spread and not-confidently-rewritable verbatim paths -------

    /// A tag with a spread attribute is still rewritten structurally, but the
    /// spread's raw bytes are preserved verbatim; recorded here under the
    /// sorting-on / wrapping-off config so the fix is shown not to disturb it.
    /// The spread value contains a multibyte char to confirm its raw slice is
    /// emitted untouched regardless of the boundary arithmetic.
    #[test]
    fn spread_attribute_raw_is_preserved() {
        let t = tag(
            "div",
            false,
            vec![attr("id=\"b\""), spread_attr("{...café}")],
        );
        let src = "<div id=\"b\" {...café}";
        let out = render_start_tag(&t, &config(), Newline::Lf, src);
        // Both attributes present verbatim, single-space separated (no upstream
        // break), canonical terminator.
        assert_eq!(out, "<div id=\"b\" {...café}>");
    }

    /// A not-confidently-rewritable tag emits `src_tag` byte-for-byte, never
    /// reaching `leading_breaks_by_attribute`, even with a multibyte char present.
    #[test]
    fn not_confidently_rewritable_tag_is_verbatim() {
        let mut t = tag("div", true, vec![attr("b")]);
        t.confidently_rewritable = false;
        let src = "<div a={x > yñ} b />";
        let out = render_start_tag(&t, &config(), Newline::Lf, src);
        assert_eq!(out, src, "not-confidently-rewritable tag is emitted verbatim");
    }
}

#[cfg(test)]
mod classification_equivalence {
    //! Bugfix classification-equivalence regression (render-multibyte-char-boundary-panic,
    //! task 4) — Property 4 / bug 2.4.
    //!
    //! The one-line fix in [`leading_breaks_by_attribute`] changed only the byte
    //! arithmetic for the end offset of the last non-whitespace char before a
    //! resolved attribute (`i + 1` -> `i + c.len_utf8()`). It must NOT change the
    //! `\n`-presence decision: whether an attribute's leading whitespace run
    //! counts as a "break" (contains `\n`) or a "space" (does not) is a property
    //! of the whitespace run, independent of how wide the preceding non-space
    //! char happens to be.
    //!
    //! Each test below pairs an **ASCII** `src_tag` with its **multibyte analog**
    //! — the single-byte non-whitespace char immediately before the attribute's
    //! whitespace run is replaced by a multibyte non-whitespace char, keeping the
    //! **same whitespace run** — and asserts that the leading-break classification
    //! is identical for the pair. Because the multibyte char is the last
    //! non-whitespace char before the run, it is exactly the char whose width the
    //! fix now measures; if the fix had disturbed the `\n` decision (or, before
    //! the fix, panicked), these pairs would diverge.
    //!
    //! Two regimes are covered so the equivalence is meaningful rather than
    //! trivially "space everywhere":
    //!
    //! * **break regime** — the whitespace run BEFORE the (non-first) attribute
    //!   CONTAINS a `\n`, so the classification is `true` ("break") for both the
    //!   ASCII and multibyte analogs.
    //! * **space regime** — the whitespace run contains no `\n`, so the
    //!   classification is `false` ("space") for both analogs.
    //!
    //! The classification is asserted two ways: directly against the private
    //! [`leading_breaks_by_attribute`] (reachable from this in-crate test module),
    //! and via the observable break-vs-space result of [`render_start_tag`] on the
    //! sorting-on / wrapping-off path.

    use super::*;
    use crate::config::{Configuration, SchemeId};
    use crate::model::{display_width, Attribute, AttributeKind, ScannedTag};

    /// Build a `NameValue` attribute with width measured at `indent_width = 2`.
    fn attr(raw: &'static str) -> Attribute<'static> {
        Attribute {
            kind: AttributeKind::NameValue,
            name_key: None,
            raw,
            width: display_width(raw, 2),
        }
    }

    /// Build a `ScannedTag` on the sorting-on / wrapping-off path. `span` is
    /// unused by `render_wrapping_disabled` (it takes `src_tag` directly).
    fn tag(name: &'static str, attributes: Vec<Attribute<'static>>) -> ScannedTag<'static> {
        ScannedTag {
            span: 0..1,
            name,
            self_closing: false,
            attributes,
            confidently_rewritable: true,
            open_col: 0,
            base_indent_col: 0,
            children: Vec::new(),
        }
    }

    /// The buggy-path config: sorting ENABLED, wrapping DISABLED, tiny width so
    /// `leading_breaks_by_attribute` is reached.
    fn config() -> Configuration {
        Configuration {
            line_width: 1,
            fluid_wrapping: false,
            attribute_sorting: true,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: false,
            astro: false,
            svelte: false,
        }
    }

    // ---- break regime: whitespace run before the attribute contains `\n` -----

    /// Break regime, direct check against the private classifier.
    ///
    /// Two attributes `x="1"` then `y="2"`. The break precedes the second,
    /// non-first attribute `y="2"` (so the classification is actually consulted —
    /// a break before the first attribute would be dropped). The last
    /// non-whitespace char before that `\n`-containing run is, in the ASCII
    /// analog, the single-byte `"` that closes `x="1"`; in the multibyte analog
    /// that same closing region is followed by a multibyte non-whitespace marker
    /// `¡` sitting immediately before the whitespace run. Both runs contain `\n`,
    /// so both classify the second attribute as a break.
    #[test]
    fn break_regime_classifies_identically_direct() {
        let t = tag("div", vec![attr("x=\"1\""), attr("y=\"2\"")]);

        // ASCII analog: `x="1"` is the last non-whitespace token; the run before
        // `y="2"` contains `\n`.
        let ascii = "<div x=\"1\"\n  y=\"2\">";
        // Multibyte analog: the single-byte non-whitespace char (`\u{2b}`-class
        // marker) immediately before the whitespace run is a multibyte `¡`,
        // keeping the SAME `\n`-containing whitespace run.
        let multibyte = "<div x=\"1\"¡\n  y=\"2\">";

        let ascii_breaks = leading_breaks_by_attribute(ascii, &t);
        let multibyte_breaks = leading_breaks_by_attribute(multibyte, &t);

        // Equivalence: identical classification vectors for the pair.
        assert_eq!(
            ascii_breaks, multibyte_breaks,
            "fix must not change the \\n decision: ascii={ascii_breaks:?} multibyte={multibyte_breaks:?}"
        );
        // And the classification is the meaningful "break" one: the second
        // attribute is preceded by a `\n`.
        assert_eq!(
            multibyte_breaks,
            vec![false, true],
            "second attribute's leading run contains \\n -> break"
        );
    }

    /// Break regime, observed via rendered break-vs-space output.
    ///
    /// On the sorting-on / wrapping-off path a preserved break is emitted as a
    /// lone `\n` before the bound attribute; every other separator is a single
    /// space. The ASCII and multibyte analogs must therefore render the same
    /// break-vs-space shape: `x="1"` first (single space after `<div`), then a
    /// `\n` before `y="2"`.
    #[test]
    fn break_regime_renders_identically() {
        let t = tag("div", vec![attr("x=\"1\""), attr("y=\"2\"")]);
        let ascii = "<div x=\"1\"\n  y=\"2\">";
        let multibyte = "<div x=\"1\"¡\n  y=\"2\">";

        let ascii_out = render_start_tag(&t, &config(), Newline::Lf, ascii);
        let multibyte_out = render_start_tag(&t, &config(), Newline::Lf, multibyte);

        assert_eq!(
            ascii_out, multibyte_out,
            "rendered break-vs-space must match: ascii={ascii_out:?} multibyte={multibyte_out:?}"
        );
        // The meaningful break is present: a lone newline before `y="2"`.
        assert_eq!(multibyte_out, "<div x=\"1\"\ny=\"2\">");
        assert!(multibyte_out.contains('\n'), "break preserved before y");
    }

    // ---- space regime: whitespace run before the attribute has no `\n` -------

    /// Space regime, direct check against the private classifier.
    ///
    /// Same two attributes, but now the whitespace run before `y="2"` is a plain
    /// space with no `\n`. The multibyte analog again places a multibyte `¡` as
    /// the last non-whitespace char before that run. Both classify the second
    /// attribute as a space (no break).
    #[test]
    fn space_regime_classifies_identically_direct() {
        let t = tag("div", vec![attr("x=\"1\""), attr("y=\"2\"")]);
        let ascii = "<div x=\"1\" y=\"2\">";
        let multibyte = "<div x=\"1\"¡ y=\"2\">";

        let ascii_breaks = leading_breaks_by_attribute(ascii, &t);
        let multibyte_breaks = leading_breaks_by_attribute(multibyte, &t);

        assert_eq!(
            ascii_breaks, multibyte_breaks,
            "fix must not change the \\n decision: ascii={ascii_breaks:?} multibyte={multibyte_breaks:?}"
        );
        // No `\n` anywhere -> no break for either attribute.
        assert_eq!(
            multibyte_breaks,
            vec![false, false],
            "no \\n in the leading runs -> all spaces"
        );
    }

    /// Space regime, observed via rendered output: both analogs render on a
    /// single line, single-space separated, no `\n`.
    #[test]
    fn space_regime_renders_identically() {
        let t = tag("div", vec![attr("x=\"1\""), attr("y=\"2\"")]);
        let ascii = "<div x=\"1\" y=\"2\">";
        let multibyte = "<div x=\"1\"¡ y=\"2\">";

        let ascii_out = render_start_tag(&t, &config(), Newline::Lf, ascii);
        let multibyte_out = render_start_tag(&t, &config(), Newline::Lf, multibyte);

        assert_eq!(
            ascii_out, multibyte_out,
            "rendered output must match: ascii={ascii_out:?} multibyte={multibyte_out:?}"
        );
        assert_eq!(multibyte_out, "<div x=\"1\" y=\"2\">");
        assert!(!multibyte_out.contains('\n'), "no break -> single line");
    }
}

#[cfg(test)]
mod multibyte_boundary_property_tests {
    //! Bugfix property coverage (render-multibyte-char-boundary-panic, task 5).
    //!
    //! Two `proptest` properties pin the fixed behavior of the sorting-on /
    //! wrapping-off render path over arbitrary valid UTF-8 — the exact path that
    //! panicked on the unfixed `leading_breaks_by_attribute` (`i + 1` landing
    //! inside a multibyte char) and that now computes `i + c.len_utf8()`:
    //!
    //! * (a) **Never-panic** — for arbitrary valid UTF-8 `Start_Tags` (including
    //!   multibyte element names and multibyte bytes woven into the `src_tag`
    //!   head and inter-attribute gaps), [`render_start_tag`] with
    //!   `attribute_sorting = true` and `fluid_wrapping = false` at arbitrary
    //!   small widths SHALL complete without panic. A `proptest`-caught panic
    //!   fails the property with a minimized counterexample (Bug 2.1, 2.2, 2.3).
    //! * (b) **Tag-level idempotence** —
    //!   `render_start_tag(render_start_tag(x)) == render_start_tag(x)` on the
    //!   same sorting-on / wrapping-off path, so the attribute-bound-break fixed
    //!   point holds for multibyte input exactly as it already does for ASCII
    //!   (Bug 3.2, 3.3).
    //!
    //! ## Driving the buggy path on purpose
    //!
    //! `leading_breaks_by_attribute` is reached only when sorting is ENABLED and
    //! wrapping is DISABLED, so every config built here states that explicitly:
    //! `attribute_sorting = true`, `fluid_wrapping = false`. The function then
    //! resolves each attribute's `raw` slice inside `src_tag` via `find_from` and
    //! slices `src_tag[last_nonspace..rel]` back to the previous non-whitespace
    //! char. To actually exercise the multibyte boundary arithmetic, the
    //! generators do NOT emit random strings (which would rarely resolve an
    //! attribute). Instead each case:
    //!
    //! 1. generates a base set of **ASCII, whitespace-free** attribute `raw`
    //!    slices (so each resolves unambiguously in `src_tag`);
    //! 2. builds `src_tag = <name` + per-attribute (separator) + `raw` +
    //!    terminator, where the element name and the separators MAY carry
    //!    multibyte non-whitespace chars placed immediately before the whitespace
    //!    run that precedes an attribute — so the char whose width the fix now
    //!    measures is multibyte and `last_nonspace` must land on its trailing
    //!    boundary;
    //! 3. picks each separator's whitespace run as either a single space or a
    //!    `\n`-containing run, so both the break and space classification regimes
    //!    are exercised.
    //!
    //! Because the attribute `raw` slices stay ASCII and whitespace-free, they
    //! resolve past the interleaved multibyte chars, so the boundary arithmetic
    //! over multibyte input is genuinely driven rather than skipped.

    use super::*;
    use crate::config::{Configuration, SchemeId};
    use crate::model::{display_width, Attribute, AttributeKind, ScannedTag};
    use proptest::prelude::*;

    /// Indentation unit used throughout; the boundary behavior is independent of
    /// the indent model, so pinning it keeps the generators simple.
    const INDENT_WIDTH: usize = 2;
    /// The same indentation unit as [`INDENT_WIDTH`], typed for the `u8` config
    /// field so no lossy cast is needed.
    const INDENT_WIDTH_U8: u8 = 2;

    /// A generated separator between the previous token and the next attribute:
    /// an optional multibyte "marker" char sitting immediately before the
    /// whitespace run, plus the whitespace run itself (with or without a `\n`).
    /// The marker is the last non-whitespace char before the run, so it is
    /// exactly the char whose UTF-8 width the fix measures.
    #[derive(Clone, Debug)]
    struct GenSep {
        /// Optional multibyte non-whitespace marker placed before the whitespace.
        marker: Option<char>,
        /// The whitespace run; always non-empty so the attribute is a distinct
        /// token, and may contain a `\n` to drive the break regime.
        ws: String,
    }

    /// A small vocabulary of multibyte (>1 byte) non-whitespace chars spanning
    /// 2-, 3-, and 4-byte UTF-8 encodings, so `last_nonspace = i + c.len_utf8()`
    /// is exercised across every multibyte width.
    fn multibyte_marker() -> impl Strategy<Value = char> {
        prop::sample::select(vec![
            '\u{a1}',   // ¡  2 bytes
            '\u{e9}',   // é  2 bytes
            '\u{f1}',   // ñ  2 bytes
            '\u{20ac}', // €  3 bytes
            '\u{4e2d}', // 中 3 bytes (also wide)
            '\u{1f600}', // 😀 4 bytes
        ])
    }

    /// Strategy for one separator: optionally a multibyte marker, then a
    /// whitespace run that is either a single space or a `\n`-containing run.
    fn sep_strategy() -> impl Strategy<Value = GenSep> {
        (
            prop::option::of(multibyte_marker()),
            prop::sample::select(vec![" ", "  ", "\n", " \n  ", "\n\t", "\r\n  "]),
        )
            .prop_map(|(marker, ws)| GenSep {
                marker,
                ws: ws.to_string(),
            })
    }

    /// Strategy for a tag: a (possibly multibyte) element name, a set of
    /// distinct ASCII whitespace-free attribute raws, one separator per
    /// attribute, a self-closing flag, and a small line width.
    ///
    /// Attribute raws are made distinct by an index prefix so each resolves to
    /// its own occurrence in `src_tag` (no aliasing), and are ASCII +
    /// whitespace-free so they resolve past any interleaved multibyte marker.
    #[allow(clippy::type_complexity)]
    fn case_strategy() -> impl Strategy<Value = (String, Vec<String>, Vec<GenSep>, bool, u32)> {
        // Element name: ASCII base optionally followed by a multibyte char, so
        // multibyte names are covered too.
        let name = ("[a-z][a-z0-9]{0,5}", prop::option::of(multibyte_marker()))
            .prop_map(|(base, mb)| match mb {
                Some(c) => format!("{base}{c}"),
                None => base,
            });

        name.prop_flat_map(|name| {
            // 1..=6 attributes; raws are prefixed with their index for distinctness.
            prop::collection::vec("[a-z][a-z0-9]{0,6}(=\"[a-z0-9]{0,5}\")?", 1..=6).prop_flat_map(
                move |bodies| {
                    let raws: Vec<String> = bodies
                        .iter()
                        .enumerate()
                        .map(|(i, b)| format!("a{i}{b}"))
                        .collect();
                    let n = raws.len();
                    let name = name.clone();
                    (
                        Just(name),
                        Just(raws),
                        prop::collection::vec(sep_strategy(), n),
                        any::<bool>(),
                        1u32..40,
                    )
                },
            )
        })
    }

    /// Build the borrowed `Attribute` list from owned raws, classifying each as
    /// `NameValue` when it contains `=` and `Plain` otherwise (kind does not
    /// affect `leading_breaks_by_attribute`, which keys purely off `raw`).
    fn attributes_from(raws: &[String]) -> Vec<Attribute<'_>> {
        raws.iter()
            .map(|raw| {
                let kind = if raw.contains('=') {
                    AttributeKind::NameValue
                } else {
                    AttributeKind::Plain
                };
                Attribute {
                    kind,
                    name_key: None,
                    raw: raw.as_str(),
                    width: display_width(raw, INDENT_WIDTH),
                }
            })
            .collect()
    }

    /// Build a `ScannedTag` on the sorting-on / wrapping-off path.
    fn tag<'a>(
        name: &'a str,
        self_closing: bool,
        attributes: Vec<Attribute<'a>>,
    ) -> ScannedTag<'a> {
        ScannedTag {
            span: 0..1,
            name,
            self_closing,
            attributes,
            confidently_rewritable: true,
            open_col: 0,
            base_indent_col: 0,
            children: Vec::new(),
        }
    }

    /// The config that drives `leading_breaks_by_attribute`: sorting ENABLED,
    /// wrapping DISABLED (stated explicitly, per the task), with a small width.
    fn config(line_width: u32) -> Configuration {
        Configuration {
            line_width,
            // Explicitly: this is the ONLY path that reaches
            // `leading_breaks_by_attribute` (sorting on, wrapping off).
            fluid_wrapping: false,
            attribute_sorting: true,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: INDENT_WIDTH_U8,
            use_tabs: false,
            vue: false,
            astro: false,
            svelte: false,
        }
    }

    /// Assemble a `src_tag` from the name, attribute raws, and per-attribute
    /// separators: `<name` then, for each attribute, its separator (optional
    /// multibyte marker + whitespace run) followed by the attribute `raw`, then
    /// the terminator. Each attribute's `raw` is thus preceded by a whitespace
    /// run whose last preceding non-whitespace char may be multibyte — exactly
    /// the shape `leading_breaks_by_attribute` must slice across safely.
    fn build_src_tag(name: &str, raws: &[String], seps: &[GenSep], self_closing: bool) -> String {
        let mut s = String::new();
        s.push('<');
        s.push_str(name);
        for (raw, sep) in raws.iter().zip(seps) {
            if let Some(m) = sep.marker {
                s.push(m);
            }
            s.push_str(&sep.ws);
            s.push_str(raw);
        }
        s.push_str(if self_closing { " />" } else { ">" });
        s
    }

    proptest! {
        // Property (a): Never-panic on the sorting-on / wrapping-off path for
        // arbitrary valid UTF-8 (Validates: Requirements 7; Bug 2.1, 2.2, 2.3).

        /// For arbitrary valid UTF-8 `Start_Tags` whose `src_tag` interleaves
        /// multibyte non-whitespace chars immediately before attribute
        /// occurrences, `render_start_tag` with `attribute_sorting = true` and
        /// `fluid_wrapping = false` SHALL complete without panic. Reaching this
        /// test body at all means the call returned normally — a boundary panic
        /// inside `leading_breaks_by_attribute` would unwind and `proptest` would
        /// report the minimized counterexample instead.
        #[test]
        fn sorting_on_wrapping_off_never_panics_on_utf8(
            (name, raws, seps, self_closing, line_width) in case_strategy(),
        ) {
            let attrs = attributes_from(&raws);
            let t = tag(&name, self_closing, attrs);
            let src_tag = build_src_tag(&name, &raws, &seps, self_closing);

            // The call itself is the property: completing without unwinding
            // proves no non-char-boundary slice fired (Bug 2.1, 2.2, 2.3).
            let out = render_start_tag(&t, &config(line_width), Newline::Lf, &src_tag);

            // A well-formed, non-empty result is produced (Bug 2.2): it begins
            // with `<name` and carries the canonical terminator.
            let prefix = format!("<{name}");
            prop_assert!(
                out.starts_with(&prefix),
                "output must start with `<name`: out={out:?}"
            );
            if self_closing {
                prop_assert!(
                    out.ends_with(" />"),
                    "self-closing output must end with the canonical terminator: out={out:?}"
                );
            } else {
                prop_assert!(
                    out.ends_with('>'),
                    "non-self-closing output must end with `>`: out={out:?}"
                );
            }
        }

        // Property (b): Tag-level idempotence on the same path for both
        // multibyte and ASCII inputs (Validates: Requirements 8; Bug 3.2, 3.3).

        /// `render_start_tag(render_start_tag(x)) == render_start_tag(x)` on the
        /// sorting-on / wrapping-off path. The first render produces canonical
        /// text (attributes verbatim, separated by a single space or a lone
        /// `\n`); feeding that output back as `src_tag` with the SAME
        /// `ScannedTag` and config must reproduce it byte-for-byte. Because the
        /// attribute `raw` slices are re-emitted verbatim, they still resolve in
        /// the first render's output, so the second pass classifies the same
        /// attribute-bound breaks and reaches a fixed point — now for multibyte
        /// input as it already did for ASCII.
        #[test]
        fn sorting_on_wrapping_off_is_idempotent_on_utf8(
            (name, raws, seps, self_closing, line_width) in case_strategy(),
        ) {
            let attrs = attributes_from(&raws);
            let t = tag(&name, self_closing, attrs);
            let src_tag = build_src_tag(&name, &raws, &seps, self_closing);
            let cfg = config(line_width);

            let first = render_start_tag(&t, &cfg, Newline::Lf, &src_tag);
            let second = render_start_tag(&t, &cfg, Newline::Lf, &first);

            prop_assert_eq!(
                &second,
                &first,
                "sorting-on / wrapping-off render must be a fixed point: \
                 first={:?} second={:?} src={:?}",
                first,
                second,
                src_tag
            );
        }
    }
}
