//! Structure-equivalence verification: tag-local and whole-document fingerprints.
//!
//! This module implements the two-granularity structure-equivalence machinery
//! that makes the plugin's "adjust only whitespace, never change structure"
//! guarantee enforceable (Requirements 7.5, 7.6, 7.7). It is entirely pure: it
//! derives fingerprints from already-scanned tags (or by scanning a buffer) and
//! compares them, never mutating anything and never performing I/O.
//!
//! Two layers are provided, mirroring the design:
//!
//! - **Per-tag ([`tag_fingerprint`] / [`tag_fingerprints_equivalent`]).** The
//!   engine fingerprints each rewritten start tag individually, capturing its
//!   structural signature — element name, self-closing flag, and the ordered
//!   list of attribute `name → raw-value` pairs — before and after its own
//!   rewrite. On per-tag divergence the engine reverts ONLY that tag's bytes and
//!   keeps every other tag's rewrite (Requirement 7.6). There is deliberately NO
//!   file-wide confidence classifier.
//! - **Document-level ([`structural_fingerprint`] / [`fingerprints_equivalent`]).**
//!   The whole input and the whole output are each scanned to a [`Fingerprint`]
//!   — the ordered sequence of tag signatures interleaved with the verbatim text
//!   between tags — and compared as a final backstop (Requirement 7.7).
//!
//! # The Requirement 7.5 equivalence relation
//!
//! Two fingerprints are equivalent when their markup structure matches:
//! identical element sequence/nesting and identical attribute name→value pairs,
//! disregarding only the whitespace the formatter adds or removes *between*
//! attributes and at the start of continuation lines. Because every attribute's
//! [`raw`](crate::model::Attribute::raw) slice is captured verbatim and never
//! reconstructed, the inter-attribute whitespace the formatter touches is simply
//! not part of a fingerprint's `name → raw-value` pairs — so comparing raw
//! values for exact equality already honors "disregard only formatter
//! whitespace".
//!
//! When sorting is enabled, a tag's attributes are compared as a **multiset**
//! (order-insensitive), with one exception: the mutually behavior-affecting
//! attributes — the members of any duplicate-name set and every `DynamicSpread`
//! attribute — are locked by `sort_attributes`, so their *relative* order must
//! still match (Requirement 5.5). When sorting is disabled the attribute lists
//! are compared as an ordered sequence.

use std::collections::BTreeMap;

use crate::dialect::ScannerProfile;
use crate::model::{Fingerprint, FingerprintItem, ScannedTag, TagFingerprint};
use crate::scanner::{scan_start_tags, ParseFailure};

/// Produce the tag-local [`TagFingerprint`] of a scanned start tag.
///
/// The signature is `(name, self_closing, [(name_key, raw-value)…])`, where each
/// attribute contributes its lowercased comparison name (`None` for a pure
/// spread) paired with its exact raw source slice. This captures element and
/// attribute *identity* while ignoring the inter-attribute whitespace the
/// formatter is allowed to change (that whitespace is never part of a `raw`
/// slice). Used to compare a tag before and after its own rewrite
/// (Requirement 7.6).
#[must_use]
pub fn tag_fingerprint(tag: &ScannedTag<'_>) -> TagFingerprint {
    TagFingerprint {
        name: tag.name.to_string(),
        self_closing: tag.self_closing,
        attributes: tag
            .attributes
            .iter()
            .enumerate()
            .map(|(idx, attr)| (attr.name_key.clone(), masked_attr_value(tag, idx, attr.raw)))
            .collect(),
        // Recurse into nested attribute-value elements so the structure check
        // sees the full two-level (and arbitrarily deep) markup (Finding 5,
        // Requirements 7.5, 7.6). Children are kept in source order; the plugin
        // never reorders them.
        children: tag
            .children
            .iter()
            .map(|child| tag_fingerprint(&child.tag))
            .collect(),
    }
}

/// Placeholder that replaces each nested-element byte region inside an owning
/// attribute's raw value when building its fingerprint. Using a single NUL char
/// (which never appears in valid source markup) per child collapses the child's
/// bytes to a stable token so the attribute-value comparison ignores the
/// whitespace a two-level child rewrite introduced, while the child's own
/// structure is compared separately through [`TagFingerprint::children`].
const CHILD_PLACEHOLDER: &str = "\u{0}";

/// Build the fingerprint value for attribute `attr_index` of `tag`, with every
/// nested child's byte region inside that attribute's raw value replaced by
/// [`CHILD_PLACEHOLDER`].
///
/// An attribute whose value contains a genuine nested element (`primary={ <a …>
/// … </a> }`) has that element rewritten independently by the engine, so the
/// attribute's raw bytes change (new continuation whitespace inside the child).
/// Those changes are NOT a structural divergence — the child's structure is
/// captured recursively in [`TagFingerprint::children`] — so the attribute-value
/// comparison must disregard them. Masking each child region to a single stable
/// token leaves exactly the NON-child skeleton of the value (which the formatter
/// must preserve byte-for-byte) available for comparison, honoring the
/// Requirement 7.5 "disregard only formatter whitespace" relation at both levels.
///
/// Attributes with no children (the common case) return their raw verbatim, so
/// every existing fingerprint is unchanged.
///
/// Char-boundary note: `offset_in_attr_raw` and each child span length are
/// scanner-produced char-boundary offsets within the attribute's raw slice, so
/// every slice taken here is boundary-safe; out-of-range or non-boundary offsets
/// are skipped defensively, never panicking (Requirement 7.9).
fn masked_attr_value(tag: &ScannedTag<'_>, attr_index: usize, raw: &str) -> String {
    // Collect the (start, end) child regions owned by this attribute, in raw
    // coordinates, sorted by start.
    let mut regions: Vec<(usize, usize)> = tag
        .children
        .iter()
        .filter(|c| c.owner_attr_index == attr_index)
        .map(|c| {
            let start = c.offset_in_attr_raw;
            let end = start + (c.tag.span.end - c.tag.span.start);
            (start, end)
        })
        .collect();
    if regions.is_empty() {
        return raw.to_string();
    }
    regions.sort_by_key(|(s, _)| *s);

    let mut out = String::with_capacity(raw.len());
    let mut cursor = 0usize;
    for (start, end) in regions {
        // Defensive bounds / ordering / boundary checks (never panic).
        if start < cursor
            || end > raw.len()
            || start > end
            || !raw.is_char_boundary(start)
            || !raw.is_char_boundary(end)
        {
            continue;
        }
        out.push_str(&raw[cursor..start]);
        out.push_str(CHILD_PLACEHOLDER);
        cursor = end;
    }
    out.push_str(&raw[cursor..]);
    out
}

