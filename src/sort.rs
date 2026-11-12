//! Attribute sorting with the `standard` scheme and behavior-preservation guards.
//!
//! This module defines the data model for an [`AttributeOrderScheme`] — an
//! ordered list of [`AttributeGroup`]s, each pairing a [`GroupMatcher`] (how an
//! attribute is assigned to the group) with an [`IntraGroupRule`] (how members
//! are ordered within the group) — and the single built-in `standard` scheme
//! (Requirements 5.1, 5.4, 5.8).
//!
//! The actual reordering of a tag's attributes, including the
//! behavior-preservation guards for duplicate names and spread/dynamic
//! overrides, is implemented by [`sort_attributes`] below.
//!
//! # Group precedence vs. render order
//!
//! Groups are listed in **render order** — the top-to-bottom order the design
//! gives (Requirement 5.1), which is the order in which the groups' attributes
//! appear in the rewritten start tag. Assignment of an attribute to a group,
//! however, uses **most-specific-wins** precedence: a [`GroupMatcher::ExactNames`]
//! or [`GroupMatcher::Prefixes`] group always claims an attribute ahead of a
//! [`GroupMatcher::Any`] group, regardless of their relative render positions.
//! This is what lets the `data-*`, `aria-*`, and event-handler groups (render
//! slots 7–9) claim their attributes even though the "other standard
//! attributes" catch-all (render slot 6) appears earlier in the list. The
//! [`AttributeOrderScheme::group_index_for`] helper implements this precedence
//! and returns the *render* index of the chosen group, so both assignment and
//! ordering come from a single source of truth.

use std::collections::HashMap;

use crate::model::{Attribute, AttributeKind};

/// How an [`AttributeGroup`] decides whether a given attribute belongs to it.
///
/// Matching is performed against an attribute's lowercased comparison name
/// (`Attribute::name_key`), so matchers are case-insensitive by construction:
/// callers compare already-lowercased names against the lowercased literals
/// stored here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupMatcher {
    /// Matches an attribute whose name is exactly one of the listed names.
    ///
    /// The order of names is significant only when the group's
    /// [`IntraGroupRule`] is [`IntraGroupRule::ExactNameOrder`], in which case
    /// members are ordered to follow this list (for example `name`, then
    /// `type`, then `value`).
    ExactNames(Vec<String>),
    /// Matches an attribute whose name starts with any of the listed prefixes,
    /// for example `data-` or `aria-`.
    Prefixes(Vec<String>),
    /// Matches any attribute. Used for the catch-all slots, which collect every
    /// attribute not claimed by a more specific group (Requirement 5.8).
    Any,
}

impl GroupMatcher {
    /// Returns `true` when `name_key` (an already-lowercased attribute name)
    /// belongs to this matcher.
    #[must_use]
    pub fn matches(&self, name_key: &str) -> bool {
        match self {
            GroupMatcher::ExactNames(names) => names.iter().any(|n| n == name_key),
            GroupMatcher::Prefixes(prefixes) => {
                prefixes.iter().any(|p| name_key.starts_with(p.as_str()))
            }
            GroupMatcher::Any => true,
        }
    }

    /// Whether this matcher is a specific (non-`Any`) matcher. Specific matchers
    /// win over [`GroupMatcher::Any`] during group assignment regardless of
    /// render order.
    #[must_use]
    pub fn is_specific(&self) -> bool {
        !matches!(self, GroupMatcher::Any)
    }
}

/// How the members of an [`AttributeGroup`] are ordered relative to one another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntraGroupRule {
    /// Preserve the original source order of the group's members (the default,
    /// Requirement 5.4).
    SourceOrder,
    /// Order members alphabetically by their comparison name.
    Alphabetical,
    /// Order members to follow the explicit name list of the group's
    /// [`GroupMatcher::ExactNames`] matcher (for example `name`, then `type`,
    /// then `value`); members not present in the list, or groups whose matcher
    /// is not `ExactNames`, fall back to source order. This is the "explicit
    /// intra-group ordering rule" permitted by Requirement 5.4.
    ExactNameOrder,
}

/// A single ordered group within an [`AttributeOrderScheme`]: a human-readable
/// `name`, the [`GroupMatcher`]s that assign attributes to it, and the
/// [`IntraGroupRule`] that orders its members.
///
/// A group carries a *list* of matchers so that the design's composite groups —
/// for example `aria-*` **and** `role` (`Prefixes(["aria-"]) + ExactNames(["role"])`) —
/// are expressible without widening the [`GroupMatcher`] vocabulary. An
/// attribute belongs to the group when **any** of its matchers matches. A group
/// is considered specific (wins over an `Any` catch-all) when any of its
/// matchers is specific.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeGroup {
    /// Human-readable group name, used for documentation and tests only; it is
    /// never emitted into formatted output.
    pub name: &'static str,
    /// How attributes are assigned to this group. An attribute matches the group
    /// when any matcher in this list matches it.
    pub matchers: Vec<GroupMatcher>,
    /// How members of this group are ordered relative to one another.
    pub intra: IntraGroupRule,
}

impl AttributeGroup {
    /// Returns `true` when `name_key` matches any of this group's matchers.
    #[must_use]
    pub fn matches(&self, name_key: &str) -> bool {
        self.matchers.iter().any(|m| m.matches(name_key))
    }

    /// Whether this group is specific — i.e. has at least one non-`Any` matcher.
    /// Specific groups win over `Any`-only groups during assignment.
    #[must_use]
    pub fn is_specific(&self) -> bool {
        self.matchers.iter().any(GroupMatcher::is_specific)
    }

    /// The group's explicit ordered name list, when its ordering is driven by an
    /// [`IntraGroupRule::ExactNameOrder`] rule over an [`GroupMatcher::ExactNames`]
    /// matcher. Returns the first `ExactNames` list found, or an empty slice when
    /// the group has no such matcher. Used by [`sort_attributes`] to order
    /// members such as `name`, `type`, `value`.
    #[must_use]
    pub fn exact_name_order(&self) -> &[String] {
        for matcher in &self.matchers {
            if let GroupMatcher::ExactNames(names) = matcher {
                return names;
            }
        }
        &[]
    }
}

/// A named, ordered set of attribute grouping rules.
///
/// The `groups` list is in **render order** (Requirement 5.1). Group assignment
/// uses most-specific-wins precedence via [`AttributeOrderScheme::group_index_for`]
/// so that `ExactNames`/`Prefixes` groups claim their attributes ahead of any
/// [`GroupMatcher::Any`] catch-all, while the returned render index preserves the
/// design's top-to-bottom ordering. Within a group, members are ordered by the
/// group's [`IntraGroupRule`] (Requirement 5.4). The final group is a
/// [`GroupMatcher::Any`] catch-all so every attribute has a home and unmatched
/// attributes are placed last, preserving their source order (Requirement 5.8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeOrderScheme {
    /// The ordered list of groups, in render order (first group renders first).
    pub groups: Vec<AttributeGroup>,
}

impl AttributeOrderScheme {
    /// Resolve the render index of the group an attribute with the given
    /// lowercased `name_key` belongs to, using most-specific-wins precedence.
    ///
    /// A specific ([`GroupMatcher::ExactNames`] / [`GroupMatcher::Prefixes`])
    /// group always wins over a [`GroupMatcher::Any`] group. Among specific
    /// groups, the earliest in render order wins; likewise for `Any` groups.
    /// Returns `None` only if the scheme has no group that matches (which cannot
    /// happen for the `standard` scheme, as it ends in an `Any` catch-all).
    #[must_use]
    pub fn group_index_for(&self, name_key: &str) -> Option<usize> {
        // First pass: the earliest specific group that matches.
        let specific = self
            .groups
            .iter()
            .position(|g| g.is_specific() && g.matches(name_key));
        if specific.is_some() {
            return specific;
        }
        // Fallback: the earliest non-specific (`Any`-only) group.
        self.groups.iter().position(|g| !g.is_specific())
    }

    /// Build the built-in `standard` scheme.
    ///
    /// The `standard` scheme is a **union across dialects**: its groups include
    /// HTML, JSX, Vue, Svelte, and Astro forms coexisting in one ordered list.
    /// On a pure HTML file the framework-specific groups are simply inert and
    /// never match any attribute, which is correct and intended. Groups are in
    /// render order; within each group the intra-group rule is
    /// [`IntraGroupRule::SourceOrder`] unless noted (Requirement 5.4).
    ///
    /// The ten groups, in render order:
    ///
    /// 1. Definition / identity — `is`, `key`, element-binding directives.
    /// 2. Structural framework directives — Vue `v-*` control flow, Astro
    ///    `client:*`.
    /// 3. `id`.
    /// 4. `class` / `className` / `:class` / `class:`.
    /// 5. `name`, then `type`, then `value` (explicit intra order).
    /// 6. Other standard attributes — ordinary name-value / plain attributes.
    /// 7. `data-*`.
    /// 8. `aria-*` and `role`.
    /// 9. Event handlers — `on*`, `on:*`, `@*`.
    /// 10. Catch-all / unmatched — placed last, source order (Requirement 5.8).
    #[must_use]
    pub fn standard() -> Self {
        Self {
            groups: vec![
                // 1. Definition / identity — `is`, `key`, element-binding directives.
                AttributeGroup {
                    name: "definition-identity",
                    matchers: vec![GroupMatcher::ExactNames(vec![
                        "is".to_string(),
                        "key".to_string(),
                        "v-bind:key".to_string(),
                        ":key".to_string(),
                    ])],
                    intra: IntraGroupRule::SourceOrder,
                },
                // 2. Structural framework directives — Vue `v-*` control flow,
                //    Astro `client:*`.
                AttributeGroup {
                    name: "structural-directives",
                    matchers: vec![
                        GroupMatcher::ExactNames(vec![
                            "v-if".to_string(),
                            "v-else-if".to_string(),
                            "v-else".to_string(),
                            "v-for".to_string(),
                            "v-show".to_string(),
                        ]),
                        GroupMatcher::Prefixes(vec!["client:".to_string()]),
                    ],
                    intra: IntraGroupRule::SourceOrder,
                },
                // 3. id.
                AttributeGroup {
                    name: "id",
                    matchers: vec![GroupMatcher::ExactNames(vec!["id".to_string()])],
                    intra: IntraGroupRule::SourceOrder,
                },
                // 4. class / className / :class / class:.
                AttributeGroup {
                    name: "class",
                    matchers: vec![GroupMatcher::ExactNames(vec![
                        "class".to_string(),
                        "classname".to_string(),
                        ":class".to_string(),
                        "class:".to_string(),
                    ])],
                    intra: IntraGroupRule::SourceOrder,
                },
                // 5. name, then type, then value (explicit intra order).
                AttributeGroup {
                    name: "name-type-value",
                    matchers: vec![GroupMatcher::ExactNames(vec![
                        "name".to_string(),
                        "type".to_string(),
                        "value".to_string(),
                    ])],
                    intra: IntraGroupRule::ExactNameOrder,
                },
                // 6. Other standard attributes — ordinary name-value / plain.
                AttributeGroup {
                    name: "other-standard",
                    matchers: vec![GroupMatcher::Any],
                    intra: IntraGroupRule::SourceOrder,
                },
                // 7. data-*.
                AttributeGroup {
                    name: "data",
                    matchers: vec![GroupMatcher::Prefixes(vec!["data-".to_string()])],
                    intra: IntraGroupRule::SourceOrder,
                },
                // 8. aria-* and role (`Prefixes(["aria-"]) + ExactNames(["role"])`).
                AttributeGroup {
                    name: "aria-role",
                    matchers: vec![
                        GroupMatcher::Prefixes(vec!["aria-".to_string()]),
                        GroupMatcher::ExactNames(vec!["role".to_string()]),
                    ],
                    intra: IntraGroupRule::SourceOrder,
                },
                // 9. Event handlers — on*, on:*, @*.
                AttributeGroup {
                    name: "event-handlers",
                    matchers: vec![GroupMatcher::Prefixes(vec![
                        "on".to_string(),
                        "on:".to_string(),
                        "@".to_string(),
                    ])],
                    intra: IntraGroupRule::SourceOrder,
                },
                // 10. Catch-all / unmatched.
                AttributeGroup {
                    name: "catch-all",
                    matchers: vec![GroupMatcher::Any],
                    intra: IntraGroupRule::SourceOrder,
                },
            ],
        }
    }
}

