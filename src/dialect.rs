//! Markup dialect resolution and per-dialect scanner profiles.
//!
//! [`Dialect`] enumerates the supported markup languages. [`Dialect::from_extension`]
//! resolves a file extension (case-insensitively) to a dialect, honoring the
//! optional-dialect enablement flags in the resolved [`Configuration`]
//! (Requirements 3.1–3.5). [`Dialect::scanner_profile`] returns the per-dialect
//! [`ScannerProfile`] the start-tag scanner (task 5) consumes: the tag-name start
//! rules, opaque-region definitions, and attribute-form rules that make the
//! scanner dialect-parameterized without baking dialect knowledge into the
//! scanner state machine itself.

use crate::config::Configuration;

/// A supported markup dialect.
///
/// HTML, JSX, and TSX are always supported; Vue, Astro, and Svelte are only
/// resolvable when enabled in the configuration (Requirements 3.1–3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dialect {
    /// HTML (`.html`).
    Html,
    /// JSX (`.jsx`).
    Jsx,
    /// TSX (`.tsx`).
    Tsx,
    /// Vue single-file component templates (`.vue`), gated on `config.vue`.
    Vue,
    /// Astro components (`.astro`), gated on `config.astro`.
    Astro,
    /// Svelte components (`.svelte`), gated on `config.svelte`.
    Svelte,
}

impl Dialect {
    /// Resolve a file extension to a [`Dialect`], matching case-insensitively and
    /// honoring the optional-dialect enablement flags.
    ///
    /// HTML/JSX/TSX always resolve (Requirements 3.1, 3.2). Vue/Astro/Svelte
    /// resolve only when their flag is enabled in `config`; when disabled they
    /// yield `None` so the file is preserved byte-for-byte (Requirements 3.3,
    /// 3.4). Any other extension yields `None` (Requirement 3.5).
    ///
    /// The extension is the bare extension without a leading dot (for example
    /// `"html"`, not `".html"`).
    #[must_use]
    pub fn from_extension(ext: &str, config: &Configuration) -> Option<Self> {
        // Match case-insensitively without allocating for the common ASCII case.
        let lowered = ext.to_ascii_lowercase();
        match lowered.as_str() {
            "html" => Some(Self::Html),
            "jsx" => Some(Self::Jsx),
            "tsx" => Some(Self::Tsx),
            "vue" if config.vue => Some(Self::Vue),
            "astro" if config.astro => Some(Self::Astro),
            "svelte" if config.svelte => Some(Self::Svelte),
            _ => None,
        }
    }

    /// The scanner profile for this dialect: tag-name start rules, opaque-region
    /// definitions, and attribute-form rules that parameterize the start-tag
    /// scanner (task 5).
    #[must_use]
    pub fn scanner_profile(self) -> ScannerProfile {
        match self {
            Self::Html => ScannerProfile::html(),
            Self::Jsx | Self::Tsx => ScannerProfile::jsx(),
            Self::Vue => ScannerProfile::vue(),
            Self::Astro => ScannerProfile::astro(),
            Self::Svelte => ScannerProfile::svelte(),
        }
    }
}

/// Rules governing which character may begin an element name after a `<`.
///
/// The start-tag scanner only enters tag-scanning when a `<` is followed by a
/// character these rules admit; this keeps `<` used as a less-than operator (in
/// JSX/TSX expressions) or ordinary text from being mistaken for a tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TagNameStart {
    /// Whether an ASCII letter (`A`–`Z`, `a`–`z`) may begin a tag name. True for
    /// every dialect.
    pub ascii_letter: bool,
    /// Whether `_` and `$` may begin a tag name. True for the JS-family dialects
    /// (JSX/TSX and the framework dialects that embed component identifiers).
    pub underscore_dollar: bool,
    /// Whether a tag name may use member/namespace forms with `.` or `:` after
    /// the initial name character (for example `<Foo.Bar>` or `<svg:rect>` in
    /// JSX). The leading character itself still obeys the rules above.
    pub member_namespace: bool,
}