/// Whether a tag's `before` and `after` fingerprints are equivalent under the
/// Requirement 7.5 relation.
///
/// The element name and self-closing flag must match exactly. Attribute
/// equivalence depends on `sorting`:
///
/// - `sorting == false`: the attribute `(name_key, raw)` pairs must match as an
///   **ordered sequence** — sorting is off, so the formatter must not have
///   reordered anything.
/// - `sorting == true`: the pairs must match as a **multiset** (order-insensitive),
///   except that the locked behavior-affecting attributes — every member of a
///   duplicate-name set and every pure-spread (`DynamicSpread`) attribute — must
///   keep their *relative* order between `before` and `after` (Requirement 5.5).
///
/// Pure. Never panics.
#[must_use]
pub fn tag_fingerprints_equivalent(
    before: &TagFingerprint,
    after: &TagFingerprint,
    sorting: bool,
) -> bool {
    if before.name != after.name || before.self_closing != after.self_closing {
        return false;
    }
    if !attributes_equivalent(&before.attributes, &after.attributes, sorting) {
        return false;
    }
    // Nested attribute-value elements must match one-for-one in source order,
    // each recursively equivalent under the same relation (Finding 5). A
    // dropped, added, or structurally changed child is a divergence just like a
    // top-level tag. Children are never reordered by the plugin, so they are
    // compared as an ordered sequence regardless of `sorting`.
    if before.children.len() != after.children.len() {
        return false;
    }
    before
        .children
        .iter()
        .zip(after.children.iter())
        .all(|(b, a)| tag_fingerprints_equivalent(b, a, sorting))
}

/// Compare two attribute `(name_key, raw)` lists under the Requirement 7.5
/// relation, parameterized by whether sorting was enabled.
fn attributes_equivalent(
    before: &[(Option<String>, String)],
    after: &[(Option<String>, String)],
    sorting: bool,
) -> bool {
    // A different count can never be equivalent: wrapping/sorting is a
    // permutation that preserves the attribute set (Requirement 4.6).
    if before.len() != after.len() {
        return false;
    }

    if !sorting {
        // Sorting off: order must be preserved exactly.
        return before == after;
    }

    // Sorting on: the full set must match as a multiset...
    if !multisets_equal(before, after) {
        return false;
    }

    // ...and the locked, behavior-affecting attributes must keep their relative
    // order. These are the pure spreads (`name_key == None`) and every member of
    // a duplicate-name set. Projecting both lists down to just those locked
    // pairs must yield the same ordered subsequence.
    locked_subsequence(before) == locked_subsequence(after)
}

/// Whether two attribute-pair slices contain the same elements with the same
/// multiplicities (order-insensitive equality).
fn multisets_equal(a: &[(Option<String>, String)], b: &[(Option<String>, String)]) -> bool {
    counts(a) == counts(b)
}

/// Count the multiplicity of each `(name_key, raw)` pair. A `BTreeMap` gives a
/// deterministic, order-independent representation suitable for equality.
fn counts(pairs: &[(Option<String>, String)]) -> BTreeMap<(Option<String>, String), usize> {
    let mut map: BTreeMap<(Option<String>, String), usize> = BTreeMap::new();
    for pair in pairs {
        *map.entry(pair.clone()).or_insert(0) += 1;
    }
    map
}

/// Project an attribute list down to just its locked, behavior-affecting pairs,
/// preserving their relative order.
///
/// Locked pairs are exactly those `sort_attributes` is forbidden to reorder
/// relative to one another (Requirement 5.5):
///
/// - every pure-spread attribute (`name_key == None`), and
/// - every attribute whose `name_key` appears more than once in the list (a
///   duplicate-name set — reordering duplicates can change behavior).
///
/// The resulting ordered subsequence is what must match between a tag's before
/// and after fingerprints even when the rest of the attributes are compared as a
/// multiset.
fn locked_subsequence(pairs: &[(Option<String>, String)]) -> Vec<(Option<String>, String)> {
    // Names that occur more than once are duplicate-name sets and are locked.
    let mut name_counts: BTreeMap<&Option<String>, usize> = BTreeMap::new();
    for (name, _) in pairs {
        *name_counts.entry(name).or_insert(0) += 1;
    }

    pairs
        .iter()
        .filter(|(name, _)| {
            // Pure spreads are always locked; a named attribute is locked when
            // its name is duplicated. (A `None` key is also caught by the count
            // test, but spelling the spread case out keeps the intent explicit.)
            name.is_none() || name_counts.get(name).copied().unwrap_or(0) > 1
        })
        .cloned()
        .collect()
}

/// Scan `src` to a whole-document [`Fingerprint`]: the ordered sequence of
/// start-tag signatures interleaved with the verbatim text lying between them.
///
/// The buffer is scanned with [`scan_start_tags`]; every byte that is not part
/// of a located start tag (ordinary text, end tags, comments, opaque regions,
/// and so on) is captured verbatim as a [`FingerprintItem::Text`] run between
/// the surrounding tag items. The result is the document-level structure used by
/// [`fingerprints_equivalent`] as the backstop comparison (Requirement 7.7).
///
/// Returns the same [`ParseFailure`] the scanner would, so a truncated input is
/// reported identically whether it is being formatted or fingerprinted.
pub fn structural_fingerprint(
    src: &str,
    profile: &ScannerProfile,
    indent_width: usize,
) -> Result<Fingerprint, ParseFailure> {
    let tags = scan_start_tags(src, profile, indent_width)?;

    let mut items = Vec::with_capacity(tags.len() * 2 + 1);
    let mut cursor = 0usize;
    for tag in &tags {
        // Verbatim text between the previous tag's end and this tag's start.
        if tag.span.start > cursor {
            items.push(FingerprintItem::Text(src[cursor..tag.span.start].to_string()));
        }
        items.push(FingerprintItem::Tag(tag_fingerprint(tag)));
        cursor = tag.span.end;
    }
    // Trailing text after the last tag (or the whole buffer when there are no
    // tags at all).
    if cursor < src.len() {
        items.push(FingerprintItem::Text(src[cursor..].to_string()));
    }

    Ok(Fingerprint { items })
}