/// Reorder a start tag's attributes according to `scheme`, preserving every
/// behavior-affecting relative order (Requirement 5).
///
/// The result is always a **permutation** of `attrs` — no attribute is added,
/// dropped, or mutated (Requirement 5.3) — and the function is **pure and
/// deterministic**: the same inputs always yield the same ordering
/// (Requirement 5.9). It never panics.
///
/// # Algorithm
///
/// The sort is a stable, two-layered process bounded by the two
/// behavior-preservation guards from the design (Requirement 5.5):
///
/// 1. **Spread/dynamic barriers.** Each [`AttributeKind::DynamicSpread`]
///    attribute is a fixed barrier that never moves: it stays at its exact
///    source index, and spreads never cross one another. The remaining
///    attributes are partitioned into the *segments* lying strictly before the
///    first spread, between consecutive spreads, and after the last spread.
///    Only within a single segment may attributes be reordered. This is the
///    conservative rule that guarantees no reordering can change which value a
///    spread overrides or is overridden by (Requirement 5.5).
///
/// 2. **Per-segment group sort.** Within a segment the attributes are
///    stable-partitioned by their group's **render index**
///    ([`AttributeOrderScheme::group_index_for`]); attributes sharing a group
///    are then ordered by that group's [`IntraGroupRule`] (Requirement 5.4).
///    The trailing [`GroupMatcher::Any`] catch-all collects everything a
///    specific group did not claim and keeps those attributes in source order
///    (Requirement 5.8).
///
/// 3. **Duplicate-name locking.** Within a segment, every set of two or more
///    attributes sharing the same `name_key` forms a *locked unit*: the unit
///    moves as a whole, anchored to its first member's group and source
///    position, and its members keep their original relative order
///    (Requirement 5.5). A pure spread has no `name_key` and is never part of a
///    duplicate set (and is already a barrier besides).
///
/// Because partitioning and the intra-group sort are both **stable** and keyed
/// only on source position and comparison name, re-running the sort on its own
/// output is a fixed point, which gives determinism (Requirement 5.9).
#[must_use]
pub fn sort_attributes<'a>(
    attrs: &[Attribute<'a>],
    scheme: &AttributeOrderScheme,
) -> Vec<Attribute<'a>> {
    let mut out: Vec<Attribute<'a>> = Vec::with_capacity(attrs.len());

    // Walk the list, carving out maximal spread-free segments. Each spread is a
    // barrier emitted in place; each segment between barriers is sorted on its
    // own so no attribute ever crosses a spread.
    let mut segment_start = 0usize;
    for (i, attr) in attrs.iter().enumerate() {
        if attr.kind == AttributeKind::DynamicSpread {
            sort_segment(&attrs[segment_start..i], scheme, &mut out);
            out.push(attr.clone());
            segment_start = i + 1;
        }
    }
    sort_segment(&attrs[segment_start..], scheme, &mut out);

    out
}

/// A sortable unit within a segment: one or more attributes that travel
/// together. A unit of length > 1 is a locked duplicate-name set. `anchor` is
/// the segment-local index of the unit's first member, used as the unit's group
/// anchor and as the stable ordering key; `members` are segment-local indices
/// in source order.
struct Unit {
    anchor: usize,
    group: usize,
    members: Vec<usize>,
}

/// Sort one spread-free `segment` and append the result to `out`.
///
/// Implements layers 2 and 3 of the [`sort_attributes`] algorithm: duplicate
/// names are collapsed into locked units anchored at their first member, units
/// are stable-partitioned by group render index, and each group's members are
/// ordered by its [`IntraGroupRule`]. Indices below are *segment-local*.
fn sort_segment<'a>(
    segment: &[Attribute<'a>],
    scheme: &AttributeOrderScheme,
    out: &mut Vec<Attribute<'a>>,
) {
    if segment.is_empty() {
        return;
    }

    // Group render index for every attribute. The standard scheme always ends
    // in an `Any` catch-all, so `group_index_for` only returns `None` for a
    // scheme with no catch-all; such attributes are treated as belonging to a
    // synthetic "after everything" bucket so they land last in source order
    // (Requirement 5.8).
    let catch_all_index = scheme.groups.len();
    let group_of = |attr: &Attribute<'a>| -> usize {
        attr.name_key
            .as_deref()
            .and_then(|key| scheme.group_index_for(key))
            .or_else(|| scheme.group_index_for(""))
            .unwrap_or(catch_all_index)
    };

    // Build the units. The first occurrence of a name_key creates a unit; later
    // occurrences of the same name_key append to that unit (locking duplicates).
    // Attributes with no name_key (none remain here in practice, since pure
    // spreads are barriers, but named dynamic forms could in theory) each get
    // their own unit.
    let mut units: Vec<Unit> = Vec::new();
    let mut first_unit_for: HashMap<&str, usize> = HashMap::new();
    for (local, attr) in segment.iter().enumerate() {
        if let Some(key) = attr.name_key.as_deref() {
            if let Some(&unit_idx) = first_unit_for.get(key) {
                units[unit_idx].members.push(local);
                continue;
            }
            first_unit_for.insert(key, units.len());
        }
        units.push(Unit {
            anchor: local,
            group: group_of(attr),
            members: vec![local],
        });
    }

    // Stable partition of units by group render index: stable so units within
    // the same group keep source order before the intra-group rule runs. Rust's
    // `sort_by_key` is stable, and keying on the group index alone preserves the
    // source ordering of equal-group units.
    units.sort_by_key(|u| u.group);

    // Order the units within each contiguous same-group run by the group's
    // intra-group rule, then emit their members (locked sets stay intact).
    let mut i = 0usize;
    while i < units.len() {
        let group = units[i].group;
        let mut j = i + 1;
        while j < units.len() && units[j].group == group {
            j += 1;
        }
        order_group_units(&mut units[i..j], group, segment, scheme);
        for unit in &units[i..j] {
            for &member in &unit.members {
                out.push(segment[member].clone());
            }
        }
        i = j;
    }
}