impl TagNameStart {
    /// Whether `ch` is a valid first character of a tag name under these rules.
    #[must_use]
    pub fn accepts_first_char(self, ch: char) -> bool {
        (self.ascii_letter && ch.is_ascii_alphabetic())
            || (self.underscore_dollar && (ch == '_' || ch == '$'))
    }
}

/// A verbatim region the scanner must copy without scanning it for tags.
///
/// Each variant names a class of opaque region recognized by the shared scanner
/// state machine (task 5). The scanner enters the region on its opener and exits
/// on its closer, emitting everything in between byte-for-byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpaqueRegion {
    /// Markup comment: `<!-- ... -->`.
    Comment,
    /// CDATA section: `<![CDATA[ ... ]]>`.
    CData,
    /// Doctype or other `<! ... >` declaration.
    Declaration,
    /// Processing instruction: `<? ... ?>`.
    ProcessingInstruction,
    /// A raw-text / escapable-raw-text element body whose contents are never
    /// markup — `<script>`, `<style>`, `<textarea>`, `<title>`. Once such a start
    /// tag is consumed, everything up to the matching end tag is opaque.
    RawTextElement,
    /// Astro frontmatter fenced by `---` at the top of the file.
    Frontmatter,
}

/// The forms an attribute may take within a start tag for a dialect.
///
/// These flags tell the attribute tokenizer (task 5.3) which value syntaxes to
/// recognize. Plain and quoted name-value forms are universal; braced
/// expression values and spread forms are dialect-specific.
///
/// Each field is an independent capability toggle for the tokenizer rather than
/// internal state that a state machine or enum would model more clearly: a
/// dialect can enable any combination of these forms (for example Vue enables
/// unquoted, braced, and spread forms together while HTML enables only unquoted).
/// Collapsing them into a two-variant enum would lose that independence, so
/// `clippy::struct_excessive_bools` is deliberately allowed here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttributeForms {
    /// Plain boolean attributes with no value, e.g. `disabled`. Always true.
    pub plain: bool,
    /// Quoted name-value attributes, e.g. `class="a"` or `id='b'`. Always true.
    pub quoted_value: bool,
    /// Unquoted name-value attributes, e.g. `id=main`. HTML only; the JS-family
    /// dialects require quotes or braces.
    pub unquoted_value: bool,
    /// Braced expression values, e.g. `class={x}` (JSX/TSX), `on:click={...}`
    /// (Svelte), or `client:load`. Enabled for the JS-family and framework
    /// dialects.
    pub braced_value: bool,
    /// Spread / dynamic attributes captured verbatim, e.g. `{...props}` (JSX),
    /// `v-bind="obj"` (Vue). Enabled for the JS-family and framework dialects.
    pub spread: bool,
}

/// Everything the start-tag scanner needs to process one dialect: how tag names
/// begin, which regions are opaque, and which attribute forms to recognize.
///
/// Profiles are small, `const`-constructible value objects produced by
/// [`Dialect::scanner_profile`]. The scanner is written once against this
/// profile rather than branching on the dialect, which keeps dialect knowledge
/// declarative and localized here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannerProfile {
    /// The dialect this profile describes.
    pub dialect: Dialect,
    /// Rules for which characters begin a tag name after `<`.
    pub tag_name_start: TagNameStart,
    /// The opaque regions this dialect recognizes.
    pub opaque_regions: &'static [OpaqueRegion],
    /// The attribute forms the tokenizer should recognize.
    pub attribute_forms: AttributeForms,
    /// Whether child-position `{ ... }` expression containers are skipped as
    /// opaque during the main byte walk. True only for the JSX/TSX profile,
    /// where child containers hold JavaScript/TypeScript expression text (such
    /// as generics and `<` operators) that must never be scanned for tags.
    pub scan_child_expression_containers: bool,
}

/// Opaque regions common to the markup (HTML-like) dialects.
const MARKUP_OPAQUE: &[OpaqueRegion] = &[
    OpaqueRegion::Comment,
    OpaqueRegion::CData,
    OpaqueRegion::Declaration,
    OpaqueRegion::ProcessingInstruction,
    OpaqueRegion::RawTextElement,
];