/// Whether the `input` and `output` document fingerprints are equivalent under
/// the Requirement 7.5 relation.
///
/// The two item sequences must have the same length and each positional pair
/// must match in kind:
///
/// - [`FingerprintItem::Text`] runs must be **byte-identical** — the formatter
///   never changes anything between start tags (Requirement 7.4), so any
///   difference here is a real structural divergence.
/// - [`FingerprintItem::Tag`] items are compared with
///   [`tag_fingerprints_equivalent`] under the same `sorting` flag, so the
///   multiset/locked-order semantics apply per tag at the document level too.
///
/// Pure. Never panics.
#[must_use]
pub fn fingerprints_equivalent(input: &Fingerprint, output: &Fingerprint, sorting: bool) -> bool {
    if input.items.len() != output.items.len() {
        return false;
    }
    input
        .items
        .iter()
        .zip(output.items.iter())
        .all(|(a, b)| match (a, b) {
            (FingerprintItem::Text(ta), FingerprintItem::Text(tb)) => ta == tb,
            (FingerprintItem::Tag(fa), FingerprintItem::Tag(fb)) => {
                tag_fingerprints_equivalent(fa, fb, sorting)
            }
            // A tag where the other side has text (or vice versa) is a structural
            // divergence — the element sequence itself changed.
            _ => false,
        })
}

/// Whether the total count of `<` and the total count of `>` over the whole
/// buffer are preserved between `input` and `output`.
///
/// This is a dialect-agnostic backstop invariant for the structure-equivalence
/// self-check (Requirements 2.7, 7.5). The plugin's only transform is adjusting
/// whitespace and attribute *order* inside genuine start tags; it never adds or
/// removes a `<` or `>` anywhere in the buffer. So the per-buffer counts of both
/// delimiters are an invariant of every legitimate wrap/sort transform, and it
/// is trivially insensitive to whitespace and reordering (neither changes a
/// `<`/`>` count).
///
/// Its purpose is to catch a corrupting rewrite that drops (or adds) a `<`/`>` —
/// for example a TypeScript generic misread as a JSX start tag whose angle
/// brackets are dropped — even when the same misread occurs symmetrically on
/// both the input and output scan and therefore slips past the fingerprint
/// comparison. Because the counts are taken over the raw bytes of the whole
/// buffer rather than over any tag-excluded residue, a dropped delimiter is
/// always observed here.
///
/// Returns `true` iff `count('<', input) == count('<', output)` and
/// `count('>', input) == count('>', output)`.
///
/// A single `O(n)` pass over each buffer, no allocation. Pure; never panics.
#[must_use]
pub fn angle_brackets_conserved(input: &str, output: &str) -> bool {
    count_angle_brackets(input) == count_angle_brackets(output)
}