/// Order the units of a single group run in place according to the group's
/// [`IntraGroupRule`]. The slice already holds units in stable source order, so
/// [`IntraGroupRule::SourceOrder`] (and any group index past the scheme, i.e.
/// the synthetic catch-all) is a no-op.
fn order_group_units(
    run: &mut [Unit],
    group: usize,
    segment: &[Attribute<'_>],
    scheme: &AttributeOrderScheme,
) {
    // The comparison name of a unit's anchor member, or the empty string for a
    // nameless anchor (so nameless units sort deterministically, before any
    // named one).
    let anchor_key = |unit: &Unit| -> &str { segment[unit.anchor].name_key.as_deref().unwrap_or("") };

    let Some(group_def) = scheme.groups.get(group) else {
        return; // Synthetic catch-all bucket: keep source order (Requirement 5.8).
    };
    match group_def.intra {
        IntraGroupRule::SourceOrder => {}
        IntraGroupRule::Alphabetical => {
            // Stable sort by the anchor member's comparison name; equal names
            // (which, being equal name_keys, are a single locked unit anyway)
            // keep source order.
            run.sort_by(|a, b| anchor_key(a).cmp(anchor_key(b)));
        }
        IntraGroupRule::ExactNameOrder => {
            let order = group_def.exact_name_order();
            // Position in the explicit list, or a value past the list for names
            // not present, so unlisted members fall back to the end in source
            // order (the stable sort preserves their relative source order).
            run.sort_by_key(|unit| {
                let key = anchor_key(unit);
                order.iter().position(|n| n == key).unwrap_or(order.len())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::display_width;

    /// The expected render-ordered group names of the `standard` scheme, from
    /// the design's ten-group list.
    const STANDARD_GROUP_NAMES: [&str; 10] = [
        "definition-identity",
        "structural-directives",
        "id",
        "class",
        "name-type-value",
        "other-standard",
        "data",
        "aria-role",
        "event-handlers",
        "catch-all",
    ];

    #[test]
    fn standard_scheme_has_ten_groups_in_render_order() {
        let scheme = AttributeOrderScheme::standard();
        assert_eq!(scheme.groups.len(), 10, "standard scheme has exactly 10 groups");
        let names: Vec<&str> = scheme.groups.iter().map(|g| g.name).collect();
        assert_eq!(names.as_slice(), &STANDARD_GROUP_NAMES);
    }

    #[test]
    fn standard_scheme_ends_in_any_catch_all_with_source_order() {
        let scheme = AttributeOrderScheme::standard();
        let last = scheme.groups.last().expect("non-empty scheme");
        assert_eq!(last.name, "catch-all");
        assert_eq!(last.matchers, vec![GroupMatcher::Any]);
        assert_eq!(last.intra, IntraGroupRule::SourceOrder);
    }

    #[test]
    fn name_type_value_group_uses_explicit_intra_order() {
        let scheme = AttributeOrderScheme::standard();
        let group = &scheme.groups[4];
        assert_eq!(group.name, "name-type-value");
        assert_eq!(group.intra, IntraGroupRule::ExactNameOrder);
        // The explicit order is name, then type, then value (Requirement 5.4).
        assert_eq!(group.exact_name_order(), &["name", "type", "value"]);
    }

    #[test]
    fn all_groups_except_name_type_value_default_to_source_order() {
        let scheme = AttributeOrderScheme::standard();
        for group in &scheme.groups {
            if group.name == "name-type-value" {
                assert_eq!(group.intra, IntraGroupRule::ExactNameOrder);
            } else {
                assert_eq!(
                    group.intra,
                    IntraGroupRule::SourceOrder,
                    "group {} should default to SourceOrder (Requirement 5.4)",
                    group.name
                );
            }
        }
    }

    #[test]
    fn exact_name_matcher_matches_only_listed_names() {
        let matcher = GroupMatcher::ExactNames(vec!["id".to_string(), "name".to_string()]);
        assert!(matcher.matches("id"));
        assert!(matcher.matches("name"));
        assert!(!matcher.matches("idx"));
        assert!(!matcher.matches("names"));
        assert!(matcher.is_specific());
    }

    #[test]
    fn prefix_matcher_matches_by_prefix() {
        let matcher = GroupMatcher::Prefixes(vec!["data-".to_string(), "aria-".to_string()]);
        assert!(matcher.matches("data-id"));
        assert!(matcher.matches("aria-label"));
        assert!(!matcher.matches("datum"));
        assert!(!matcher.matches("role"));
        assert!(matcher.is_specific());
    }

    #[test]
    fn any_matcher_matches_everything_and_is_not_specific() {
        let matcher = GroupMatcher::Any;
        assert!(matcher.matches("anything"));
        assert!(matcher.matches(""));
        assert!(!matcher.is_specific());
    }

    #[test]
    fn group_assignment_resolves_canonical_attributes_to_expected_groups() {
        let scheme = AttributeOrderScheme::standard();
        let idx = |name: &str| scheme.group_index_for(name).expect("catch-all always matches");

        assert_eq!(idx("is"), 0);
        assert_eq!(idx("key"), 0);
        assert_eq!(idx(":key"), 0);
        assert_eq!(idx("v-if"), 1);
        assert_eq!(idx("client:load"), 1);
        assert_eq!(idx("id"), 2);
        assert_eq!(idx("class"), 3);
        assert_eq!(idx("classname"), 3);
        assert_eq!(idx(":class"), 3);
        assert_eq!(idx("name"), 4);
        assert_eq!(idx("type"), 4);
        assert_eq!(idx("value"), 4);
        assert_eq!(idx("data-track"), 6);
        assert_eq!(idx("aria-label"), 7);
        assert_eq!(idx("role"), 7);
        assert_eq!(idx("onclick"), 8);
        assert_eq!(idx("on:click"), 8);
        assert_eq!(idx("@click"), 8);
    }

    #[test]
    fn ordinary_attributes_fall_into_other_standard_not_catch_all() {
        // An ordinary attribute that matches no specific group lands in the
        // first `Any` group ("other-standard", render slot 6), not the final
        // catch-all, so ordinary attributes render before data-/aria-/events.
        let scheme = AttributeOrderScheme::standard();
        assert_eq!(scheme.group_index_for("placeholder"), Some(5));
        assert_eq!(scheme.group_index_for("href"), Some(5));
        assert_eq!(scheme.group_index_for("disabled"), Some(5));
    }

    #[test]
    fn specific_groups_win_over_earlier_any_group() {
        // `data-*`, `aria-*`, and event handlers (render slots 7-9) must claim
        // their attributes even though the `Any` "other-standard" group (slot 6)
        // appears earlier in render order (Requirement 5.1 ordering with
        // most-specific-wins assignment).
        let scheme = AttributeOrderScheme::standard();
        assert_eq!(scheme.group_index_for("data-x"), Some(6));
        assert_eq!(scheme.group_index_for("aria-hidden"), Some(7));
        assert_eq!(scheme.group_index_for("onsubmit"), Some(8));
    }

    #[test]
    fn aria_role_group_matches_both_prefix_and_exact_role() {
        let scheme = AttributeOrderScheme::standard();
        let group = &scheme.groups[7];
        assert_eq!(group.name, "aria-role");
        assert!(group.matches("aria-expanded"));
        assert!(group.matches("role"));
        assert!(!group.matches("roles"));
        assert!(group.is_specific());
    }

    #[test]
    fn standard_scheme_is_deterministic() {
        // Repeated construction yields an identical scheme (Requirement 5.9
        // determinism of the ordering data).
        assert_eq!(AttributeOrderScheme::standard(), AttributeOrderScheme::standard());
    }

    // --- sort_attributes helpers -------------------------------------------

    /// Build a named-ish attribute whose `raw` doubles as its identity in
    /// assertions. `name_key` is derived from `raw` up to the first `=`,
    /// lowercased, matching how the scanner sets comparison names.
    fn attr(kind: AttributeKind, raw: &str) -> Attribute<'_> {
        let name = raw.split(['=', ' ']).next().unwrap_or(raw);
        Attribute {
            kind,
            name_key: Some(name.to_ascii_lowercase()),
            raw,
            width: display_width(raw, 2),
        }
    }

    /// A plain/name-value attribute (kind does not affect sorting; only
    /// `DynamicSpread` acts as a barrier).
    fn nv(raw: &str) -> Attribute<'_> {
        attr(AttributeKind::NameValue, raw)
    }

    /// A pure spread attribute with no comparison name (`{...props}`).
    fn spread(raw: &str) -> Attribute<'_> {
        Attribute {
            kind: AttributeKind::DynamicSpread,
            name_key: None,
            raw,
            width: display_width(raw, 2),
        }
    }

    /// The `raw` slices of a sorted attribute list, for compact assertions.
    fn raws<'a>(attrs: &[Attribute<'a>]) -> Vec<&'a str> {
        attrs.iter().map(|a| a.raw).collect()
    }

    #[test]
    fn sorts_into_group_render_order() {
        // A grab-bag of attributes in scrambled source order is reordered into
        // the scheme's render order (Requirement 5.1): id, class, name/type/value,
        // other, data-, aria-, events.
        let scheme = AttributeOrderScheme::standard();
        let input = [
            nv("onclick=\"f()\""),
            nv("data-x=\"1\""),
            nv("class=\"c\""),
            nv("href=\"/\""),
            nv("id=\"i\""),
            nv("aria-label=\"a\""),
            nv("type=\"text\""),
        ];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(
            raws(&got),
            vec![
                "id=\"i\"",
                "class=\"c\"",
                "type=\"text\"",
                "href=\"/\"",
                "data-x=\"1\"",
                "aria-label=\"a\"",
                "onclick=\"f()\"",
            ],
        );
    }

    #[test]
    fn preserves_source_order_within_a_group() {
        // Two `other-standard` attributes keep their source order (Requirement
        // 5.4, SourceOrder default).
        let scheme = AttributeOrderScheme::standard();
        let input = [nv("href=\"/\""), nv("rel=\"next\""), nv("title=\"t\"")];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(raws(&got), vec!["href=\"/\"", "rel=\"next\"", "title=\"t\""]);
    }

    #[test]
    fn applies_exact_name_order_intra_group_rule() {
        // The name/type/value group orders members by its explicit list
        // regardless of source order (Requirement 5.4 ExactNameOrder).
        let scheme = AttributeOrderScheme::standard();
        let input = [nv("value=\"v\""), nv("name=\"n\""), nv("type=\"text\"")];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(raws(&got), vec!["name=\"n\"", "type=\"text\"", "value=\"v\""]);
    }

    #[test]
    fn exact_name_order_unlisted_members_fall_back_to_source_order() {
        // Only name/type/value are in the explicit list; any other member of
        // the same group (there are none in `standard`, so this is a synthetic
        // scheme check) would trail in source order. Here we confirm a partial
        // set still orders by the list.
        let scheme = AttributeOrderScheme::standard();
        let input = [nv("type=\"text\""), nv("name=\"n\"")];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(raws(&got), vec!["name=\"n\"", "type=\"text\""]);
    }

    #[test]
    fn unmatched_catch_all_attributes_trail_in_source_order() {
        // The `standard` scheme routes ordinary attributes to the `other-standard`
        // Any group, so to exercise the trailing catch-all we use a scheme whose
        // only specific group is `id`; everything else lands in the final Any
        // catch-all and keeps source order (Requirement 5.8).
        let scheme = AttributeOrderScheme {
            groups: vec![
                AttributeGroup {
                    name: "id",
                    matchers: vec![GroupMatcher::ExactNames(vec!["id".to_string()])],
                    intra: IntraGroupRule::SourceOrder,
                },
                AttributeGroup {
                    name: "catch-all",
                    matchers: vec![GroupMatcher::Any],
                    intra: IntraGroupRule::SourceOrder,
                },
            ],
        };
        let input = [nv("zeta=\"z\""), nv("id=\"i\""), nv("alpha=\"a\"")];
        let got = sort_attributes(&input, &scheme);
        // id first, then the two unmatched attributes in source order.
        assert_eq!(raws(&got), vec!["id=\"i\"", "zeta=\"z\"", "alpha=\"a\""]);
    }

    #[test]
    fn duplicate_names_keep_relative_order_and_move_as_a_unit() {
        // Two `class` attributes (a duplicate-name set) must keep their relative
        // source order and travel together anchored at the first occurrence
        // (Requirement 5.5). The set moves to the class group as a whole.
        let scheme = AttributeOrderScheme::standard();
        let input = [
            nv("href=\"/\""),
            nv("class=\"first\""),
            nv("id=\"i\""),
            nv("class=\"second\""),
        ];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(
            raws(&got),
            vec!["id=\"i\"", "class=\"first\"", "class=\"second\"", "href=\"/\""],
        );
    }

    #[test]
    fn spread_is_a_barrier_that_never_moves() {
        // A spread pins its position: attributes before it sort among themselves,
        // attributes after it sort among themselves, and nothing crosses the
        // spread (Requirement 5.5).
        let scheme = AttributeOrderScheme::standard();
        let input = [
            nv("onclick=\"f()\""),
            nv("id=\"i\""),
            spread("{...props}"),
            nv("data-x=\"1\""),
            nv("class=\"c\""),
        ];
        let got = sort_attributes(&input, &scheme);
        // Before the spread: id (group 2) then onclick (group 8). After: class
        // (group 3) then data-x (group 6). The spread stays in the middle.
        assert_eq!(
            raws(&got),
            vec![
                "id=\"i\"",
                "onclick=\"f()\"",
                "{...props}",
                "class=\"c\"",
                "data-x=\"1\"",
            ],
        );
    }

    #[test]
    fn multiple_spreads_never_cross_one_another() {
        // Each spread is an independent barrier; segments between them sort
        // independently and the spreads keep their relative order (Requirement 5.5).
        let scheme = AttributeOrderScheme::standard();
        let input = [
            spread("{...a}"),
            nv("onclick=\"f()\""),
            nv("id=\"i\""),
            spread("{...b}"),
            nv("class=\"c\""),
        ];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(
            raws(&got),
            vec!["{...a}", "id=\"i\"", "onclick=\"f()\"", "{...b}", "class=\"c\""],
        );
    }

    #[test]
    fn output_is_a_permutation_of_the_input() {
        // Sorting adds and removes nothing (Requirement 5.3): the multiset of
        // raw slices is identical before and after.
        let scheme = AttributeOrderScheme::standard();
        let input = [
            nv("data-x=\"1\""),
            nv("class=\"c\""),
            spread("{...props}"),
            nv("class=\"c2\""),
            nv("id=\"i\""),
            nv("onclick=\"f()\""),
            spread("v-bind=\"obj\""),
            nv("aria-label=\"a\""),
        ];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(got.len(), input.len());
        let mut before = raws(&input);
        let mut after = raws(&got);
        before.sort_unstable();
        after.sort_unstable();
        assert_eq!(before, after, "output must be a permutation of the input");
    }

    #[test]
    fn named_dynamic_spread_barrier_with_name_key_still_pins() {
        // `v-bind="obj"` is a DynamicSpread that carries a name_key; it must
        // still act as a barrier and never move, just like a pure spread.
        let scheme = AttributeOrderScheme::standard();
        let mut vbind = nv("v-bind=\"obj\"");
        vbind.kind = AttributeKind::DynamicSpread;
        let input = [nv("onclick=\"f()\""), vbind, nv("id=\"i\"")];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(raws(&got), vec!["onclick=\"f()\"", "v-bind=\"obj\"", "id=\"i\""]);
    }

    #[test]
    fn empty_and_single_inputs_are_handled() {
        let scheme = AttributeOrderScheme::standard();
        let empty: Vec<&str> = raws(&sort_attributes(&[], &scheme));
        assert_eq!(empty, Vec::<&str>::new());
        let one = [nv("id=\"i\"")];
        assert_eq!(raws(&sort_attributes(&one, &scheme)), vec!["id=\"i\""]);
    }

    #[test]
    fn sort_is_deterministic_and_idempotent() {
        // Re-sorting identical input yields identical output, and sorting the
        // sorted output is a fixed point (Requirement 5.9).
        let scheme = AttributeOrderScheme::standard();
        let input = [
            nv("onclick=\"f()\""),
            nv("data-x=\"1\""),
            nv("class=\"c\""),
            spread("{...props}"),
            nv("id=\"i\""),
            nv("class=\"c2\""),
        ];
        let first = sort_attributes(&input, &scheme);
        let again = sort_attributes(&input, &scheme);
        assert_eq!(raws(&first), raws(&again), "same input -> same output");
        let fixed = sort_attributes(&first, &scheme);
        assert_eq!(raws(&first), raws(&fixed), "sorting sorted output is a fixed point");
    }

    #[test]
    fn alphabetical_intra_group_rule_orders_by_name() {
        // Exercise the Alphabetical rule with a synthetic scheme: members of the
        // group are ordered by comparison name (Requirement 5.4).
        let scheme = AttributeOrderScheme {
            groups: vec![AttributeGroup {
                name: "alpha",
                matchers: vec![GroupMatcher::Any],
                intra: IntraGroupRule::Alphabetical,
            }],
        };
        let input = [nv("gamma=\"g\""), nv("alpha=\"a\""), nv("beta=\"b\"")];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(raws(&got), vec!["alpha=\"a\"", "beta=\"b\"", "gamma=\"g\""]);
    }
}