/// Opaque regions for the JSX/TSX dialects. JSX comments inside markup are
/// expression containers (`{/* ... */}`) handled by the attribute/child
/// tokenizer rather than a markup comment region; CDATA and processing
/// instructions do not occur, so only declaration-like skipping is retained
/// alongside raw-text handling for embedded `<script>`/`<style>` (rare but
/// possible in mixed files).
const JSX_OPAQUE: &[OpaqueRegion] = &[OpaqueRegion::RawTextElement];

/// Opaque regions for Astro: markup regions plus top-of-file frontmatter.
const ASTRO_OPAQUE: &[OpaqueRegion] = &[
    OpaqueRegion::Comment,
    OpaqueRegion::CData,
    OpaqueRegion::Declaration,
    OpaqueRegion::ProcessingInstruction,
    OpaqueRegion::RawTextElement,
    OpaqueRegion::Frontmatter,
];

impl ScannerProfile {
    /// HTML profile: ASCII tag names, markup opaque regions, HTML attribute forms
    /// (including unquoted values), no braced/spread forms.
    #[must_use]
    pub const fn html() -> Self {
        Self {
            dialect: Dialect::Html,
            tag_name_start: TagNameStart {
                ascii_letter: true,
                underscore_dollar: false,
                member_namespace: false,
            },
            opaque_regions: MARKUP_OPAQUE,
            attribute_forms: AttributeForms {
                plain: true,
                quoted_value: true,
                unquoted_value: true,
                braced_value: false,
                spread: false,
            },
            scan_child_expression_containers: false,
        }
    }

    /// JSX/TSX profile: letter/`_`/`$` tag names with member/namespace forms,
    /// minimal opaque regions, braced and spread attribute forms, no unquoted
    /// values.
    #[must_use]
    pub const fn jsx() -> Self {
        Self {
            dialect: Dialect::Jsx,
            tag_name_start: TagNameStart {
                ascii_letter: true,
                underscore_dollar: true,
                member_namespace: true,
            },
            opaque_regions: JSX_OPAQUE,
            attribute_forms: AttributeForms {
                plain: true,
                quoted_value: true,
                unquoted_value: false,
                braced_value: true,
                spread: true,
            },
            scan_child_expression_containers: true,
        }
    }

    /// Vue profile: HTML-like tag names and opaque regions, with braced/spread
    /// forms enabled for directive expressions (`:class="..."`, `v-bind="obj"`).
    #[must_use]
    pub const fn vue() -> Self {
        Self {
            dialect: Dialect::Vue,
            tag_name_start: TagNameStart {
                ascii_letter: true,
                underscore_dollar: true,
                member_namespace: false,
            },
            opaque_regions: MARKUP_OPAQUE,
            attribute_forms: AttributeForms {
                plain: true,
                quoted_value: true,
                unquoted_value: true,
                braced_value: true,
                spread: true,
            },
            scan_child_expression_containers: false,
        }
    }

    /// Astro profile: HTML-like tag names plus component identifiers, markup
    /// opaque regions plus frontmatter, and braced/spread forms for `client:*`
    /// and expression attributes.
    #[must_use]
    pub const fn astro() -> Self {
        Self {
            dialect: Dialect::Astro,
            tag_name_start: TagNameStart {
                ascii_letter: true,
                underscore_dollar: true,
                member_namespace: true,
            },
            opaque_regions: ASTRO_OPAQUE,
            attribute_forms: AttributeForms {
                plain: true,
                quoted_value: true,
                unquoted_value: true,
                braced_value: true,
                spread: true,
            },
            scan_child_expression_containers: false,
        }
    }