/// Count the `<` and `>` bytes in `src`, returning `(count('<'), count('>'))`.
///
/// Counting over bytes is correct because both `<` (`0x3C`) and `>` (`0x3E`) are
/// ASCII and can never appear as a continuation byte of a multi-byte UTF-8
/// sequence, so a byte scan cannot miscount them. A single `O(n)` pass, no
/// allocation.
fn count_angle_brackets(src: &str) -> (usize, usize) {
    let mut lt = 0usize;
    let mut gt = 0usize;
    for &byte in src.as_bytes() {
        match byte {
            b'<' => lt += 1,
            b'>' => gt += 1,
            _ => {}
        }
    }
    (lt, gt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Dialect;
    use crate::model::ScannedTag;

    fn html() -> ScannerProfile {
        Dialect::Html.scanner_profile()
    }

    fn jsx() -> ScannerProfile {
        Dialect::Jsx.scanner_profile()
    }

    /// Scan exactly one tag and return it, panicking on anything else.
    fn one_tag<'a>(src: &'a str, profile: &ScannerProfile) -> ScannedTag<'a> {
        let tags = scan_start_tags(src, profile, 2).expect("scan should succeed");
        assert_eq!(tags.len(), 1, "expected one tag in {src:?}");
        tags.into_iter().next().unwrap()
    }

    /// Build a tag fingerprint directly from `(name_key, raw)` pairs.
    fn fp(name: &str, self_closing: bool, attrs: &[(Option<&str>, &str)]) -> TagFingerprint {
        TagFingerprint {
            name: name.to_string(),
            self_closing,
            attributes: attrs
                .iter()
                .map(|(n, r)| (n.map(str::to_string), (*r).to_string()))
                .collect(),
            children: Vec::new(),
        }
    }

    // ----- tag_fingerprint -------------------------------------------------

    #[test]
    fn tag_fingerprint_captures_name_flag_and_attribute_pairs() {
        let tag = one_tag("<div class=\"a\" id='b' disabled>", &html());
        let f = tag_fingerprint(&tag);
        assert_eq!(f.name, "div");
        assert!(!f.self_closing);
        assert_eq!(
            f.attributes,
            vec![
                (Some("class".to_string()), "class=\"a\"".to_string()),
                (Some("id".to_string()), "id='b'".to_string()),
                (Some("disabled".to_string()), "disabled".to_string()),
            ]
        );
    }

    #[test]
    fn tag_fingerprint_records_self_closing_and_spread() {
        let tag = one_tag("<Comp {...props} a={x} />", &jsx());
        let f = tag_fingerprint(&tag);
        assert_eq!(f.name, "Comp");
        assert!(f.self_closing);
        assert_eq!(f.attributes[0], (None, "{...props}".to_string()));
        assert_eq!(f.attributes[1], (Some("a".to_string()), "a={x}".to_string()));
    }

    // ----- tag_fingerprints_equivalent: whitespace-only differences --------

    #[test]
    fn equivalent_when_only_inter_attribute_whitespace_differs() {
        // The two forms differ only in the whitespace between attributes, which
        // is never part of a raw slice, so the fingerprints are identical and
        // thus equivalent whether or not sorting is enabled.
        let compact = tag_fingerprint(&one_tag("<div a=\"1\" b=\"2\">", &html()));
        let spaced = tag_fingerprint(&one_tag("<div   a=\"1\"\n  b=\"2\">", &html()));
        assert_eq!(compact, spaced);
        assert!(tag_fingerprints_equivalent(&compact, &spaced, false));
        assert!(tag_fingerprints_equivalent(&compact, &spaced, true));
    }

    #[test]
    fn whitespace_inside_a_value_is_preserved_and_compared() {
        // Whitespace *inside* a quoted value is part of the raw slice and must be
        // preserved; changing it is a real divergence.
        let a = fp("div", false, &[(Some("title"), "title=\"a b\"")]);
        let b = fp("div", false, &[(Some("title"), "title=\"a  b\"")]);
        assert!(!tag_fingerprints_equivalent(&a, &b, false));
        assert!(!tag_fingerprints_equivalent(&a, &b, true));
    }

    // ----- name / self-closing mismatches ----------------------------------

    #[test]
    fn different_name_or_self_closing_is_not_equivalent() {
        let base = fp("div", false, &[(Some("a"), "a")]);
        let renamed = fp("span", false, &[(Some("a"), "a")]);
        let closed = fp("div", true, &[(Some("a"), "a")]);
        assert!(!tag_fingerprints_equivalent(&base, &renamed, false));
        assert!(!tag_fingerprints_equivalent(&base, &closed, false));
    }

    // ----- ordered vs multiset semantics -----------------------------------

    #[test]
    fn reorder_is_divergent_when_sorting_disabled_but_equivalent_when_enabled() {
        let original = fp(
            "div",
            false,
            &[(Some("b"), "b=\"2\""), (Some("a"), "a=\"1\"")],
        );
        let reordered = fp(
            "div",
            false,
            &[(Some("a"), "a=\"1\""), (Some("b"), "b=\"2\"")],
        );
        // Sorting off: reordering is a structural divergence.
        assert!(!tag_fingerprints_equivalent(&original, &reordered, false));
        // Sorting on: order-insensitive multiset, so this is equivalent.
        assert!(tag_fingerprints_equivalent(&original, &reordered, true));
    }

    #[test]
    fn dropped_or_added_attribute_is_never_equivalent() {
        let two = fp("div", false, &[(Some("a"), "a"), (Some("b"), "b")]);
        let one = fp("div", false, &[(Some("a"), "a")]);
        assert!(!tag_fingerprints_equivalent(&two, &one, false));
        assert!(!tag_fingerprints_equivalent(&two, &one, true));
    }

    #[test]
    fn changed_value_breaks_multiset_equivalence() {
        let a = fp("div", false, &[(Some("a"), "a=\"1\""), (Some("b"), "b=\"2\"")]);
        let b = fp("div", false, &[(Some("b"), "b=\"2\""), (Some("a"), "a=\"X\"")]);
        // Even under sorting, the value changed, so the multiset differs.
        assert!(!tag_fingerprints_equivalent(&a, &b, true));
    }

    // ----- locked behavior-affecting attributes ----------------------------

    #[test]
    fn spread_relative_order_must_be_preserved_even_when_sorting() {
        // Two spreads plus a named attribute. Reordering the two spreads relative
        // to each other changes behavior and must NOT be treated as equivalent,
        // even though the overall multiset is unchanged.
        let before = fp(
            "Comp",
            false,
            &[(None, "{...a}"), (None, "{...b}"), (Some("x"), "x={1}")],
        );
        // Named attribute moved ahead of the spreads: spreads keep their relative
        // order (a before b), so this is still equivalent under sorting.
        let x_moved = fp(
            "Comp",
            false,
            &[(Some("x"), "x={1}"), (None, "{...a}"), (None, "{...b}")],
        );
        assert!(tag_fingerprints_equivalent(&before, &x_moved, true));

        // The two spreads swapped relative order: NOT equivalent, locked order
        // violated (Requirement 5.5).
        let spreads_swapped = fp(
            "Comp",
            false,
            &[(None, "{...b}"), (None, "{...a}"), (Some("x"), "x={1}")],
        );
        assert!(!tag_fingerprints_equivalent(&before, &spreads_swapped, true));
    }

    #[test]
    fn duplicate_name_relative_order_must_be_preserved_even_when_sorting() {
        // Two attributes sharing a name form a duplicate-name set; their relative
        // order is behavior-affecting and locked.
        let before = fp(
            "input",
            false,
            &[
                (Some("class"), "class=\"a\""),
                (Some("class"), "class=\"b\""),
                (Some("id"), "id=\"x\""),
            ],
        );
        // id moved before the class pair; the two class attributes keep their
        // relative order (a before b) -> equivalent under sorting.
        let id_moved = fp(
            "input",
            false,
            &[
                (Some("id"), "id=\"x\""),
                (Some("class"), "class=\"a\""),
                (Some("class"), "class=\"b\""),
            ],
        );
        assert!(tag_fingerprints_equivalent(&before, &id_moved, true));

        // The two class attributes swapped relative order -> NOT equivalent.
        let class_swapped = fp(
            "input",
            false,
            &[
                (Some("class"), "class=\"b\""),
                (Some("class"), "class=\"a\""),
                (Some("id"), "id=\"x\""),
            ],
        );
        assert!(!tag_fingerprints_equivalent(&before, &class_swapped, true));
    }

    // ----- structural_fingerprint ------------------------------------------

    #[test]
    fn structural_fingerprint_interleaves_tags_and_text() {
        let src = "before <a href=\"x\">mid</a> after <b />end";
        let doc = structural_fingerprint(src, &html(), 2).expect("scan succeeds");
        // Items: Text("before "), Tag(a), Text("mid</a> after "), Tag(b), Text("end").
        assert_eq!(doc.items.len(), 5);
        match &doc.items[0] {
            FingerprintItem::Text(t) => assert_eq!(t, "before "),
            other @ FingerprintItem::Tag(_) => panic!("expected leading text, got {other:?}"),
        }
        match &doc.items[1] {
            FingerprintItem::Tag(f) => assert_eq!(f.name, "a"),
            other @ FingerprintItem::Text(_) => panic!("expected tag a, got {other:?}"),
        }
        match &doc.items[2] {
            FingerprintItem::Text(t) => assert_eq!(t, "mid</a> after "),
            other @ FingerprintItem::Tag(_) => panic!("expected mid text, got {other:?}"),
        }
        match &doc.items[3] {
            FingerprintItem::Tag(f) => {
                assert_eq!(f.name, "b");
                assert!(f.self_closing);
            }
            other @ FingerprintItem::Text(_) => panic!("expected tag b, got {other:?}"),
        }
        match &doc.items[4] {
            FingerprintItem::Text(t) => assert_eq!(t, "end"),
            other @ FingerprintItem::Tag(_) => panic!("expected trailing text, got {other:?}"),
        }
    }

    #[test]
    fn structural_fingerprint_of_text_only_input_is_one_text_item() {
        let doc = structural_fingerprint("no tags here", &html(), 2).unwrap();
        assert_eq!(doc.items.len(), 1);
        assert!(matches!(&doc.items[0], FingerprintItem::Text(t) if t == "no tags here"));

        // Empty input yields no items at all.
        let empty = structural_fingerprint("", &html(), 2).unwrap();
        assert_eq!(empty.items.len(), 0);
    }

    #[test]
    fn structural_fingerprint_propagates_parse_failures() {
        let err = structural_fingerprint("<div class=\"x", &html(), 2).unwrap_err();
        assert!(err.message.contains("unterminated"));
        assert_eq!(err.line, 1);
    }

    // ----- fingerprints_equivalent: document level -------------------------

    #[test]
    fn document_equivalent_under_inter_attribute_whitespace_change() {
        // Only the whitespace inside the start tag changed; the text between tags
        // and the attribute pairs are identical, so the documents are equivalent.
        let input = structural_fingerprint("x <a b=\"1\" c=\"2\">y</a>z", &html(), 2).unwrap();
        let output =
            structural_fingerprint("x <a\n  b=\"1\"\n  c=\"2\"\n>y</a>z", &html(), 2).unwrap();
        assert!(fingerprints_equivalent(&input, &output, false));
        assert!(fingerprints_equivalent(&input, &output, true));
    }

    #[test]
    fn document_divergent_when_inter_tag_text_changes() {
        let input = structural_fingerprint("<a>hello</a>", &html(), 2).unwrap();
        // The text between tags changed from "hello" to "world" — a real break.
        let output = structural_fingerprint("<a>world</a>", &html(), 2).unwrap();
        assert!(!fingerprints_equivalent(&input, &output, false));
        assert!(!fingerprints_equivalent(&input, &output, true));
    }

    #[test]
    fn document_divergent_when_attribute_pair_changes() {
        let input = structural_fingerprint("<a href=\"old\">", &html(), 2).unwrap();
        let output = structural_fingerprint("<a href=\"new\">", &html(), 2).unwrap();
        assert!(!fingerprints_equivalent(&input, &output, false));
        assert!(!fingerprints_equivalent(&input, &output, true));
    }

    #[test]
    fn document_divergent_when_tag_count_changes() {
        let input = structural_fingerprint("<a><b>", &html(), 2).unwrap();
        let output = structural_fingerprint("<a>", &html(), 2).unwrap();
        assert!(!fingerprints_equivalent(&input, &output, false));
    }

    #[test]
    fn document_reorder_follows_sorting_flag() {
        let input = structural_fingerprint("<a b=\"1\" c=\"2\">", &html(), 2).unwrap();
        let output = structural_fingerprint("<a c=\"2\" b=\"1\">", &html(), 2).unwrap();
        // Sorting off: reorder is a divergence.
        assert!(!fingerprints_equivalent(&input, &output, false));
        // Sorting on: order-insensitive, so equivalent.
        assert!(fingerprints_equivalent(&input, &output, true));
    }

    // ----- angle_brackets_conserved ----------------------------------------
    //
    // The whole-buffer `<`/`>` conservation invariant (Requirements 2.7, 7.5).
    // A legitimate wrap/sort transform only moves whitespace and permutes
    // attribute slices, so it never changes a `<`/`>` count; a corrupting
    // rewrite that drops a delimiter always does. These cases pin both sides.

    #[test]
    fn angle_brackets_conserved_true_for_whitespace_only_rewrite() {
        // Fluid-wrapping a start tag across lines only inserts whitespace; the
        // `<` and `>` counts are untouched, so the invariant holds.
        let input = "<div class=\"a\" id=\"b\" role=\"c\">text</div>";
        let output = "<div\n  class=\"a\"\n  id=\"b\"\n  role=\"c\"\n>text</div>";
        assert!(angle_brackets_conserved(input, output));
    }

    #[test]
    fn angle_brackets_conserved_true_for_reorder_only_rewrite() {
        // Attribute sorting permutes existing attribute slices; no `<`/`>` is
        // added or removed, so the invariant holds.
        let input = "<div class=\"a\" id=\"b\" role=\"c\">";
        let output = "<div role=\"c\" class=\"a\" id=\"b\">";
        assert!(angle_brackets_conserved(input, output));
    }

    #[test]
    fn angle_brackets_conserved_false_for_dropped_lt() {
        // The output is missing one `<` relative to the input.
        let input = "<div><span>";
        let output = "<div>span>";
        assert!(!angle_brackets_conserved(input, output));
    }

    #[test]
    fn angle_brackets_conserved_false_for_dropped_gt() {
        // The output is missing one `>` relative to the input.
        let input = "<div><span>";
        let output = "<div><span";
        assert!(!angle_brackets_conserved(input, output));
    }

    #[test]
    fn angle_brackets_conserved_false_for_symmetric_misread_generic() {
        // The motivating regression: a TypeScript generic misread as a JSX start
        // tag whose inner `<` is dropped. `useState<Array<Task>>` becomes
        // `useState<Array Task>>` — one fewer `<`. Even though the misread is
        // symmetric across both scans (and so slips past the fingerprint
        // comparison), the raw whole-buffer `<` count exposes it.
        let input = "const x = useState<Array<Task>>([]);";
        let output = "const x = useState<Array Task>>([]);";
        // Exactly one `<` was dropped; `>` count is unchanged.
        assert!(!angle_brackets_conserved(input, output));
    }
}