#[cfg(test)]
mod sorting_unit_tests {
    //! Task 7.3 — focused unit coverage for `sort_attributes` under the
    //! built-in `standard` scheme: placement of a representative attribute from
    //! every one of the ten groups, duplicate-name locking, spread/dynamic
    //! locking, and the unmatched-attribute (catch-all) suffix behavior.
    //!
    //! These tests live in a separate module from `mod tests` so they compose
    //! cleanly with the property-test modules added alongside them, and never
    //! edit or remove the existing example tests. They validate Requirements
    //! 5.4 (intra-group ordering), 5.5 (behavior-affecting order is locked), and
    //! 5.8 (unmatched attributes trail, in source order).

    use super::*;
    use crate::model::{Attribute, AttributeKind};

    /// Build a name-value attribute whose `raw` doubles as its identity in
    /// assertions. `name_key` is the lowercased text up to the first `=` or
    /// space, matching how the scanner derives comparison names.
    fn nv(raw: &str) -> Attribute<'_> {
        let name = raw.split(['=', ' ']).next().unwrap_or(raw);
        Attribute {
            kind: AttributeKind::NameValue,
            name_key: Some(name.to_ascii_lowercase()),
            raw,
            width: raw.len(),
        }
    }

    /// Build a pure spread attribute with no comparison name (`{...x}`).
    fn spread(raw: &str) -> Attribute<'_> {
        Attribute {
            kind: AttributeKind::DynamicSpread,
            name_key: None,
            raw,
            width: raw.len(),
        }
    }

    /// The `raw` slices of a sorted attribute list, for compact assertions.
    fn raws<'a>(attrs: &[Attribute<'a>]) -> Vec<&'a str> {
        attrs.iter().map(|a| a.raw).collect()
    }

    /// A single representative attribute from each of the ten `standard`
    /// groups, supplied in deliberately scrambled source order, is reordered
    /// into the scheme's full render order (Requirement 5.1 ordering with the
    /// intra-group and catch-all rules of 5.4 / 5.8).
    ///
    /// Note the `standard` scheme routes ordinary attributes into the
    /// "other-standard" `Any` group (render slot 6) rather than the final
    /// `catch-all` `Any` group (render slot 10), so a representative of the
    /// final catch-all is not expressible on the `standard` scheme; the
    /// catch-all's own source-order behavior is covered separately below with a
    /// scheme that actually routes attributes to it.
    #[test]
    fn one_attribute_from_each_standard_group_lands_in_full_render_order() {
        let scheme = AttributeOrderScheme::standard();
        // Scrambled input: one attribute per group, intentionally not in render
        // order, so the assertion proves the sort — not the input — establishes
        // the order.
        let input = [
            nv("@click=\"f\""),       // 9: event handlers (@*)
            nv("aria-label=\"a\""),   // 8: aria-* / role
            nv("data-track=\"t\""),   // 7: data-*
            nv("href=\"/\""),         // 6: other-standard (ordinary)
            nv("type=\"text\""),      // 5: name/type/value
            nv("class=\"c\""),        // 4: class
            nv("id=\"i\""),           // 3: id
            nv("v-if=\"ok\""),        // 2: structural directives
            nv("is=\"x\""),           // 1: definition / identity
        ];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(
            raws(&got),
            vec![
                "is=\"x\"",         // group 1
                "v-if=\"ok\"",      // group 2
                "id=\"i\"",         // group 3
                "class=\"c\"",      // group 4
                "type=\"text\"",    // group 5
                "href=\"/\"",       // group 6
                "data-track=\"t\"", // group 7
                "aria-label=\"a\"", // group 8
                "@click=\"f\"",     // group 9
            ],
        );
    }

    /// Every attribute from a group renders strictly after every attribute from
    /// a lower-numbered group: assert the pairwise ordering for each adjacent
    /// pair of groups 1..=9 independently, so a regression in any single group's
    /// render slot is pinpointed by the failing pair rather than hidden inside a
    /// single large fixture (Requirement 5.1 / 5.4 placement).
    #[test]
    fn each_standard_group_renders_after_the_previous_group() {
        let scheme = AttributeOrderScheme::standard();
        // Representatives for groups 1..=9 in render order.
        let reps = [
            "is=\"x\"",
            "v-show=\"ok\"",
            "id=\"i\"",
            "className=\"c\"",
            "name=\"n\"",
            "rel=\"next\"",
            "data-x=\"1\"",
            "role=\"button\"",
            "onclick=\"f()\"",
        ];
        for window in reps.windows(2) {
            let earlier = window[0];
            let later = window[1];
            // Feed the pair reversed; the sort must restore render order.
            let input = [nv(later), nv(earlier)];
            let got = sort_attributes(&input, &scheme);
            assert_eq!(
                raws(&got),
                vec![earlier, later],
                "{earlier} (earlier group) must render before {later} (later group)",
            );
        }
    }

    /// The name/type/value group applies its explicit intra-group order
    /// (Requirement 5.4) while the surrounding groups keep source order, proving
    /// group placement and intra-group ordering compose correctly.
    #[test]
    fn name_type_value_intra_order_holds_between_neighboring_groups() {
        let scheme = AttributeOrderScheme::standard();
        let input = [
            nv("value=\"v\""), // group 5, listed last
            nv("href=\"/\""),  // group 6
            nv("type=\"t\""),  // group 5, listed middle
            nv("class=\"c\""), // group 4
            nv("name=\"n\""),  // group 5, listed first
        ];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(
            raws(&got),
            vec![
                "class=\"c\"",
                "name=\"n\"",
                "type=\"t\"",
                "value=\"v\"",
                "href=\"/\"",
            ],
        );
    }

    /// A duplicate-name set spanning three occurrences travels together as one
    /// locked unit, anchored to its first member's group and keeping the
    /// members' relative source order (Requirement 5.5).
    #[test]
    fn duplicate_name_set_of_three_locks_and_moves_as_one_unit() {
        let scheme = AttributeOrderScheme::standard();
        let input = [
            nv("class=\"a\""), // first class -> anchors the class group (4)
            nv("id=\"i\""),    // group 3, renders before class
            nv("class=\"b\""), // locked to the set
            nv("href=\"/\""),  // group 6
            nv("class=\"c\""), // locked to the set
        ];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(
            raws(&got),
            vec![
                "id=\"i\"",
                "class=\"a\"",
                "class=\"b\"",
                "class=\"c\"",
                "href=\"/\"",
            ],
            "the three same-name attributes stay contiguous, in source order, \
             at the class group's render slot",
        );
    }

    /// Duplicate names that would otherwise be reordered by an intra-group rule
    /// still keep their source order: two `name` attributes in the explicit
    /// name/type/value group remain in their original relative order because a
    /// duplicate set is locked (Requirement 5.5 overrides 5.4 for duplicates).
    #[test]
    fn duplicate_names_are_not_reordered_by_intra_group_rule() {
        let scheme = AttributeOrderScheme::standard();
        let input = [nv("name=\"second-wins\""), nv("type=\"t\""), nv("name=\"first\"")];
        let got = sort_attributes(&input, &scheme);
        // Both `name` attributes form a locked set anchored at the first `name`;
        // `type` follows per the explicit order, but the two `name`s keep their
        // original relative order rather than being alphabetized or reordered.
        assert_eq!(
            raws(&got),
            vec!["name=\"second-wins\"", "name=\"first\"", "type=\"t\""],
        );
    }

    /// A spread attribute is a hard barrier: attributes never cross it, so a
    /// representative drawn from the full group range on each side of a single
    /// spread sorts only within its own side (Requirement 5.5).
    #[test]
    fn spread_lock_prevents_any_group_from_crossing_the_barrier() {
        let scheme = AttributeOrderScheme::standard();
        let input = [
            nv("onclick=\"f()\""), // group 9, before the spread
            nv("id=\"before\""),   // group 3, before the spread
            spread("{...props}"),
            nv("aria-x=\"a\""),   // group 8, after the spread
            nv("id=\"after\""),   // group 3, after the spread
        ];
        let got = sort_attributes(&input, &scheme);
        // Before the barrier: id (3) then onclick (9). After the barrier: id (3)
        // then aria-x (8). The `id=\"before\"` never jumps ahead of the spread
        // to join `id=\"after\"`, and vice versa.
        assert_eq!(
            raws(&got),
            vec![
                "id=\"before\"",
                "onclick=\"f()\"",
                "{...props}",
                "id=\"after\"",
                "aria-x=\"a\"",
            ],
        );
    }

    /// Unmatched attributes under a scheme whose only specific group is `id`
    /// are placed after all matched attributes and keep their source order among
    /// themselves — the trailing catch-all suffix behavior of Requirement 5.8.
    #[test]
    fn unmatched_attributes_form_a_source_ordered_suffix() {
        let scheme = AttributeOrderScheme {
            groups: vec![
                AttributeGroup {
                    name: "id",
                    matchers: vec![GroupMatcher::ExactNames(vec!["id".to_string()])],
                    intra: IntraGroupRule::SourceOrder,
                },
                AttributeGroup {
                    name: "catch-all",
                    matchers: vec![GroupMatcher::Any],
                    intra: IntraGroupRule::SourceOrder,
                },
            ],
        };
        let input = [
            nv("zeta=\"z\""),  // unmatched
            nv("id=\"i\""),    // matched -> group 0
            nv("alpha=\"a\""), // unmatched
            nv("beta=\"b\""),  // unmatched
        ];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(
            raws(&got),
            vec!["id=\"i\"", "zeta=\"z\"", "alpha=\"a\"", "beta=\"b\""],
            "matched attribute first, then unmatched attributes in source order",
        );
    }

    /// On the `standard` scheme, ordinary (unmatched-by-a-specific-group)
    /// attributes land in the `other-standard` slot (render slot 6): they trail
    /// the specific groups 1..=5, precede the specific groups 7..=9, and keep
    /// their source order among themselves (Requirement 5.8 as realized by the
    /// standard scheme's first `Any` group).
    #[test]
    fn ordinary_attributes_trail_specific_groups_in_source_order_on_standard() {
        let scheme = AttributeOrderScheme::standard();
        let input = [
            nv("data-x=\"1\""),  // group 7, specific, renders after ordinary
            nv("rel=\"next\""),  // ordinary -> slot 6
            nv("id=\"i\""),      // group 3, specific, renders before ordinary
            nv("href=\"/\""),    // ordinary -> slot 6
            nv("title=\"t\""),   // ordinary -> slot 6
        ];
        let got = sort_attributes(&input, &scheme);
        assert_eq!(
            raws(&got),
            vec![
                "id=\"i\"",
                "rel=\"next\"",
                "href=\"/\"",
                "title=\"t\"",
                "data-x=\"1\"",
            ],
            "ordinary attributes sit between the specific groups, in source order",
        );
    }
}