    /// Svelte profile: HTML-like tag names plus component identifiers, markup
    /// opaque regions, and braced/spread forms for `{...}` directives and
    /// `on:click={...}` handlers.
    #[must_use]
    pub const fn svelte() -> Self {
        Self {
            dialect: Dialect::Svelte,
            tag_name_start: TagNameStart {
                ascii_letter: true,
                underscore_dollar: true,
                member_namespace: false,
            },
            opaque_regions: MARKUP_OPAQUE,
            attribute_forms: AttributeForms {
                plain: true,
                quoted_value: true,
                unquoted_value: true,
                braced_value: true,
                spread: true,
            },
            scan_child_expression_containers: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a configuration with the given optional-dialect flags.
    fn config_with(vue: bool, astro: bool, svelte: bool) -> Configuration {
        Configuration {
            vue,
            astro,
            svelte,
            ..Configuration::default()
        }
    }

    #[test]
    fn always_supported_extensions_resolve() {
        let config = Configuration::default();
        assert_eq!(Dialect::from_extension("html", &config), Some(Dialect::Html));
        assert_eq!(Dialect::from_extension("jsx", &config), Some(Dialect::Jsx));
        assert_eq!(Dialect::from_extension("tsx", &config), Some(Dialect::Tsx));
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        let config = config_with(true, true, true);
        assert_eq!(Dialect::from_extension("HTML", &config), Some(Dialect::Html));
        assert_eq!(Dialect::from_extension("Jsx", &config), Some(Dialect::Jsx));
        assert_eq!(Dialect::from_extension("TSX", &config), Some(Dialect::Tsx));
        assert_eq!(Dialect::from_extension("Vue", &config), Some(Dialect::Vue));
        assert_eq!(Dialect::from_extension("ASTRO", &config), Some(Dialect::Astro));
        assert_eq!(Dialect::from_extension("SvElTe", &config), Some(Dialect::Svelte));
    }

    #[test]
    fn optional_dialects_require_enablement() {
        let disabled = config_with(false, false, false);
        assert_eq!(Dialect::from_extension("vue", &disabled), None);
        assert_eq!(Dialect::from_extension("astro", &disabled), None);
        assert_eq!(Dialect::from_extension("svelte", &disabled), None);

        let enabled = config_with(true, true, true);
        assert_eq!(Dialect::from_extension("vue", &enabled), Some(Dialect::Vue));
        assert_eq!(Dialect::from_extension("astro", &enabled), Some(Dialect::Astro));
        assert_eq!(
            Dialect::from_extension("svelte", &enabled),
            Some(Dialect::Svelte)
        );
    }

    #[test]
    fn optional_dialects_are_enabled_independently() {
        let only_vue = config_with(true, false, false);
        assert_eq!(Dialect::from_extension("vue", &only_vue), Some(Dialect::Vue));
        assert_eq!(Dialect::from_extension("astro", &only_vue), None);
        assert_eq!(Dialect::from_extension("svelte", &only_vue), None);

        let only_svelte = config_with(false, false, true);
        assert_eq!(Dialect::from_extension("vue", &only_svelte), None);
        assert_eq!(Dialect::from_extension("astro", &only_svelte), None);
        assert_eq!(
            Dialect::from_extension("svelte", &only_svelte),
            Some(Dialect::Svelte)
        );
    }

    #[test]
    fn unknown_extensions_resolve_to_none() {
        let config = config_with(true, true, true);
        assert_eq!(Dialect::from_extension("md", &config), None);
        assert_eq!(Dialect::from_extension("ts", &config), None);
        assert_eq!(Dialect::from_extension("js", &config), None);
        assert_eq!(Dialect::from_extension("", &config), None);
        // A leading dot is not stripped by this function; callers pass the bare
        // extension, so a dotted form does not match.
        assert_eq!(Dialect::from_extension(".html", &config), None);
    }

    #[test]
    fn jsx_and_tsx_share_a_scanner_profile() {
        let jsx = Dialect::Jsx.scanner_profile();
        let tsx = Dialect::Tsx.scanner_profile();
        // Both use the JS-family profile; only the originating dialect label
        // would differ, and the shared constructor uses `Jsx` for both.
        assert_eq!(jsx, tsx);
        assert!(jsx.attribute_forms.braced_value);
        assert!(jsx.attribute_forms.spread);
        assert!(!jsx.attribute_forms.unquoted_value);
        assert!(jsx.tag_name_start.underscore_dollar);
        assert!(jsx.tag_name_start.member_namespace);
        // The JSX/TSX profile skips child-position `{ ... }` containers; TSX
        // shares the JSX profile, so the flag holds for both.
        assert!(jsx.scan_child_expression_containers);
        assert!(tsx.scan_child_expression_containers);
    }

    #[test]
    fn only_jsx_tsx_skip_child_expression_containers() {
        assert!(ScannerProfile::jsx().scan_child_expression_containers);
        assert!(!ScannerProfile::html().scan_child_expression_containers);
        assert!(!ScannerProfile::vue().scan_child_expression_containers);
        assert!(!ScannerProfile::astro().scan_child_expression_containers);
        assert!(!ScannerProfile::svelte().scan_child_expression_containers);
    }

    #[test]
    fn html_profile_allows_unquoted_values_and_no_braces() {
        let html = Dialect::Html.scanner_profile();
        assert_eq!(html.dialect, Dialect::Html);
        assert!(html.attribute_forms.unquoted_value);
        assert!(!html.attribute_forms.braced_value);
        assert!(!html.attribute_forms.spread);
        assert!(html.opaque_regions.contains(&OpaqueRegion::RawTextElement));
        assert!(html.opaque_regions.contains(&OpaqueRegion::Comment));
        assert!(!html.tag_name_start.underscore_dollar);
    }

    #[test]
    fn astro_profile_recognizes_frontmatter() {
        let astro = Dialect::Astro.scanner_profile();
        assert_eq!(astro.dialect, Dialect::Astro);
        assert!(astro.opaque_regions.contains(&OpaqueRegion::Frontmatter));
        assert!(astro.attribute_forms.braced_value);
    }

    #[test]
    fn vue_and_svelte_profiles_enable_braced_and_spread_forms() {
        for profile in [
            Dialect::Vue.scanner_profile(),
            Dialect::Svelte.scanner_profile(),
        ] {
            assert!(profile.attribute_forms.braced_value);
            assert!(profile.attribute_forms.spread);
            assert!(profile.attribute_forms.quoted_value);
            assert!(profile.attribute_forms.plain);
        }
    }

    #[test]
    fn tag_name_start_accepts_expected_first_chars() {
        let html = Dialect::Html.scanner_profile().tag_name_start;
        assert!(html.accepts_first_char('d'));
        assert!(html.accepts_first_char('D'));
        assert!(!html.accepts_first_char('_'));
        assert!(!html.accepts_first_char('1'));

        let jsx = Dialect::Jsx.scanner_profile().tag_name_start;
        assert!(jsx.accepts_first_char('F'));
        assert!(jsx.accepts_first_char('_'));
        assert!(jsx.accepts_first_char('$'));
        assert!(!jsx.accepts_first_char('1'));
    }
}

/// Property-based tests for extension and dialect resolution.
///
/// **Property 3: Extension and dialect resolution**
///
/// **Validates: Requirements 1.4, 2.1, 3.1, 3.2, 3.3**
///
/// For all file extensions (in any letter casing) and all combinations of the
/// Vue/Astro/Svelte enablement flags, the resolved dialect equals the defined
/// function of the configuration: `html`, `jsx`, `tsx` always resolve; each
/// optional dialect resolves iff its flag is enabled; any other extension
/// resolves to `None`; and resolution is case-insensitive.
///
/// The `fileExtensions`/`additive` facet of Property 3 (the reported file-match
/// set) is validated separately alongside `resolve_config` (task 3.2), which
/// owns the `FileMatchingInfo` derivation; here we exercise the dialect
/// resolution function that drives that set.
#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    /// Build a configuration with the given optional-dialect flags.
    fn config_with(vue: bool, astro: bool, svelte: bool) -> Configuration {
        Configuration {
            vue,
            astro,
            svelte,
            ..Configuration::default()
        }
    }

    /// Randomly recase each ASCII-alphabetic character of `s`, driven by a bit
    /// vector, so the generator explores the full case-insensitive input space
    /// rather than only the all-lower / all-upper extremes.
    fn recase(s: &str, bits: &[bool]) -> String {
        s.chars()
            .enumerate()
            .map(|(i, c)| {
                if bits.get(i).copied().unwrap_or(false) {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect()
    }

    /// The always-supported base extensions and their expected dialect.
    const ALWAYS: &[(&str, Dialect)] = &[
        ("html", Dialect::Html),
        ("jsx", Dialect::Jsx),
        ("tsx", Dialect::Tsx),
    ];

    /// The optional, flag-gated extensions and their expected dialect.
    const OPTIONAL: &[(&str, Dialect)] = &[
        ("vue", Dialect::Vue),
        ("astro", Dialect::Astro),
        ("svelte", Dialect::Svelte),
    ];

    proptest! {
        /// HTML/JSX/TSX resolve to their dialect for every flag combination and
        /// every letter casing (Requirements 3.1, 3.2).
        #[test]
        fn always_supported_resolve_regardless_of_config(
            vue in any::<bool>(),
            astro in any::<bool>(),
            svelte in any::<bool>(),
            idx in 0usize..ALWAYS.len(),
            case_bits in prop::collection::vec(any::<bool>(), 0..8),
        ) {
            let config = config_with(vue, astro, svelte);
            let (ext, expected) = ALWAYS[idx];
            let recased = recase(ext, &case_bits);
            prop_assert_eq!(
                Dialect::from_extension(&recased, &config),
                Some(expected)
            );
        }

        /// Vue/Astro/Svelte resolve iff their own flag is enabled, independent of
        /// the other flags, for every letter casing (Requirements 3.3, 3.4).
        #[test]
        fn optional_dialects_resolve_iff_enabled(
            vue in any::<bool>(),
            astro in any::<bool>(),
            svelte in any::<bool>(),
            idx in 0usize..OPTIONAL.len(),
            case_bits in prop::collection::vec(any::<bool>(), 0..8),
        ) {
            let flags = [vue, astro, svelte];
            let config = config_with(vue, astro, svelte);
            let (ext, expected) = OPTIONAL[idx];
            let recased = recase(ext, &case_bits);
            let resolved = Dialect::from_extension(&recased, &config);
            if flags[idx] {
                prop_assert_eq!(resolved, Some(expected));
            } else {
                prop_assert_eq!(resolved, None);
            }
        }

        /// Any extension that is not one of the six known extensions resolves to
        /// `None` for every flag combination and casing (Requirement 3.5), which
        /// backstops the "not in the resolved set" facet of Requirements 1.4/2.1.
        #[test]
        fn unknown_extensions_resolve_to_none(
            vue in any::<bool>(),
            astro in any::<bool>(),
            svelte in any::<bool>(),
            raw in "[A-Za-z0-9._-]{0,12}",
        ) {
            // Restrict to inputs that are not any known extension, case-folded.
            let lowered = raw.to_ascii_lowercase();
            let is_known = ALWAYS
                .iter()
                .chain(OPTIONAL.iter())
                .any(|(ext, _)| *ext == lowered);
            prop_assume!(!is_known);

            let config = config_with(vue, astro, svelte);
            prop_assert_eq!(Dialect::from_extension(&raw, &config), None);
        }

        /// Resolution is case-insensitive: for every known extension, an
        /// arbitrary recasing resolves identically to the canonical lowercase
        /// form under the same configuration (Requirements 3.1, 3.2, 3.3).
        #[test]
        fn resolution_is_case_insensitive(
            vue in any::<bool>(),
            astro in any::<bool>(),
            svelte in any::<bool>(),
            idx in 0usize..(ALWAYS.len() + OPTIONAL.len()),
            case_bits in prop::collection::vec(any::<bool>(), 0..8),
        ) {
            let config = config_with(vue, astro, svelte);
            let (ext, _) = ALWAYS
                .iter()
                .chain(OPTIONAL.iter())
                .nth(idx)
                .copied()
                .expect("idx is bounded by the combined extension count");
            let recased = recase(ext, &case_bits);
            prop_assert_eq!(
                Dialect::from_extension(&recased, &config),
                Dialect::from_extension(ext, &config)
            );
        }
    }
}