/// Scenario-level tests for the two-granularity equivalence machinery that
/// exercise the per-tag revert (Requirement 7.6) and the residual document-level
/// divergence backstop (Requirement 7.7) as whole-document narratives, rather
/// than the per-function unit checks in [`tests`].
///
/// The actual byte-reverting and error-raising live in the engine (task 11.1);
/// here we prove the fingerprint/equivalence *building blocks* support those
/// outcomes: a per-tag divergence can be localized to one tag, and splicing that
/// one tag's original bytes back restores document equivalence, while a break
/// that cannot be localized to any single tag stays divergent at the document
/// level (which the engine maps to a `Format_Error`).
#[cfg(test)]
mod equivalence_scenario_tests {
    use super::*;
    use crate::dialect::Dialect;

    fn html() -> ScannerProfile {
        Dialect::Html.scanner_profile()
    }

    /// Splice `replacement` over the byte range `span` of `src`, returning the
    /// edited buffer. This is how the engine rewrites (or reverts) a single tag:
    /// every byte outside `span` is kept verbatim.
    fn splice(src: &str, span: std::ops::Range<usize>, replacement: &str) -> String {
        let mut out = String::with_capacity(src.len());
        out.push_str(&src[..span.start]);
        out.push_str(replacement);
        out.push_str(&src[span.end..]);
        out
    }