#[cfg(test)]
mod scheme_property_tests {
    //! Property 10: Sorting matches the scheme (model-based).
    //!
    //! **Validates: Requirements 5.1**
    //!
    //! For all start tags formatted with sorting enabled, the attribute order
    //! produced by [`sort_attributes`] (the production sorter) equals the order
    //! produced by an *independent* reference implementation of the `standard`
    //! scheme written for this test only. The reference model
    //! ([`model_sort`]) is deliberately written with a different internal
    //! structure than the production code — it resolves group membership with a
    //! hand-written match over the ten `standard` groups rather than reusing
    //! [`AttributeOrderScheme::group_index_for`], and it orders each segment with
    //! an explicit index-key stable sort — so agreement between the two is
    //! meaningful cross-validation rather than a tautology.

    use proptest::prelude::*;

    use super::{sort_attributes, AttributeOrderScheme};
    use crate::model::{display_width, Attribute, AttributeKind};

    /// An owned description of a generated attribute. `proptest` produces these;
    /// the borrowed [`Attribute`] slices used by the sorters are built from them
    /// per-case so lifetimes stay simple.
    #[derive(Clone, Debug)]
    struct GenAttr {
        kind: AttributeKind,
        /// The attribute's source name (empty for a pure spread). Used both to
        /// derive `name_key` and to build a unique `raw` identity.
        name: String,
        /// Disambiguating suffix so two attributes sharing a name still carry
        /// distinct `raw` slices and can be told apart in assertions.
        tag: u32,
    }

    impl GenAttr {
        /// The lowercased comparison name, or `None` for a pure spread.
        fn name_key(&self) -> Option<String> {
            if self.kind == AttributeKind::DynamicSpread && self.name.is_empty() {
                None
            } else {
                Some(self.name.to_ascii_lowercase())
            }
        }

        /// A unique, stable source-text identity for this attribute.
        fn raw(&self) -> String {
            match self.kind {
                AttributeKind::DynamicSpread if self.name.is_empty() => {
                    format!("{{...spread{}}}", self.tag)
                }
                AttributeKind::Plain => format!("{}#{}", self.name, self.tag),
                _ => format!("{}=\"v{}\"", self.name, self.tag),
            }
        }
    }