    /// Byte span of the `nth` (0-based) scanned start tag in `src`.
    fn nth_tag_span(src: &str, n: usize) -> std::ops::Range<usize> {
        let tags = scan_start_tags(src, &html(), 2).expect("scan should succeed");
        tags[n].span.clone()
    }

    // ----- tag-local + document-level whitespace-only equivalence ----------
    //
    // The task calls for whitespace-only equivalence at BOTH granularities and a
    // deliberately structure-breaking transform reported as non-equivalent, as a
    // single coherent scenario. (The per-function unit module covers the pieces;
    // this re-asserts them together on one document for traceability.)

    #[test]
    fn whitespace_only_rewrite_is_equivalent_at_both_granularities() {
        // A compact document and the same document with the two start tags
        // re-wrapped across lines. Only inter-attribute whitespace changed.
        let input = "<section id=\"main\" class=\"wrap\">text<a href=\"x\" rel=\"y\">link</a></section>";
        let output = "<section\n  id=\"main\"\n  class=\"wrap\"\n>text<a\n  href=\"x\"\n  rel=\"y\"\n>link</a></section>";

        // Tag-local: each rewritten start tag is equivalent to its original.
        let in_tags = scan_start_tags(input, &html(), 2).unwrap();
        let out_tags = scan_start_tags(output, &html(), 2).unwrap();
        assert_eq!(in_tags.len(), out_tags.len());
        for (before, after) in in_tags.iter().zip(out_tags.iter()) {
            let fb = tag_fingerprint(before);
            let fa = tag_fingerprint(after);
            assert!(tag_fingerprints_equivalent(&fb, &fa, false));
            assert!(tag_fingerprints_equivalent(&fb, &fa, true));
        }

        // Document-level: the whole-document fingerprints are equivalent too.
        let doc_in = structural_fingerprint(input, &html(), 2).unwrap();
        let doc_out = structural_fingerprint(output, &html(), 2).unwrap();
        assert!(fingerprints_equivalent(&doc_in, &doc_out, false));
        assert!(fingerprints_equivalent(&doc_in, &doc_out, true));
    }

    #[test]
    fn structure_breaking_transform_is_reported_non_equivalent() {
        // A transform that drops an attribute from a tag is a real structural
        // break and must be reported as non-equivalent at both granularities.
        let input = "<img src=\"a.png\" alt=\"pic\">";
        let broken = "<img src=\"a.png\">"; // alt dropped

        let before = tag_fingerprint(&scan_start_tags(input, &html(), 2).unwrap()[0]);
        let after = tag_fingerprint(&scan_start_tags(broken, &html(), 2).unwrap()[0]);
        assert!(!tag_fingerprints_equivalent(&before, &after, false));
        assert!(!tag_fingerprints_equivalent(&before, &after, true));

        let doc_in = structural_fingerprint(input, &html(), 2).unwrap();
        let doc_out = structural_fingerprint(broken, &html(), 2).unwrap();
        assert!(!fingerprints_equivalent(&doc_in, &doc_out, false));
    }

    // ----- per-tag revert (Requirement 7.6) --------------------------------