    /// Build a borrowed [`Attribute`] over a pre-materialized `raw` slice and an
    /// optional comparison name. The caller owns the strings so the borrow
    /// outlives the sort call.
    fn borrow<'a>(kind: AttributeKind, name_key: Option<&str>, raw: &'a str) -> Attribute<'a> {
        Attribute {
            kind,
            name_key: name_key.map(str::to_string),
            raw,
            width: display_width(raw, 2),
        }
    }

    // --- Independent reference model of the `standard` scheme ----------------

    /// Render-index of the `standard` group that claims `name_key`, written by
    /// hand (NOT via `group_index_for`) as an independent oracle. The ten
    /// groups and their membership mirror the design's `standard` scheme, with
    /// most-specific-wins precedence encoded directly by testing the specific
    /// groups first and only then falling back to the `other-standard` catch-all
    /// at render slot 5. Returns the render slot 0..=9.
    fn model_group_index(name_key: &str) -> usize {
        // 0. Definition / identity.
        if matches!(name_key, "is" | "key" | "v-bind:key" | ":key") {
            return 0;
        }
        // 1. Structural framework directives.
        if matches!(name_key, "v-if" | "v-else-if" | "v-else" | "v-for" | "v-show")
            || name_key.starts_with("client:")
        {
            return 1;
        }
        // 2. id.
        if name_key == "id" {
            return 2;
        }
        // 3. class family.
        if matches!(name_key, "class" | "classname" | ":class" | "class:") {
            return 3;
        }
        // 4. name / type / value.
        if matches!(name_key, "name" | "type" | "value") {
            return 4;
        }
        // 7. data-* (checked before the catch-all so specific wins).
        if name_key.starts_with("data-") {
            return 6;
        }
        // 8. aria-* and role.
        if name_key.starts_with("aria-") || name_key == "role" {
            return 7;
        }
        // 9. event handlers.
        if name_key.starts_with("on") || name_key.starts_with("on:") || name_key.starts_with('@') {
            return 8;
        }
        // 6. other standard (the first `Any` catch-all, render slot 5).
        5
    }

    /// The explicit intra-group order for the name/type/value group (render slot
    /// 4). Any other group uses source order in the `standard` scheme.
    fn name_type_value_rank(name_key: &str) -> usize {
        match name_key {
            "name" => 0,
            "type" => 1,
            "value" => 2,
            _ => 3,
        }
    }

    /// Independent reference sort of `attrs` under the `standard` scheme. This is
    /// the model Property 10 compares the production sorter against.
    ///
    /// Algorithm (deliberately distinct from the production implementation):
    /// 1. Spreads are barriers emitted in place; the list splits into
    ///    spread-free segments.
    /// 2. Within a segment, every attribute is tagged with the render slot and
    ///    first-occurrence index of its `name_key` so a duplicate-name set is
    ///    anchored (and thus locked) to its first member.
    /// 3. A single stable sort by `(group_of_anchor, intra_rank_of_anchor,
    ///    anchor_index)` reorders the segment; because duplicates share the same
    ///    anchor key they stay contiguous and in source order.
    fn model_sort(attrs: &[Attribute<'_>]) -> Vec<String> {
        let mut out: Vec<String> = Vec::with_capacity(attrs.len());
        let mut segment_start = 0usize;
        for (i, attr) in attrs.iter().enumerate() {
            if attr.kind == AttributeKind::DynamicSpread {
                model_sort_segment(&attrs[segment_start..i], &mut out);
                out.push(attr.raw.to_string());
                segment_start = i + 1;
            }
        }
        model_sort_segment(&attrs[segment_start..], &mut out);
        out
    }

    /// Sort one spread-free segment and append the resulting raw slices to `out`.
    fn model_sort_segment(segment: &[Attribute<'_>], out: &mut Vec<String>) {
        // First-occurrence index of each name_key within this segment.
        let mut first_index: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for (local, attr) in segment.iter().enumerate() {
            if let Some(key) = attr.name_key.as_deref() {
                first_index.entry(key).or_insert(local);
            }
        }

        // The anchor of an attribute is its own index unless it shares a name
        // with an earlier attribute, in which case it inherits that earlier
        // index — this locks duplicate-name sets together.
        let anchor_of = |local: usize| -> usize {
            segment[local]
                .name_key
                .as_deref()
                .and_then(|key| first_index.get(key).copied())
                .unwrap_or(local)
        };

        let group_of = |local: usize| -> usize {
            segment[local]
                .name_key
                .as_deref()
                .map_or(5, model_group_index)
        };
        let intra_rank_of = |local: usize| -> usize {
            let key = segment[local].name_key.as_deref().unwrap_or("");
            if group_of(local) == 4 {
                name_type_value_rank(key)
            } else {
                0
            }
        };

        let mut order: Vec<usize> = (0..segment.len()).collect();
        order.sort_by(|&a, &b| {
            let aa = anchor_of(a);
            let ba = anchor_of(b);
            // Key off each attribute's anchor so a whole locked duplicate set
            // sorts as a unit.
            (group_of(aa), intra_rank_of(aa), aa)
                .cmp(&(group_of(ba), intra_rank_of(ba), ba))
                // Within a locked set, keep original source order.
                .then(a.cmp(&b))
        });

        for local in order {
            out.push(segment[local].raw.to_string());
        }
    }

    // --- Generators ----------------------------------------------------------

    /// A pool of attribute names spanning every `standard` group plus ordinary
    /// and framework forms, including names that collide to exercise duplicate
    /// locking. The empty string denotes a pure spread (paired with
    /// `DynamicSpread`).
    fn name_strategy() -> impl Strategy<Value = (AttributeKind, String)> {
        prop_oneof![
            // Pure spread: no name.
            Just((AttributeKind::DynamicSpread, String::new())),
            // Named dynamic spread barrier (e.g. Vue v-bind="obj").
            Just((AttributeKind::DynamicSpread, "v-bind".to_string())),
            // One of a fixed pool of names covering all groups.
            prop::sample::select(vec![
                "is", "key", ":key", "v-if", "v-for", "client:load", "id", "class",
                "className", ":class", "name", "type", "value", "href", "rel", "title",
                "placeholder", "disabled", "data-x", "data-track", "aria-label",
                "aria-hidden", "role", "onclick", "onsubmit", "on:click", "@click",
            ])
            .prop_flat_map(|name| {
                // Plain for valueless-ish names, NameValue otherwise; the kind
                // does not affect sorting but widens coverage.
                prop_oneof![
                    Just((AttributeKind::NameValue, name.to_string())),
                    Just((AttributeKind::Plain, name.to_string())),
                ]
            }),
        ]
    }

    /// A list of 0..=10 generated attributes.
    fn attrs_strategy() -> impl Strategy<Value = Vec<GenAttr>> {
        prop::collection::vec(name_strategy(), 0..=10).prop_map(|names| {
            names
                .into_iter()
                .enumerate()
                .map(|(i, (kind, name))| GenAttr {
                    kind,
                    name,
                    #[allow(clippy::cast_possible_truncation)]
                    tag: i as u32,
                })
                .collect()
        })
    }

    proptest! {
        /// Property 10: the production sorter's output order equals the
        /// independent reference model's order for the `standard` scheme.
        #[test]
        fn sort_attributes_matches_reference_model(gen in attrs_strategy()) {
            let scheme = AttributeOrderScheme::standard();

            // Materialize owned strings, then borrow `Attribute`s over them.
            let raws: Vec<String> = gen.iter().map(GenAttr::raw).collect();
            let keys: Vec<Option<String>> = gen.iter().map(GenAttr::name_key).collect();
            let attrs: Vec<Attribute<'_>> = gen
                .iter()
                .enumerate()
                .map(|(i, g)| borrow(g.kind, keys[i].as_deref(), &raws[i]))
                .collect();

            let produced: Vec<String> =
                sort_attributes(&attrs, &scheme).iter().map(|a| a.raw.to_string()).collect();
            let expected = model_sort(&attrs);

            prop_assert_eq!(
                produced,
                expected,
                "production sort order must equal the reference scheme model"
            );
        }
    }
}

#[cfg(test)]
mod partition_property_tests {
    //! Property 12: the group partition is a stable, ordered partition with a
    //! trailing unmatched group (Validates: Requirements 5.4, 5.8).
    //!
    //! These tests generate random attribute lists and assert three facets of
    //! the partition produced by [`sort_attributes`]:
    //!
    //! 1. **Ordered partition** — attributes come out grouped by their scheme
    //!    group's render index, in non-decreasing render-index order, so the
    //!    groups appear in the scheme's declared order.
    //! 2. **Stability within source-order groups** — within a group whose
    //!    intra-group rule is [`IntraGroupRule::SourceOrder`], members keep their
    //!    original relative source order (Requirement 5.4).
    //! 3. **Trailing unmatched group** — attributes that match no *specific*
    //!    group land in the catch-all bucket after every matched attribute,
    //!    preserving their source order (Requirement 5.8).
    //!
    //! To keep the partition clean and its reasoning unambiguous, the generators
    //! avoid pure spreads (which act as barriers) and duplicate names (which
    //! form locked units). Those behavior-preserving guards are the subject of a
    //! separate property (Property 13); here we isolate the partition itself.

    use super::*;
    use crate::model::display_width;
    use proptest::prelude::*;

    /// A fixed vocabulary of distinct attribute names spanning several of the
    /// `standard` scheme's render groups, so a random sample of them exercises
    /// multi-group partitioning. Every name here is distinct and lowercased, so
    /// a sample of distinct names never introduces a duplicate-name locked unit.
    const NAMES: &[&str] = &[
        // definition-identity (group 0)
        "is",
        "key",
        // structural-directives (group 1)
        "v-if",
        "v-for",
        "client:load",
        // id (group 2)
        "id",
        // class (group 3)
        "class",
        "classname",
        // name-type-value (group 4) — ExactNameOrder, excluded from the
        // source-order generator below but used by the ordered-partition test.
        "name",
        "type",
        "value",
        // other-standard (group 5, the first Any group)
        "href",
        "rel",
        "title",
        "placeholder",
        "disabled",
        // data-* (group 6)
        "data-track",
        "data-id",
        // aria-* and role (group 7)
        "aria-label",
        "role",
        // event handlers (group 8)
        "onclick",
        "on:submit",
        "@input",
    ];

    /// Build a name-value attribute from a bare name. `name_key` mirrors the
    /// scanner's lowercased comparison name; `raw` doubles as the attribute's
    /// identity in assertions (every generated name is distinct).
    fn attr(name: &'static str) -> Attribute<'static> {
        Attribute {
            kind: AttributeKind::NameValue,
            name_key: Some(name.to_ascii_lowercase()),
            raw: name,
            width: display_width(name, 2),
        }
    }

    /// A strategy yielding a list of attributes built from a set of DISTINCT
    /// names drawn from `NAMES` in a random order. Distinctness keeps the
    /// partition clean: no duplicate-name locked units and no pure-spread
    /// barriers, so the output is a straight group partition of the input.
    fn distinct_attr_lists() -> impl Strategy<Value = Vec<Attribute<'static>>> {
        // A shuffled subset of indices into NAMES. `subsequence` preserves no
        // particular order, so we additionally shuffle to randomize source
        // order and thereby stress the stability guarantee.
        prop::sample::subsequence((0..NAMES.len()).collect::<Vec<_>>(), 0..=NAMES.len())
            .prop_shuffle()
            .prop_map(|idxs| idxs.into_iter().map(|i| attr(NAMES[i])).collect())
    }

    /// The render-group index of an attribute under the given scheme, using the
    /// scheme's own most-specific-wins assignment (the single source of truth).
    fn group_index(scheme: &AttributeOrderScheme, a: &Attribute<'_>) -> usize {
        a.name_key
            .as_deref()
            .and_then(|k| scheme.group_index_for(k))
            .expect("standard scheme ends in an Any catch-all, so every name matches")
    }

    proptest! {
        /// Ordered partition: reading the sorted output left to right, each
        /// attribute's group render index is >= the previous attribute's — i.e.
        /// attributes are emitted grouped and the groups appear in the scheme's
        /// declared render order (Requirement 5.4 ordering facet).
        ///
        /// **Validates: Requirements 5.4, 5.8**
        #[test]
        fn groups_appear_in_non_decreasing_render_order(attrs in distinct_attr_lists()) {
            let scheme = AttributeOrderScheme::standard();
            let sorted = sort_attributes(&attrs, &scheme);

            // The partition preserves the attribute set (a precondition for the
            // partition claim to be meaningful).
            prop_assert_eq!(sorted.len(), attrs.len());

            let mut prev: Option<usize> = None;
            for a in &sorted {
                let g = group_index(&scheme, a);
                if let Some(p) = prev {
                    prop_assert!(
                        g >= p,
                        "group render indices must be non-decreasing across the \
                         output (saw {} after {})",
                        g,
                        p
                    );
                }
                prev = Some(g);
            }
        }

        /// Contiguity: all attributes sharing a group render index form a single
        /// contiguous run in the output. Together with the non-decreasing
        /// property above this is exactly the "ordered partition" claim — no
        /// group is split by an attribute from another group.
        ///
        /// **Validates: Requirements 5.4, 5.8**
        #[test]
        fn each_group_is_a_contiguous_run(attrs in distinct_attr_lists()) {
            let scheme = AttributeOrderScheme::standard();
            let sorted = sort_attributes(&attrs, &scheme);

            // The set of render indices seen so far that are already "closed"
            // (a different index intervened after them). Encountering a closed
            // index again would mean the group was split.
            let mut seen_then_left: std::collections::HashSet<usize> = std::collections::HashSet::new();
            let mut current: Option<usize> = None;
            for a in &sorted {
                let g = group_index(&scheme, a);
                match current {
                    Some(c) if c == g => {}
                    Some(c) => {
                        seen_then_left.insert(c);
                        prop_assert!(
                            !seen_then_left.contains(&g),
                            "group {} reappears after being left — not contiguous",
                            g
                        );
                        current = Some(g);
                    }
                    None => current = Some(g),
                }
            }
        }

        /// Stability within source-order groups: for every pair of attributes
        /// that land in the SAME source-order group, their relative order in the
        /// output matches their relative order in the input (Requirement 5.4,
        /// SourceOrder default). The `name-type-value` group is excluded because
        /// its intra-group rule is `ExactNameOrder`, not source order; the
        /// generator here draws only from source-order groups.
        ///
        /// **Validates: Requirements 5.4**
        #[test]
        fn source_order_groups_preserve_relative_order(
            attrs in source_order_attr_lists()
        ) {
            let scheme = AttributeOrderScheme::standard();
            let sorted = sort_attributes(&attrs, &scheme);

            // Source position of each attribute by its raw identity (names are
            // distinct in this generator).
            let src_pos = |raw: &str| attrs.iter().position(|a| a.raw == raw).unwrap();

            // For every adjacent-in-output pair that shares a group, the earlier
            // one in the output must be the earlier one in the input. Checking
            // same-group pairs across the whole output (not just adjacent) is
            // equivalent given contiguity; we check all same-group pairs to be
            // thorough.
            for i in 0..sorted.len() {
                for j in (i + 1)..sorted.len() {
                    let gi = group_index(&scheme, &sorted[i]);
                    let gj = group_index(&scheme, &sorted[j]);
                    if gi == gj {
                        prop_assert!(
                            src_pos(sorted[i].raw) < src_pos(sorted[j].raw),
                            "within group {}, output order must follow source order \
                             ({:?} before {:?})",
                            gi,
                            sorted[i].raw,
                            sorted[j].raw
                        );
                    }
                }
            }
        }

        /// Trailing unmatched group (Requirement 5.8): under a scheme whose only
        /// specific group is `id`, every attribute other than `id` is unmatched
        /// and must appear AFTER the matched `id` attributes, preserving its
        /// source order among the other unmatched attributes.
        ///
        /// **Validates: Requirements 5.8**
        #[test]
        fn unmatched_attributes_trail_in_source_order(attrs in distinct_attr_lists()) {
            // A minimal scheme: `id` is the single specific group, everything
            // else falls into the trailing Any catch-all in source order.
            let scheme = AttributeOrderScheme {
                groups: vec![
                    AttributeGroup {
                        name: "id",
                        matchers: vec![GroupMatcher::ExactNames(vec!["id".to_string()])],
                        intra: IntraGroupRule::SourceOrder,
                    },
                    AttributeGroup {
                        name: "catch-all",
                        matchers: vec![GroupMatcher::Any],
                        intra: IntraGroupRule::SourceOrder,
                    },
                ],
            };
            let sorted = sort_attributes(&attrs, &scheme);

            // Partition the OUTPUT into matched (id) and unmatched buckets, in
            // output order.
            let is_id = |a: &Attribute<'_>| a.name_key.as_deref() == Some("id");
            let first_unmatched = sorted.iter().position(|a| !is_id(a));

            // Every matched (id) attribute precedes every unmatched one: no id
            // appears after the first unmatched attribute.
            if let Some(split) = first_unmatched {
                for a in &sorted[split..] {
                    prop_assert!(
                        !is_id(a),
                        "a matched `id` attribute appears after an unmatched one; \
                         unmatched attributes must trail all matched ones (Req 5.8)"
                    );
                }
            }

            // The unmatched attributes keep their source order among themselves.
            let unmatched_src: Vec<&str> = attrs
                .iter()
                .filter(|a| !is_id(a))
                .map(|a| a.raw)
                .collect();
            let unmatched_out: Vec<&str> = sorted
                .iter()
                .filter(|a| !is_id(a))
                .map(|a| a.raw)
                .collect();
            prop_assert_eq!(
                unmatched_out,
                unmatched_src,
                "unmatched attributes must preserve their source order (Req 5.8)"
            );
        }
    }

    /// Names restricted to groups whose intra-group rule is
    /// [`IntraGroupRule::SourceOrder`], excluding the `name-type-value`
    /// (`ExactNameOrder`) group, so the stability-within-group property can
    /// assert pure source-order preservation.
    const SOURCE_ORDER_NAMES: &[&str] = &[
        "is", "key", // group 0
        "v-if", "v-for", "client:load", // group 1
        "id",      // group 2
        "class", "classname", // group 3
        "href", "rel", "title", "placeholder", "disabled", // group 5
        "data-track", "data-id", // group 6
        "aria-label", "role", // group 7
        "onclick", "on:submit", "@input", // group 8
    ];

    /// A strategy yielding a shuffled list of DISTINCT attributes drawn only
    /// from [`SOURCE_ORDER_NAMES`].
    fn source_order_attr_lists() -> impl Strategy<Value = Vec<Attribute<'static>>> {
        prop::sample::subsequence(
            (0..SOURCE_ORDER_NAMES.len()).collect::<Vec<_>>(),
            0..=SOURCE_ORDER_NAMES.len(),
        )
        .prop_shuffle()
        .prop_map(|idxs| idxs.into_iter().map(|i| attr(SOURCE_ORDER_NAMES[i])).collect())
    }
}