    #[test]
    fn per_tag_revert_localizes_divergence_and_restores_equivalence() {
        // A document with THREE start tags. We simulate the engine having
        // rewritten all three: the first and third are valid whitespace-only
        // rewrites, but the MIDDLE tag's rewrite accidentally dropped an
        // attribute (a structural divergence localized to that one tag).
        let input = "<a href=\"1\" rel=\"r\">one<b class=\"c\" id=\"i\">two<c data=\"d\" x=\"y\">three";

        // The "after all rewrites" buffer: tag 0 and tag 2 are re-wrapped
        // (whitespace only), tag 1 is broken (lost its `id` attribute).
        let after_rewrites =
            "<a\n  href=\"1\"\n  rel=\"r\"\n>one<b\n  class=\"c\"\n>two<c\n  data=\"d\"\n  x=\"y\"\n>three";

        let in_tags = scan_start_tags(input, &html(), 2).unwrap();
        let out_tags = scan_start_tags(after_rewrites, &html(), 2).unwrap();
        assert_eq!(in_tags.len(), 3);
        assert_eq!(out_tags.len(), 3);

        // The per-tag self-check: compare each rewritten tag's before/after
        // fingerprint. Exactly tag index 1 diverges; 0 and 2 are equivalent.
        let diverged: Vec<usize> = in_tags
            .iter()
            .zip(out_tags.iter())
            .enumerate()
            .filter(|(_, (b, a))| {
                !tag_fingerprints_equivalent(&tag_fingerprint(b), &tag_fingerprint(a), false)
            })
            .map(|(i, _)| i)
            .collect();
        assert_eq!(diverged, vec![1], "only the middle tag should diverge");

        // Revert ONLY that tag: splice the middle tag's ORIGINAL bytes back into
        // the rewritten buffer, keeping the other two tags' valid rewrites. We
        // locate the middle tag's span in the rewritten buffer and overwrite it
        // with the original middle-tag source slice.
        let original_mid = &input[nth_tag_span(input, 1)];
        let reverted = splice(after_rewrites, out_tags[1].span.clone(), original_mid);

        // After the targeted revert the document is structurally equivalent to
        // the input again: the middle tag is a safe no-op, the other two keep
        // their whitespace-only rewrites.
        let doc_in = structural_fingerprint(input, &html(), 2).unwrap();
        let doc_reverted = structural_fingerprint(&reverted, &html(), 2).unwrap();
        assert!(
            fingerprints_equivalent(&doc_in, &doc_reverted, false),
            "reverting only the diverging tag must restore document equivalence"
        );

        // And the reverted middle tag now matches its original byte-for-byte:
        // the rewrite was discarded, not re-attempted.
        let reverted_tags = scan_start_tags(&reverted, &html(), 2).unwrap();
        assert_eq!(&reverted[reverted_tags[1].span.clone()], original_mid);

        // Sanity: the other two tags still carry their rewritten (wrapped) bytes,
        // i.e. the revert did not clobber the valid rewrites.
        assert!(reverted.contains("<a\n  href=\"1\"\n  rel=\"r\"\n>"));
        assert!(reverted.contains("<c\n  data=\"d\"\n  x=\"y\"\n>"));
    }

    // ----- residual document-level divergence (Requirement 7.7) ------------

    #[test]
    fn residual_document_divergence_remains_after_per_tag_reverts() {
        // A document-level break that canNOT be localized to any single tag's
        // before/after fingerprint: here the element SEQUENCE itself changed
        // (two tags in, one tag out). No per-tag revert can fix this because the
        // divergence is in how tags relate across the document, not inside one
        // tag's own rewrite. This is the case the engine maps to a Format_Error.
        let input = "<a x=\"1\"><b y=\"2\">";
        let residual = "<a x=\"1\">"; // the <b> tag vanished entirely

        // Per-tag check cannot even pair the tags up: the counts differ, so there
        // is no single tag whose revert would restore the document.
        let in_tags = scan_start_tags(input, &html(), 2).unwrap();
        let out_tags = scan_start_tags(residual, &html(), 2).unwrap();
        assert_ne!(
            in_tags.len(),
            out_tags.len(),
            "tag count changed — not a single-tag-localizable divergence"
        );

        // Document-level backstop reports non-equivalence regardless of sorting,
        // which the engine surfaces as a Format_Error leaving the input unchanged.
        let doc_in = structural_fingerprint(input, &html(), 2).unwrap();
        let doc_out = structural_fingerprint(residual, &html(), 2).unwrap();
        assert!(!fingerprints_equivalent(&doc_in, &doc_out, false));
        assert!(!fingerprints_equivalent(&doc_in, &doc_out, true));
    }

    #[test]
    fn residual_inter_tag_text_divergence_is_not_localizable_to_a_tag() {
        // A subtler residual divergence: every tag's own fingerprint is
        // unchanged, but the verbatim text BETWEEN tags was altered. No per-tag
        // revert touches inter-tag text, so this divergence survives all per-tag
        // reverts and must remain a document-level (Format_Error) divergence.
        let input = "<a href=\"x\">keep<b id=\"i\">tail";
        let text_broken = "<a href=\"x\">CHANGED<b id=\"i\">tail";

        // Every tag is individually equivalent (no tag-local divergence to revert).
        let in_tags = scan_start_tags(input, &html(), 2).unwrap();
        let out_tags = scan_start_tags(text_broken, &html(), 2).unwrap();
        assert_eq!(in_tags.len(), out_tags.len());
        for (b, a) in in_tags.iter().zip(out_tags.iter()) {
            assert!(tag_fingerprints_equivalent(
                &tag_fingerprint(b),
                &tag_fingerprint(a),
                false
            ));
        }

        // Yet the document fingerprints diverge on the changed text run.
        let doc_in = structural_fingerprint(input, &html(), 2).unwrap();
        let doc_out = structural_fingerprint(text_broken, &html(), 2).unwrap();
        assert!(!fingerprints_equivalent(&doc_in, &doc_out, false));
    }
}

/// Property-based self-check-soundness tests for [`angle_brackets_conserved`]
/// (bugfix spec task 8.6, property (d); Requirements 2.7, 7.5).
///
/// The angle-bracket-conservation invariant must be both **complete** and
/// **sound** for the self-check's purpose:
///
/// - **Complete (no false negatives).** For any generated buffer pair that
///   differs *only* in whitespace and attribute order — exactly the legitimate
///   transforms the plugin performs — `angle_brackets_conserved` returns `true`.
///   A legitimate wrap/sort never changes a `<`/`>` count, so the invariant must
///   never reject one.
/// - **Sound (catches the corruption).** For any generated rewrite that drops or
///   adds at least one `<` or `>` — the shape of the generics-misread corruption
///   — `angle_brackets_conserved` returns `false`. This is the hole the
///   invariant closes, and it must hold regardless of where in the buffer the
///   delimiter count changed.
///
/// Both directions are proven over broad generators rather than the handful of
/// hand-picked unit cases in [`tests`], so the invariant's two-sided contract is
/// established universally.
#[cfg(test)]
mod angle_bracket_soundness_property_tests {
    use super::angle_brackets_conserved;
    use proptest::prelude::*;