#[cfg(test)]
mod idempotence_property_tests {
    //! Property 15: Sorting is order-idempotent (Validates: Requirements 5.9).
    //!
    //! **Validates: Requirements 5.9**
    //!
    //! For all start tags, sorting an already-sorted attribute list under the
    //! same configuration yields the same order — i.e. [`sort_attributes`] is a
    //! fixed point on its own output. Concretely, for every generated list
    //! `attrs` and the `standard` scheme:
    //!
    //! * **Order-idempotence** — `sort(sort(attrs)) == sort(attrs)`, compared as
    //!   full [`Attribute`] equality (`kind`, `name_key`, the verbatim `raw`
    //!   byte slice, and `width`), so the second pass reproduces the first
    //!   byte-for-byte on every attribute.
    //! * **Determinism** — calling `sort(attrs)` twice yields identical results,
    //!   so the function depends only on its inputs (Requirement 5.9).
    //!
    //! The generators deliberately mix attributes from every `standard` group
    //! with duplicate names (which form locked units) and pure/named spreads
    //! (which act as barriers) so idempotence is exercised across the
    //! partition, the duplicate-locking guard, and the spread-barrier guard
    //! together.

    use proptest::prelude::*;

    use super::{sort_attributes, AttributeOrderScheme};
    use crate::model::{display_width, Attribute, AttributeKind};

    /// An owned description of a generated attribute. `proptest` produces these;
    /// borrowed [`Attribute`] slices are built from them per-case so lifetimes
    /// stay simple.
    #[derive(Clone, Debug)]
    struct GenAttr {
        kind: AttributeKind,
        /// Source name (empty for a pure spread). Drives both `name_key` and the
        /// `raw` identity. Names are drawn from a pool that includes deliberate
        /// collisions so duplicate-name locking is exercised.
        name: String,
        /// Disambiguating suffix so two attributes sharing a name still carry
        /// distinct `raw` slices.
        tag: u32,
    }

    impl GenAttr {
        /// The lowercased comparison name, or `None` for a pure spread.
        fn name_key(&self) -> Option<String> {
            if self.kind == AttributeKind::DynamicSpread && self.name.is_empty() {
                None
            } else {
                Some(self.name.to_ascii_lowercase())
            }
        }

        /// A unique, stable source-text identity for this attribute.
        fn raw(&self) -> String {
            match self.kind {
                AttributeKind::DynamicSpread if self.name.is_empty() => {
                    format!("{{...spread{}}}", self.tag)
                }
                AttributeKind::Plain => format!("{}#{}", self.name, self.tag),
                _ => format!("{}=\"v{}\"", self.name, self.tag),
            }
        }
    }

    /// Build a borrowed [`Attribute`] over a pre-materialized `raw` slice and an
    /// optional comparison name. The caller owns the strings so the borrow
    /// outlives the sort call.
    fn borrow<'a>(kind: AttributeKind, name_key: Option<&str>, raw: &'a str) -> Attribute<'a> {
        Attribute {
            kind,
            name_key: name_key.map(str::to_string),
            raw,
            width: display_width(raw, 2),
        }
    }

    /// A pool of attribute names spanning every `standard` group plus ordinary
    /// and framework forms, paired with a kind. The empty string denotes a pure
    /// spread (paired with `DynamicSpread`); `v-bind` is a named spread barrier.
    /// The pool intentionally contains names that collide so a random list of
    /// 0..=12 attributes frequently includes duplicate names.
    fn name_strategy() -> impl Strategy<Value = (AttributeKind, String)> {
        prop_oneof![
            // Pure spread barrier: no name.
            Just((AttributeKind::DynamicSpread, String::new())),
            // Named dynamic spread barrier (e.g. Vue v-bind="obj").
            Just((AttributeKind::DynamicSpread, "v-bind".to_string())),
            // One of a fixed pool of names covering all groups.
            prop::sample::select(vec![
                "is", "key", ":key", "v-if", "v-for", "client:load", "id", "class",
                "className", ":class", "name", "type", "value", "href", "rel", "title",
                "placeholder", "disabled", "data-x", "data-track", "aria-label",
                "aria-hidden", "role", "onclick", "onsubmit", "on:click", "@click",
            ])
            .prop_flat_map(|name| {
                prop_oneof![
                    Just((AttributeKind::NameValue, name.to_string())),
                    Just((AttributeKind::Plain, name.to_string())),
                ]
            }),
        ]
    }

    /// A list of 0..=12 generated attributes, with `tag` suffixes assigned by
    /// position so every attribute carries a distinct `raw` identity even when
    /// names collide.
    fn attrs_strategy() -> impl Strategy<Value = Vec<GenAttr>> {
        prop::collection::vec(name_strategy(), 0..=12).prop_map(|names| {
            names
                .into_iter()
                .enumerate()
                .map(|(i, (kind, name))| GenAttr {
                    kind,
                    name,
                    #[allow(clippy::cast_possible_truncation)]
                    tag: i as u32,
                })
                .collect()
        })
    }

    /// Materialize owned `raw`/`name_key` strings for a generated list and
    /// borrow an [`Attribute`] over each. Returns the owned backing strings
    /// alongside so the borrows stay valid for the caller's scope.
    fn materialize(gen: &[GenAttr]) -> (Vec<String>, Vec<Option<String>>) {
        let raws: Vec<String> = gen.iter().map(GenAttr::raw).collect();
        let keys: Vec<Option<String>> = gen.iter().map(GenAttr::name_key).collect();
        (raws, keys)
    }

    proptest! {
        /// Property 15: sorting an already-sorted list reproduces it exactly.
        /// Full [`Attribute`] equality (including the verbatim `raw` byte slice)
        /// means the second pass is byte-for-byte identical to the first.
        ///
        /// **Validates: Requirements 5.9**
        #[test]
        fn sorting_is_order_idempotent(gen in attrs_strategy()) {
            let scheme = AttributeOrderScheme::standard();
            let (raws, keys) = materialize(&gen);
            let attrs: Vec<Attribute<'_>> = gen
                .iter()
                .enumerate()
                .map(|(i, g)| borrow(g.kind, keys[i].as_deref(), &raws[i]))
                .collect();

            let once = sort_attributes(&attrs, &scheme);
            let twice = sort_attributes(&once, &scheme);

            prop_assert_eq!(
                &twice,
                &once,
                "sorting an already-sorted list must reproduce it byte-for-byte \
                 (order-idempotence, Requirement 5.9)"
            );
        }

        /// Determinism: repeated calls on the same input yield identical output,
        /// so the sort depends only on its inputs (Requirement 5.9).
        ///
        /// **Validates: Requirements 5.9**
        #[test]
        fn sorting_is_deterministic(gen in attrs_strategy()) {
            let scheme = AttributeOrderScheme::standard();
            let (raws, keys) = materialize(&gen);
            let attrs: Vec<Attribute<'_>> = gen
                .iter()
                .enumerate()
                .map(|(i, g)| borrow(g.kind, keys[i].as_deref(), &raws[i]))
                .collect();

            let first = sort_attributes(&attrs, &scheme);
            let second = sort_attributes(&attrs, &scheme);

            prop_assert_eq!(
                &first,
                &second,
                "repeated sort calls on identical input must agree (determinism, \
                 Requirement 5.9)"
            );
        }
    }
}