    /// A token that contributes a known number of `<` and `>` to a buffer. The
    /// generators assemble buffers from these so a transform's effect on the
    /// delimiter counts is exactly controllable.
    ///
    /// - `Text` carries no `<`/`>` (ASCII letters/digits/spaces plus a multibyte
    ///   glyph), so it is inert for the counts and is the only thing whitespace
    ///   perturbation touches.
    /// - `StartTag` and `EndTag` each contribute a balanced `<…>` pair.
    #[derive(Clone, Debug)]
    enum Token {
        Text(String),
        StartTag { name: String, attrs: Vec<String> },
        EndTag(String),
    }

    impl Token {
        /// Render with a chosen inter-attribute separator so two renderings of
        /// the SAME token list can differ purely in whitespace.
        fn render(&self, sep: &str) -> String {
            match self {
                Token::Text(t) => t.clone(),
                Token::StartTag { name, attrs } => {
                    let mut s = String::from("<");
                    s.push_str(name);
                    for a in attrs {
                        s.push_str(sep);
                        s.push_str(a);
                    }
                    s.push('>');
                    s
                }
                Token::EndTag(n) => format!("</{n}>"),
            }
        }
    }

    /// Inter-tag text with no `<`/`>` of its own (so tokens fully control the
    /// delimiter counts). A multibyte glyph is included to confirm the byte-level
    /// counter is not confused by multi-byte UTF-8 (both `<`/`>` are ASCII and
    /// can never be a continuation byte).
    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![Just(' '), Just('a'), Just('Z'), Just('0'), Just('漢')],
            0..5,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    /// A single attribute with no `<`/`>` in it.
    fn attr_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            "[a-z][a-z0-9]{0,4}",
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,5}").prop_map(|(n, v)| format!("{n}=\"{v}\"")),
        ]
    }

    /// A single token.
    fn token_strategy() -> impl Strategy<Value = Token> {
        prop_oneof![
            text_strategy().prop_map(Token::Text),
            ("[A-Za-z][A-Za-z0-9]{0,4}", prop::collection::vec(attr_strategy(), 0..4))
                .prop_map(|(name, attrs)| Token::StartTag { name, attrs }),
            "[A-Za-z][A-Za-z0-9]{0,4}".prop_map(Token::EndTag),
        ]
    }

    /// A token list — the shared structure both the "input" and the
    /// whitespace/order-perturbed "output" are rendered from.
    fn tokens_strategy() -> impl Strategy<Value = Vec<Token>> {
        prop::collection::vec(token_strategy(), 0..8)
    }

    /// A non-trivial inter-attribute separator (at least one whitespace char),
    /// so two renderings can differ in whitespace while staying well-formed.
    fn sep_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(prop_oneof![Just(' '), Just('\n'), Just('\t')], 1..4)
            .prop_map(|chars| chars.into_iter().collect())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]

        /// (d.i) Completeness — Validates: Requirements 2.7, 7.5.
        ///
        /// Rendering the SAME token list twice with different inter-attribute
        /// separators AND permuting each start tag's attributes changes only
        /// whitespace and attribute order — never a `<`/`>` count — so
        /// `angle_brackets_conserved` must return `true`.
        #[test]
        fn whitespace_and_reorder_only_rewrites_conserve_brackets(
            tokens in tokens_strategy(),
            sep_a in sep_strategy(),
            sep_b in sep_strategy(),
            rotate in 0usize..4,
        ) {
            // Input: render with separator A, attributes in source order.
            let input: String = tokens.iter().map(|t| t.render(&sep_a)).collect();

            // Output: render with separator B, and rotate each start tag's
            // attribute list (a pure permutation — same multiset of slices).
            let reordered: Vec<Token> = tokens
                .iter()
                .map(|t| match t {
                    Token::StartTag { name, attrs } if attrs.len() > 1 => {
                        let mut a = attrs.clone();
                        let k = rotate % a.len();
                        a.rotate_left(k);
                        Token::StartTag { name: name.clone(), attrs: a }
                    }
                    other => other.clone(),
                })
                .collect();
            let output: String = reordered.iter().map(|t| t.render(&sep_b)).collect();

            prop_assert!(
                angle_brackets_conserved(&input, &output),
                "a whitespace+reorder-only rewrite must conserve `<`/`>`:\n\
                 input  = {:?}\noutput = {:?}",
                input,
                output
            );
        }

        /// (d.ii) Soundness — Validates: Requirements 2.7, 7.5.
        ///
        /// Starting from any generated buffer, deleting or inserting at least one
        /// `<` or `>` (the shape of the generics-misread corruption, which drops
        /// a tag delimiter) must make `angle_brackets_conserved` return `false`,
        /// regardless of where in the buffer the change lands.
        #[test]
        fn dropped_or_added_bracket_breaks_conservation(
            tokens in tokens_strategy(),
            sep in sep_strategy(),
            which in any::<bool>(),
            add in any::<bool>(),
        ) {
            let input: String = tokens.iter().map(|t| t.render(&sep)).collect();
            let target = if which { '<' } else { '>' };

            // Soundness is about a CHANGED count. If we are dropping, we can only
            // do so when the target delimiter is present; if absent, fall back to
            // adding one so the property always exercises a genuine count change.
            let do_add = add || !input.contains(target);

            let output = if do_add {
                // Insert one target delimiter in inert leading position.
                let mut s = String::new();
                s.push(target);
                s.push_str(&input);
                s
            } else {
                // Remove exactly the first occurrence of the target delimiter.
                let pos = input.find(target).expect("present when dropping");
                let mut s = String::with_capacity(input.len() - 1);
                s.push_str(&input[..pos]);
                s.push_str(&input[pos + 1..]);
                s
            };

            // The transform changed the target delimiter's count by exactly one.
            let before = input.matches(target).count();
            let after = output.matches(target).count();
            prop_assert_ne!(before, after, "the forced edit must change the count");

            prop_assert!(
                !angle_brackets_conserved(&input, &output),
                "a dropped/added `{}` must break conservation:\ninput  = {:?}\noutput = {:?}",
                target,
                input,
                output
            );
        }
    }
}