#[cfg(test)]
mod behavior_preservation_property_tests {
    //! Property 13: Behavior-affecting order is preserved under sorting
    //! (Validates: Requirements 5.5).
    //!
    //! For all start tags, [`sort_attributes`] preserves the original relative
    //! source order of:
    //!
    //! - every set of attributes that share the same comparison name
    //!   (duplicate attributes), and
    //! - every dynamic/spread attribute relative to all other attributes,
    //!
    //! so that no reordering can change which attribute value takes effect.
    //!
    //! The generators here *deliberately* bias toward the behavior-affecting
    //! cases: they draw from a small name pool so duplicate names are common,
    //! and they inject multiple pure spreads so the spread-barrier guard is
    //! exercised on nearly every case. Three facets are asserted per case:
    //!
    //! (a) the relative order of every duplicate-name set is unchanged vs the
    //!     source;
    //! (b) the relative order of all `DynamicSpread` attributes is unchanged vs
    //!     the source; and
    //! (c) more generally, for every duplicate-name set the projection of the
    //!     sorted output onto that set *together with all spreads* equals the
    //!     source projection, so a spread can never cross — and thus never
    //!     change which value overrides which — any attribute whose value it
    //!     might affect.
    //!
    //! Facet (c) is the combined spread/duplicate guarantee; facets (a) and (b)
    //! are checked separately so a regression points at the narrowest broken
    //! guarantee. Note that Requirement 5.5 constrains the relative order
    //! *within* each duplicate-name set and *of spreads against everything* —
    //! it does NOT fix the relative order between two distinct duplicate-name
    //! sets, which grouping is free to reorder, so (c) deliberately projects one
    //! duplicate set at a time rather than all locked attributes at once.

    use proptest::prelude::*;

    use super::{sort_attributes, AttributeOrderScheme};
    use crate::model::{display_width, Attribute, AttributeKind};

    /// An owned description of a generated attribute. `proptest` produces these;
    /// borrowed [`Attribute`] slices are built from them per-case so lifetimes
    /// stay simple. Each attribute carries a unique numeric `tag` so two
    /// attributes that share a name still have distinct `raw` identities and can
    /// be told apart in assertions.
    #[derive(Clone, Debug)]
    struct GenAttr {
        kind: AttributeKind,
        /// The source name (empty only for a pure spread).
        name: String,
        /// Disambiguating suffix giving every attribute a unique `raw`.
        tag: u32,
    }

    impl GenAttr {
        /// The lowercased comparison name, or `None` for a pure spread.
        fn name_key(&self) -> Option<String> {
            if self.kind == AttributeKind::DynamicSpread && self.name.is_empty() {
                None
            } else {
                Some(self.name.to_ascii_lowercase())
            }
        }

        /// A unique, stable source-text identity for this attribute.
        fn raw(&self) -> String {
            match self.kind {
                AttributeKind::DynamicSpread if self.name.is_empty() => {
                    format!("{{...spread{}}}", self.tag)
                }
                AttributeKind::Plain => format!("{}#{}", self.name, self.tag),
                _ => format!("{}=\"v{}\"", self.name, self.tag),
            }
        }
    }

    /// Build a borrowed [`Attribute`] over a pre-materialized `raw` slice and an
    /// optional comparison name.
    fn borrow<'a>(kind: AttributeKind, name_key: Option<&str>, raw: &'a str) -> Attribute<'a> {
        Attribute {
            kind,
            name_key: name_key.map(str::to_string),
            raw,
            width: display_width(raw, 2),
        }
    }

    /// A deliberately SMALL pool of names so random lists collide often and
    /// duplicate-name locked sets are the common case rather than a rare one.
    /// The names span several `standard` groups so sorting actually wants to
    /// move things around (making the preservation guarantee non-trivial).
    const NAMES: &[&str] = &["id", "class", "name", "type", "href", "data-x", "aria-label"];

    /// One generated element: a pure spread, a named dynamic spread barrier, or
    /// an ordinary name-value / plain attribute drawn from the small pool.
    /// Spreads are weighted up so multiple spreads appear frequently.
    fn element_strategy() -> impl Strategy<Value = (AttributeKind, String)> {
        prop_oneof![
            // Pure spreads (weighted so several usually appear per list).
            3 => Just((AttributeKind::DynamicSpread, String::new())),
            // Named dynamic spread barrier (e.g. Vue v-bind="obj"): still a
            // DynamicSpread, so still a barrier, but carries a name_key.
            1 => Just((AttributeKind::DynamicSpread, "v-bind".to_string())),
            // Ordinary attributes from the small, collision-prone pool.
            6 => prop::sample::select(NAMES.to_vec()).prop_flat_map(|name| {
                prop_oneof![
                    Just((AttributeKind::NameValue, name.to_string())),
                    Just((AttributeKind::Plain, name.to_string())),
                ]
            }),
        ]
    }

    /// A list of 0..=14 generated attributes with frequent name collisions and
    /// multiple spreads.
    fn attrs_strategy() -> impl Strategy<Value = Vec<GenAttr>> {
        prop::collection::vec(element_strategy(), 0..=14).prop_map(|elems| {
            elems
                .into_iter()
                .enumerate()
                .map(|(i, (kind, name))| GenAttr {
                    kind,
                    name,
                    #[allow(clippy::cast_possible_truncation)]
                    tag: i as u32,
                })
                .collect()
        })
    }

    /// Materialize owned `raw`/`name_key` strings and borrow `Attribute`s over
    /// them, returning both so the borrows outlive the sort call.
    fn materialize(gen: &[GenAttr]) -> (Vec<String>, Vec<Option<String>>) {
        let raws = gen.iter().map(GenAttr::raw).collect();
        let keys = gen.iter().map(GenAttr::name_key).collect();
        (raws, keys)
    }

    /// The source-order subsequence of `raw` identities for every attribute
    /// matching `keep`.
    fn projection<'a>(attrs: &[Attribute<'a>], keep: impl Fn(&Attribute<'a>) -> bool) -> Vec<&'a str> {
        attrs.iter().filter(|a| keep(a)).map(|a| a.raw).collect()
    }

    proptest! {
        /// (a) Duplicate-name sets keep their relative source order: for every
        /// comparison name that occurs two or more times, the subsequence of
        /// those attributes read from the sorted output is identical to the
        /// subsequence read from the source.
        ///
        /// **Validates: Requirements 5.5**
        #[test]
        fn duplicate_name_sets_keep_relative_order(gen in attrs_strategy()) {
            let scheme = AttributeOrderScheme::standard();
            let (raws, keys) = materialize(&gen);
            let attrs: Vec<Attribute<'_>> = gen
                .iter()
                .enumerate()
                .map(|(i, g)| borrow(g.kind, keys[i].as_deref(), &raws[i]))
                .collect();

            let sorted = sort_attributes(&attrs, &scheme);
            prop_assert_eq!(sorted.len(), attrs.len());

            // Comparison names that occur 2+ times in the source.
            let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
            for a in &attrs {
                if let Some(k) = a.name_key.as_deref() {
                    *counts.entry(k).or_insert(0) += 1;
                }
            }

            for (&name, &n) in &counts {
                if n < 2 {
                    continue;
                }
                let has_name = |a: &Attribute<'_>| a.name_key.as_deref() == Some(name);
                let src = projection(&attrs, has_name);
                let out = projection(&sorted, has_name);
                prop_assert_eq!(
                    out,
                    src,
                    "duplicate-name set {:?} must keep its relative source order",
                    name
                );
            }
        }

        /// (b) All `DynamicSpread` attributes keep their relative order (and in
        /// fact their absolute positions, as barriers): the subsequence of
        /// spreads read from the sorted output equals the source subsequence.
        ///
        /// **Validates: Requirements 5.5**
        #[test]
        fn spreads_keep_relative_order(gen in attrs_strategy()) {
            let scheme = AttributeOrderScheme::standard();
            let (raws, keys) = materialize(&gen);
            let attrs: Vec<Attribute<'_>> = gen
                .iter()
                .enumerate()
                .map(|(i, g)| borrow(g.kind, keys[i].as_deref(), &raws[i]))
                .collect();

            let sorted = sort_attributes(&attrs, &scheme);

            let is_spread = |a: &Attribute<'_>| a.kind == AttributeKind::DynamicSpread;
            let src = projection(&attrs, is_spread);
            let out = projection(&sorted, is_spread);
            prop_assert_eq!(
                out,
                src,
                "spread/dynamic attributes must keep their relative source order"
            );
        }

        /// (c) The combined guard: for every duplicate-name set, the projection
        /// of the sorted output onto that set *together with all spreads* equals
        /// the source projection. This is exactly the behavior-affecting
        /// guarantee of Requirement 5.5 — a duplicate set keeps its internal
        /// order (facet a) AND no spread crosses any attribute of that set, so a
        /// spread can never change which value it overrides or is overridden by.
        ///
        /// Distinct duplicate sets are projected independently: grouping is free
        /// to reorder one set relative to another, which does not change runtime
        /// behavior, so a single all-at-once projection would be too strong.
        ///
        /// **Validates: Requirements 5.5**
        #[test]
        fn spread_and_duplicate_projection_is_preserved(gen in attrs_strategy()) {
            let scheme = AttributeOrderScheme::standard();
            let (raws, keys) = materialize(&gen);
            let attrs: Vec<Attribute<'_>> = gen
                .iter()
                .enumerate()
                .map(|(i, g)| borrow(g.kind, keys[i].as_deref(), &raws[i]))
                .collect();

            let sorted = sort_attributes(&attrs, &scheme);

            // A name is behavior-affecting when it occurs more than once.
            let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
            for a in &attrs {
                if let Some(k) = a.name_key.as_deref() {
                    *counts.entry(k).or_insert(0) += 1;
                }
            }
            let duplicate_names: Vec<&str> = counts
                .iter()
                .filter(|&(_, &n)| n >= 2)
                .map(|(&k, _)| k)
                .collect();

            let is_spread = |a: &Attribute<'_>| a.kind == AttributeKind::DynamicSpread;

            // For each duplicate-name set independently, that set's members plus
            // every spread form a subsequence whose order is preserved.
            for &name in &duplicate_names {
                let keep = |a: &Attribute<'_>| is_spread(a) || a.name_key.as_deref() == Some(name);
                let src = projection(&attrs, keep);
                let out = projection(&sorted, keep);
                prop_assert_eq!(
                    out,
                    src,
                    "duplicate set {:?} together with all spreads must keep its \
                     relative source order (Requirement 5.5)",
                    name
                );
            }
        }
    }
}
