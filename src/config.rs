//! Configuration types, defaults, resolution, and diagnostics.
//!
//! The `Configuration` struct and `SchemeId` enum are the resolved, strongly
//! typed configuration shared across the core. Resolution, diagnostics, and the
//! JSON schema are implemented in task 3; this module currently provides the
//! types and their defaults so dialect resolution (task 4) can gate optional
//! dialects on the enablement flags.

#[cfg(feature = "wasm")]
use dprint_core::configuration::{
    get_unknown_property_diagnostics, ConfigKeyMap, ConfigKeyValue, ConfigurationDiagnostic,
    GlobalConfiguration,
};
#[cfg(feature = "wasm")]
use dprint_core::plugins::{FileMatchingInfo, PluginResolveConfigurationResult};
use serde::{Deserialize, Serialize};

/// Resolved plugin configuration.
///
/// Field names serialize in camelCase to match the published dprint
/// configuration keys. `indent_width` and `use_tabs` are sourced from the dprint
/// `GlobalConfiguration` rather than declared as plugin properties.
///
/// The several independent boolean flags (`fluid_wrapping`, `attribute_sorting`,
/// and the `vue`/`astro`/`svelte` dialect toggles) are each a distinct,
/// user-facing on/off configuration knob defined by the design's property table,
/// not internal state that a state machine or enum would model more clearly, so
/// `clippy::struct_excessive_bools` is deliberately allowed here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Configuration {
    /// Maximum line width in columns (1..=1000). Defaults to the dprint global
    /// line width during resolution.
    pub line_width: u32,
    /// Whether fluid (greedy) wrapping is enabled. Default `true`.
    pub fluid_wrapping: bool,
    /// Whether attribute sorting is enabled. Default `false`.
    pub attribute_sorting: bool,
    /// The attribute order scheme applied when sorting is enabled.
    pub attribute_order_scheme: SchemeId,
    /// Indentation width in columns, from the dprint `GlobalConfiguration`.
    pub indent_width: u8,
    /// Whether to indent with tabs, from the dprint `GlobalConfiguration`.
    pub use_tabs: bool,
    /// Whether the Vue dialect is enabled. Default `false`.
    pub vue: bool,
    /// Whether the Astro dialect is enabled. Default `false`.
    pub astro: bool,
    /// Whether the Svelte dialect is enabled. Default `false`.
    pub svelte: bool,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            line_width: 80,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: false,
            astro: false,
            svelte: false,
        }
    }
}

/// Identifier of a built-in attribute order scheme.
///
/// Serializes in kebab-case to match the published `attributeOrderScheme`
/// configuration values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SchemeId {
    /// The single built-in `standard` scheme, and the default when sorting is
    /// enabled with no scheme explicitly configured.
    #[default]
    Standard,
}

/// Inclusive lower bound for the plugin `lineWidth` property.
#[cfg(feature = "wasm")]
const LINE_WIDTH_MIN: i64 = 1;
/// Inclusive upper bound for the plugin `lineWidth` property.
#[cfg(feature = "wasm")]
const LINE_WIDTH_MAX: i64 = 1000;

/// Default indentation width used when the dprint `GlobalConfiguration` does not
/// provide one.
#[cfg(feature = "wasm")]
const DEFAULT_INDENT_WIDTH: u8 = 2;

/// The known `attributeOrderScheme` values, used both for validation and for the
/// diagnostic message that lists the allowed values.
#[cfg(feature = "wasm")]
const KNOWN_SCHEMES: &[&str] = &["standard"];

#[cfg(feature = "wasm")]
impl SchemeId {
    /// Resolve a scheme identifier string to a [`SchemeId`], matching the
    /// published kebab-case values. Returns `None` for any unrecognized value.
    fn from_scheme_str(value: &str) -> Option<Self> {
        match value {
            "standard" => Some(Self::Standard),
            _ => None,
        }
    }
}

/// Resolve the plugin [`Configuration`] from the raw dprint `ConfigKeyMap` and
/// `GlobalConfiguration`.
///
/// Resolution reads each known property, validating its type and (for
/// `lineWidth`) its range, and takes `indentWidth`/`useTabs` from the global
/// configuration. For every offending value it emits one
/// [`ConfigurationDiagnostic`] describing the problem and falls back to that
/// property's default, while retaining every valid value (Requirements 6.6,
/// 6.7). Each property remaining in the map after the known keys are consumed is
/// reported as an unknown-property diagnostic, one per property (Requirement
/// 6.5). An unrecognized `attributeOrderScheme` produces a diagnostic and falls
/// back to the `standard` scheme (Requirement 5.7).
///
/// `lineWidth` defaults to the global `line_width` when the plugin-level value is
/// absent (Requirement 6.1). The returned [`FileMatchingInfo`] lists the always
/// supported `html`/`jsx`/`tsx` extensions plus any enabled optional dialect
/// extensions, with `additive = true` so the plugin runs as a post-processing
/// pass (Requirements 1.4, 2.1).
#[cfg(feature = "wasm")]
#[must_use]
pub fn resolve_config(
    mut config: ConfigKeyMap,
    global: &GlobalConfiguration,
) -> PluginResolveConfigurationResult<Configuration> {
    let mut diagnostics: Vec<ConfigurationDiagnostic> = Vec::new();

    // `indentWidth`/`useTabs` come from the global configuration, not the plugin
    // config map (Requirement 6.1 companion: the plugin does not declare them).
    let indent_width = global.indent_width.unwrap_or(DEFAULT_INDENT_WIDTH);
    let use_tabs = global.use_tabs.unwrap_or(false);

    // `lineWidth` defaults to the global line width when the plugin value is
    // absent, and to the built-in default when the global is also absent.
    let line_width_default = global
        .line_width
        .unwrap_or_else(|| Configuration::default().line_width);
    let line_width = resolve_line_width(&mut config, line_width_default, &mut diagnostics);

    let defaults = Configuration::default();
    let fluid_wrapping = resolve_bool(
        &mut config,
        "fluidWrapping",
        defaults.fluid_wrapping,
        &mut diagnostics,
    );
    let attribute_sorting = resolve_bool(
        &mut config,
        "attributeSorting",
        defaults.attribute_sorting,
        &mut diagnostics,
    );
    let attribute_order_scheme = resolve_scheme(&mut config, &mut diagnostics);
    let vue = resolve_bool(&mut config, "vue", defaults.vue, &mut diagnostics);
    let astro = resolve_bool(&mut config, "astro", defaults.astro, &mut diagnostics);
    let svelte = resolve_bool(&mut config, "svelte", defaults.svelte, &mut diagnostics);

    // Every remaining key is unknown; one diagnostic per property (Req 6.5).
    diagnostics.extend(get_unknown_property_diagnostics(config));

    let resolved = Configuration {
        line_width,
        fluid_wrapping,
        attribute_sorting,
        attribute_order_scheme,
        indent_width,
        use_tabs,
        vue,
        astro,
        svelte,
    };

    let file_matching = build_file_matching_info(&resolved);

    PluginResolveConfigurationResult {
        file_matching,
        diagnostics,
        config: resolved,
    }
}

/// Build the resolved [`FileMatchingInfo`]: the always supported extensions plus
/// any enabled optional dialect extensions, no file names, and additive mode.
#[cfg(feature = "wasm")]
fn build_file_matching_info(config: &Configuration) -> FileMatchingInfo {
    let mut file_extensions = vec![
        "html".to_string(),
        "jsx".to_string(),
        "tsx".to_string(),
    ];
    if config.vue {
        file_extensions.push("vue".to_string());
    }
    if config.astro {
        file_extensions.push("astro".to_string());
    }
    if config.svelte {
        file_extensions.push("svelte".to_string());
    }
    FileMatchingInfo {
        file_extensions,
        file_names: Vec::new(),
        additive: true,
    }
}

/// Resolve and validate the `lineWidth` property.
///
/// Requires an integer value in `1..=1000`. A non-integer value yields a
/// type diagnostic; an out-of-range integer yields a range diagnostic. In both
/// cases the default is applied (Requirements 6.1, 6.6, 6.7).
#[cfg(feature = "wasm")]
fn resolve_line_width(
    config: &mut ConfigKeyMap,
    default_value: u32,
    diagnostics: &mut Vec<ConfigurationDiagnostic>,
) -> u32 {
    let Some(value) = config.shift_remove("lineWidth") else {
        return default_value;
    };
    match value {
        ConfigKeyValue::Number(n) => {
            let n = i64::from(n);
            if (LINE_WIDTH_MIN..=LINE_WIDTH_MAX).contains(&n) {
                // Safe: the range check guarantees the value fits in u32.
                u32::try_from(n).unwrap_or(default_value)
            } else {
                diagnostics.push(ConfigurationDiagnostic {
                    property_name: "lineWidth".to_string(),
                    message: format!(
                        "Expected an integer in the range {LINE_WIDTH_MIN} to {LINE_WIDTH_MAX} inclusive, but found {n}."
                    ),
                });
                default_value
            }
        }
        other => {
            diagnostics.push(ConfigurationDiagnostic {
                property_name: "lineWidth".to_string(),
                message: format!(
                    "Expected an integer in the range {LINE_WIDTH_MIN} to {LINE_WIDTH_MAX} inclusive, but found a {}.",
                    value_type_name(&other)
                ),
            });
            default_value
        }
    }
}

/// Resolve and validate a boolean property, applying `default_value` and a type
/// diagnostic when the value is present but not a boolean (Requirements 6.6,
/// 6.7).
#[cfg(feature = "wasm")]
fn resolve_bool(
    config: &mut ConfigKeyMap,
    key: &str,
    default_value: bool,
    diagnostics: &mut Vec<ConfigurationDiagnostic>,
) -> bool {
    let Some(value) = config.shift_remove(key) else {
        return default_value;
    };
    match value {
        ConfigKeyValue::Bool(b) => b,
        other => {
            diagnostics.push(ConfigurationDiagnostic {
                property_name: key.to_string(),
                message: format!(
                    "Expected a boolean (true or false), but found a {}.",
                    value_type_name(&other)
                ),
            });
            default_value
        }
    }
}

/// Resolve and validate the `attributeOrderScheme` property against the known
/// scheme set, falling back to the `standard` scheme with a diagnostic when the
/// value has the wrong type or names an unrecognized scheme (Requirements 6.4,
/// 5.7).
#[cfg(feature = "wasm")]
fn resolve_scheme(
    config: &mut ConfigKeyMap,
    diagnostics: &mut Vec<ConfigurationDiagnostic>,
) -> SchemeId {
    let Some(value) = config.shift_remove("attributeOrderScheme") else {
        return SchemeId::Standard;
    };
    match value {
        ConfigKeyValue::String(ref s) => {
            if let Some(scheme) = SchemeId::from_scheme_str(s) {
                scheme
            } else {
                diagnostics.push(ConfigurationDiagnostic {
                    property_name: "attributeOrderScheme".to_string(),
                    message: format!(
                        "Unrecognized attribute order scheme '{s}'. Allowed values: {}.",
                        KNOWN_SCHEMES.join(", ")
                    ),
                });
                SchemeId::Standard
            }
        }
        other => {
            diagnostics.push(ConfigurationDiagnostic {
                property_name: "attributeOrderScheme".to_string(),
                message: format!(
                    "Expected one of the allowed scheme values ({}), but found a {}.",
                    KNOWN_SCHEMES.join(", "),
                    value_type_name(&other)
                ),
            });
            SchemeId::Standard
        }
    }
}

/// A short human-readable name for a config value's underlying type, used in
/// type-mismatch diagnostics.
#[cfg(feature = "wasm")]
fn value_type_name(value: &ConfigKeyValue) -> &'static str {
    match value {
        ConfigKeyValue::String(_) => "string",
        ConfigKeyValue::Number(_) => "number",
        ConfigKeyValue::Bool(_) => "boolean",
        ConfigKeyValue::Array(_) => "array",
        ConfigKeyValue::Object(_) => "object",
        ConfigKeyValue::Null => "null",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_id_defaults_to_standard() {
        assert_eq!(SchemeId::default(), SchemeId::Standard);
    }

    #[test]
    fn scheme_id_serializes_kebab_case() {
        let json = serde_json::to_string(&SchemeId::Standard).unwrap();
        assert_eq!(json, "\"standard\"");
    }

    #[test]
    fn scheme_id_deserializes_kebab_case() {
        let scheme: SchemeId = serde_json::from_str("\"standard\"").unwrap();
        assert_eq!(scheme, SchemeId::Standard);
    }

    #[test]
    fn configuration_defaults_match_spec() {
        let config = Configuration::default();
        assert_eq!(config.line_width, 80, "built-in line width default");
        assert!(config.fluid_wrapping, "fluid wrapping defaults to enabled");
        assert!(
            !config.attribute_sorting,
            "attribute sorting defaults to disabled"
        );
        assert_eq!(config.attribute_order_scheme, SchemeId::Standard);
        assert_eq!(config.indent_width, 2, "indent width default");
        assert!(!config.use_tabs, "use tabs defaults to disabled");
        assert!(!config.vue, "vue dialect defaults to disabled");
        assert!(!config.astro, "astro dialect defaults to disabled");
        assert!(!config.svelte, "svelte dialect defaults to disabled");
    }

    #[test]
    fn configuration_serializes_camel_case_keys() {
        let value = serde_json::to_value(Configuration::default()).unwrap();
        let obj = value.as_object().unwrap();
        for key in [
            "lineWidth",
            "fluidWrapping",
            "attributeSorting",
            "attributeOrderScheme",
            "indentWidth",
            "useTabs",
            "vue",
            "astro",
            "svelte",
        ] {
            assert!(obj.contains_key(key), "missing camelCase key: {key}");
        }
        assert_eq!(obj["attributeOrderScheme"], serde_json::json!("standard"));
    }

    #[test]
    fn configuration_round_trips_through_json() {
        let config = Configuration {
            line_width: 100,
            fluid_wrapping: false,
            attribute_sorting: true,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 4,
            use_tabs: true,
            vue: true,
            astro: false,
            svelte: true,
        };
        let json = serde_json::to_string(&config).unwrap();
        let restored: Configuration = serde_json::from_str(&json).unwrap();
        assert_eq!(config, restored);
    }
}

/// Tests for configuration resolution, diagnostics, and file-matching derivation.
///
/// These are gated on the `wasm` feature because resolution depends on the
/// dprint `plugins`/`configuration` types that only exist with that feature.
#[cfg(all(test, feature = "wasm"))]
mod resolve_tests {
    use super::*;
    use dprint_core::configuration::{
        ConfigKeyMap, ConfigKeyValue, ConfigurationDiagnostic, GlobalConfiguration,
    };

    /// A global configuration with every field populated.
    fn global_with(line_width: Option<u32>, indent_width: Option<u8>, use_tabs: Option<bool>) -> GlobalConfiguration {
        GlobalConfiguration {
            line_width,
            use_tabs,
            indent_width,
            new_line_kind: None,
        }
    }

    /// A global configuration with sensible explicit values for width/indent.
    fn default_global() -> GlobalConfiguration {
        global_with(Some(120), Some(2), Some(false))
    }

    fn find_diagnostic<'a>(
        diagnostics: &'a [ConfigurationDiagnostic],
        property: &str,
    ) -> Option<&'a ConfigurationDiagnostic> {
        diagnostics.iter().find(|d| d.property_name == property)
    }

    #[test]
    fn empty_config_resolves_to_defaults() {
        let result = resolve_config(ConfigKeyMap::new(), &default_global());
        assert!(result.diagnostics.is_empty(), "no diagnostics for empty config");
        let config = result.config;
        // line_width falls back to the global line width.
        assert_eq!(config.line_width, 120);
        assert!(config.fluid_wrapping);
        assert!(!config.attribute_sorting);
        assert_eq!(config.attribute_order_scheme, SchemeId::Standard);
        assert_eq!(config.indent_width, 2);
        assert!(!config.use_tabs);
        assert!(!config.vue);
        assert!(!config.astro);
        assert!(!config.svelte);
    }

    #[test]
    fn line_width_defaults_to_global_line_width() {
        let result = resolve_config(ConfigKeyMap::new(), &global_with(Some(100), Some(2), Some(false)));
        assert_eq!(result.config.line_width, 100);
    }

    #[test]
    fn line_width_defaults_to_builtin_when_global_absent() {
        let result = resolve_config(ConfigKeyMap::new(), &global_with(None, None, None));
        assert_eq!(result.config.line_width, Configuration::default().line_width);
    }

    #[test]
    fn indent_width_and_use_tabs_come_from_global() {
        let result = resolve_config(ConfigKeyMap::new(), &global_with(Some(80), Some(4), Some(true)));
        assert_eq!(result.config.indent_width, 4);
        assert!(result.config.use_tabs);
    }

    #[test]
    fn valid_plugin_values_are_read() {
        let config = ConfigKeyMap::from([
            ("lineWidth".to_string(), ConfigKeyValue::from_i32(60)),
            ("fluidWrapping".to_string(), ConfigKeyValue::from_bool(false)),
            ("attributeSorting".to_string(), ConfigKeyValue::from_bool(true)),
            (
                "attributeOrderScheme".to_string(),
                ConfigKeyValue::from_str("standard"),
            ),
            ("vue".to_string(), ConfigKeyValue::from_bool(true)),
            ("astro".to_string(), ConfigKeyValue::from_bool(true)),
            ("svelte".to_string(), ConfigKeyValue::from_bool(true)),
        ]);
        let result = resolve_config(config, &default_global());
        assert!(
            result.diagnostics.is_empty(),
            "valid values should not produce diagnostics"
        );
        let c = result.config;
        assert_eq!(c.line_width, 60);
        assert!(!c.fluid_wrapping);
        assert!(c.attribute_sorting);
        assert_eq!(c.attribute_order_scheme, SchemeId::Standard);
        assert!(c.vue && c.astro && c.svelte);
    }

    #[test]
    fn line_width_boundaries_are_accepted() {
        for width in [1, 1000] {
            let config = ConfigKeyMap::from([(
                "lineWidth".to_string(),
                ConfigKeyValue::from_i32(width),
            )]);
            let result = resolve_config(config, &default_global());
            assert!(
                result.diagnostics.is_empty(),
                "width {width} should be valid"
            );
            assert_eq!(result.config.line_width, u32::try_from(width).unwrap());
        }
    }

    #[test]
    fn line_width_out_of_range_emits_diagnostic_and_uses_default() {
        for width in [0, 1001] {
            let config = ConfigKeyMap::from([(
                "lineWidth".to_string(),
                ConfigKeyValue::from_i32(width),
            )]);
            let result = resolve_config(config, &global_with(Some(120), Some(2), Some(false)));
            let diag = find_diagnostic(&result.diagnostics, "lineWidth")
                .expect("expected a lineWidth diagnostic");
            assert!(diag.message.contains("1 to 1000"), "message: {}", diag.message);
            // Falls back to the global default.
            assert_eq!(result.config.line_width, 120);
        }
    }

    #[test]
    fn line_width_wrong_type_emits_type_diagnostic_and_uses_default() {
        let config = ConfigKeyMap::from([(
            "lineWidth".to_string(),
            ConfigKeyValue::from_bool(true),
        )]);
        let result = resolve_config(config, &global_with(Some(120), Some(2), Some(false)));
        let diag = find_diagnostic(&result.diagnostics, "lineWidth")
            .expect("expected a lineWidth diagnostic");
        assert!(diag.message.contains("boolean"), "message: {}", diag.message);
        assert_eq!(result.config.line_width, 120);
    }

    #[test]
    fn boolean_wrong_type_emits_diagnostic_and_uses_default() {
        let config = ConfigKeyMap::from([(
            "fluidWrapping".to_string(),
            ConfigKeyValue::from_i32(3),
        )]);
        let result = resolve_config(config, &default_global());
        let diag = find_diagnostic(&result.diagnostics, "fluidWrapping")
            .expect("expected a fluidWrapping diagnostic");
        assert!(diag.message.contains("boolean"), "message: {}", diag.message);
        // Default fluid_wrapping is true, retained on invalid input.
        assert!(result.config.fluid_wrapping);
    }

    #[test]
    fn unrecognized_scheme_emits_diagnostic_and_falls_back() {
        let config = ConfigKeyMap::from([(
            "attributeOrderScheme".to_string(),
            ConfigKeyValue::from_str("fancy"),
        )]);
        let result = resolve_config(config, &default_global());
        let diag = find_diagnostic(&result.diagnostics, "attributeOrderScheme")
            .expect("expected an attributeOrderScheme diagnostic");
        assert!(diag.message.contains("fancy"), "message: {}", diag.message);
        assert!(diag.message.contains("standard"), "message lists allowed values");
        assert_eq!(result.config.attribute_order_scheme, SchemeId::Standard);
    }

    #[test]
    fn scheme_wrong_type_emits_diagnostic_and_falls_back() {
        let config = ConfigKeyMap::from([(
            "attributeOrderScheme".to_string(),
            ConfigKeyValue::from_bool(true),
        )]);
        let result = resolve_config(config, &default_global());
        let diag = find_diagnostic(&result.diagnostics, "attributeOrderScheme")
            .expect("expected an attributeOrderScheme diagnostic");
        assert!(diag.message.contains("boolean"), "message: {}", diag.message);
        assert_eq!(result.config.attribute_order_scheme, SchemeId::Standard);
    }

    #[test]
    fn unknown_properties_emit_one_diagnostic_each() {
        let config = ConfigKeyMap::from([
            ("mystery".to_string(), ConfigKeyValue::from_bool(true)),
            ("another".to_string(), ConfigKeyValue::from_i32(1)),
            ("lineWidth".to_string(), ConfigKeyValue::from_i32(90)),
        ]);
        let result = resolve_config(config, &default_global());
        assert!(find_diagnostic(&result.diagnostics, "mystery").is_some());
        assert!(find_diagnostic(&result.diagnostics, "another").is_some());
        // Valid known value is retained.
        assert_eq!(result.config.line_width, 90);
        // Exactly two unknown-property diagnostics, no false positives.
        let unknown = result
            .diagnostics
            .iter()
            .filter(|d| d.message.contains("Unknown property"))
            .count();
        assert_eq!(unknown, 2);
    }

    #[test]
    fn multiple_offending_values_retain_valid_ones() {
        let config = ConfigKeyMap::from([
            ("lineWidth".to_string(), ConfigKeyValue::from_i32(5000)), // out of range
            ("fluidWrapping".to_string(), ConfigKeyValue::from_bool(false)), // valid
            ("attributeSorting".to_string(), ConfigKeyValue::from_i32(1)), // wrong type
        ]);
        let result = resolve_config(config, &global_with(Some(120), Some(2), Some(false)));
        // Two diagnostics: lineWidth range + attributeSorting type.
        assert!(find_diagnostic(&result.diagnostics, "lineWidth").is_some());
        assert!(find_diagnostic(&result.diagnostics, "attributeSorting").is_some());
        // Offending values fall back to defaults.
        assert_eq!(result.config.line_width, 120);
        assert!(!result.config.attribute_sorting);
        // The valid value is retained.
        assert!(!result.config.fluid_wrapping);
    }

    // ---- FileMatchingInfo ----------------------------------------------

    #[test]
    fn file_matching_defaults_to_always_supported_extensions() {
        let result = resolve_config(ConfigKeyMap::new(), &default_global());
        let fm = result.file_matching;
        assert_eq!(fm.file_extensions, vec!["html", "jsx", "tsx"]);
        assert!(fm.file_names.is_empty(), "no file names are matched by name");
        assert!(fm.additive, "plugin runs as an additive post-processing pass");
    }

    #[test]
    fn file_matching_adds_enabled_optional_dialects() {
        let config = ConfigKeyMap::from([
            ("vue".to_string(), ConfigKeyValue::from_bool(true)),
            ("svelte".to_string(), ConfigKeyValue::from_bool(true)),
        ]);
        let result = resolve_config(config, &default_global());
        let fm = result.file_matching;
        assert_eq!(
            fm.file_extensions,
            vec!["html", "jsx", "tsx", "vue", "svelte"]
        );
        assert!(!fm.file_extensions.contains(&"astro".to_string()));
    }

    #[test]
    fn file_matching_includes_all_optional_dialects_when_enabled() {
        let config = ConfigKeyMap::from([
            ("vue".to_string(), ConfigKeyValue::from_bool(true)),
            ("astro".to_string(), ConfigKeyValue::from_bool(true)),
            ("svelte".to_string(), ConfigKeyValue::from_bool(true)),
        ]);
        let result = resolve_config(config, &default_global());
        assert_eq!(
            result.file_matching.file_extensions,
            vec!["html", "jsx", "tsx", "vue", "astro", "svelte"]
        );
    }
}

/// Property-based tests for `lineWidth` resolution and validation (Property 16).
///
/// Gated on the `wasm` feature because `resolve_config` and its helpers depend
/// on the dprint `plugins`/`configuration` types that only exist with that
/// feature.
#[cfg(all(test, feature = "wasm"))]
mod line_width_property_tests {
    use super::*;
    use dprint_core::configuration::{
        ConfigKeyMap, ConfigKeyValue, GlobalConfiguration,
    };
    use proptest::prelude::*;

    /// Build a `GlobalConfiguration` from an optional global line width, leaving
    /// the indent/tab fields at benign values that this property does not touch.
    fn global_with_line_width(line_width: Option<u32>) -> GlobalConfiguration {
        GlobalConfiguration {
            line_width,
            use_tabs: Some(false),
            indent_width: Some(2),
            new_line_kind: None,
        }
    }

    /// The default line width that resolution falls back to for a given global
    /// configuration: the global `line_width` when present, otherwise the
    /// built-in [`Configuration`] default.
    fn expected_default(global_line_width: Option<u32>) -> u32 {
        global_line_width.unwrap_or_else(|| Configuration::default().line_width)
    }

    /// Whether a resolved line width came back with a `lineWidth` diagnostic.
    fn has_line_width_diagnostic(
        result: &PluginResolveConfigurationResult<Configuration>,
    ) -> bool {
        result
            .diagnostics
            .iter()
            .any(|d| d.property_name == "lineWidth")
    }

    proptest! {
        /// Property 16 (Validates: Requirements 6.1).
        ///
        /// An absent `lineWidth` resolves to the global line width when present,
        /// or the built-in default when the global is also absent, with no
        /// `lineWidth` diagnostic. The resolved value is always within 1..=1000
        /// because every fallback source (global or built-in default) is itself
        /// in range here.
        #[test]
        fn absent_line_width_falls_back_to_default(
            global_line_width in prop::option::of(1u32..=1000),
        ) {
            let global = global_with_line_width(global_line_width);
            let result = resolve_config(ConfigKeyMap::new(), &global);

            prop_assert!(
                !has_line_width_diagnostic(&result),
                "absent lineWidth must not emit a lineWidth diagnostic"
            );
            prop_assert_eq!(result.config.line_width, expected_default(global_line_width));
            prop_assert!((1..=1000).contains(&result.config.line_width));
        }

        /// Property 16 (Validates: Requirements 6.1).
        ///
        /// Any in-range integer `lineWidth` (1..=1000) resolves to exactly that
        /// value with no diagnostic, regardless of the global line width.
        #[test]
        fn in_range_line_width_is_used_verbatim(
            width in 1i32..=1000,
            global_line_width in prop::option::of(1u32..=1000),
        ) {
            let config = ConfigKeyMap::from([(
                "lineWidth".to_string(),
                ConfigKeyValue::from_i32(width),
            )]);
            let result = resolve_config(config, &global_with_line_width(global_line_width));

            prop_assert!(
                !has_line_width_diagnostic(&result),
                "a valid in-range lineWidth must not emit a diagnostic"
            );
            // Safe: `width` is constrained to 1..=1000, which fits in u32.
            prop_assert_eq!(result.config.line_width, u32::try_from(width).unwrap());
            prop_assert!((1..=1000).contains(&result.config.line_width));
        }

        /// Property 16 (Validates: Requirements 6.1).
        ///
        /// Any out-of-range integer `lineWidth` (<= 0 or > 1000) emits a
        /// `lineWidth` diagnostic and falls back to the default (global or
        /// built-in). The resolved value stays within 1..=1000.
        #[test]
        fn out_of_range_line_width_emits_diagnostic_and_uses_default(
            // Values strictly outside the valid 1..=1000 band, drawn from both
            // the below-minimum (incl. negatives and zero) and above-maximum
            // sides so the generator covers both boundary failures.
            width in prop_oneof![i32::MIN..=0, 1001..=i32::MAX],
            global_line_width in prop::option::of(1u32..=1000),
        ) {
            let config = ConfigKeyMap::from([(
                "lineWidth".to_string(),
                ConfigKeyValue::from_i32(width),
            )]);
            let result = resolve_config(config, &global_with_line_width(global_line_width));

            prop_assert!(
                has_line_width_diagnostic(&result),
                "an out-of-range lineWidth ({width}) must emit a diagnostic"
            );
            prop_assert_eq!(result.config.line_width, expected_default(global_line_width));
            prop_assert!((1..=1000).contains(&result.config.line_width));
        }

        /// Property 16 (Validates: Requirements 6.1).
        ///
        /// A `lineWidth` whose value is not an integer (boolean, string, null)
        /// emits a `lineWidth` diagnostic and falls back to the default. The
        /// resolved value stays within 1..=1000.
        #[test]
        fn non_integer_line_width_emits_diagnostic_and_uses_default(
            value in prop_oneof![
                any::<bool>().prop_map(ConfigKeyValue::from_bool),
                "[a-z]{0,8}".prop_map(|s| ConfigKeyValue::from_str(&s)),
                Just(ConfigKeyValue::Null),
            ],
            global_line_width in prop::option::of(1u32..=1000),
        ) {
            let config = ConfigKeyMap::from([("lineWidth".to_string(), value)]);
            let result = resolve_config(config, &global_with_line_width(global_line_width));

            prop_assert!(
                has_line_width_diagnostic(&result),
                "a non-integer lineWidth must emit a diagnostic"
            );
            prop_assert_eq!(result.config.line_width, expected_default(global_line_width));
            prop_assert!((1..=1000).contains(&result.config.line_width));
        }
    }
}

/// Tests for the published JSON configuration schema (`schema.json`).
///
/// These validate the artifact published at the plugin's `config_schema_url`
/// (Requirement 6.8): that it parses as a draft-07 JSON Schema, rejects unknown
/// keys, and documents every configuration property with both a type and a
/// default. They are deliberately gated on plain `#[cfg(test)]` (no `wasm`
/// feature) because they inspect the JSON artifact with `serde_json` only and
/// do not depend on any dprint runtime types.
#[cfg(test)]
mod schema_tests {
    use serde_json::Value;

    /// The published schema, embedded at compile time so the test always checks
    /// the artifact that ships with the crate.
    const SCHEMA_SRC: &str = include_str!("../schema.json");

    /// Every user-facing configuration property the schema MUST document, each
    /// with the JSON Schema type it is expected to declare. `indentWidth` and
    /// `useTabs` are intentionally absent: they come from the dprint global
    /// configuration and are not plugin properties.
    const EXPECTED_PROPERTIES: &[(&str, &str)] = &[
        ("lineWidth", "integer"),
        ("fluidWrapping", "boolean"),
        ("attributeSorting", "boolean"),
        ("attributeOrderScheme", "string"),
        ("vue", "boolean"),
        ("astro", "boolean"),
        ("svelte", "boolean"),
    ];

    fn parse_schema() -> Value {
        serde_json::from_str(SCHEMA_SRC).expect("schema.json must be valid JSON")
    }

    #[test]
    fn schema_parses_as_json() {
        let schema = parse_schema();
        assert!(schema.is_object(), "schema root must be a JSON object");
    }

    #[test]
    fn schema_declares_draft_07_and_object_type() {
        let schema = parse_schema();
        assert_eq!(
            schema["$schema"].as_str(),
            Some("http://json-schema.org/draft-07/schema#"),
            "schema must declare the draft-07 meta-schema"
        );
        assert_eq!(
            schema["type"].as_str(),
            Some("object"),
            "the configuration root is an object"
        );
    }

    #[test]
    fn schema_disallows_additional_properties() {
        let schema = parse_schema();
        assert_eq!(
            schema["additionalProperties"].as_bool(),
            Some(false),
            "additionalProperties must be false so editors surface unknown keys"
        );
    }

    #[test]
    fn schema_lists_exactly_the_expected_properties() {
        let schema = parse_schema();
        let properties = schema["properties"]
            .as_object()
            .expect("schema must have a `properties` object");

        // Every expected property is present.
        for (name, _) in EXPECTED_PROPERTIES {
            assert!(
                properties.contains_key(*name),
                "schema is missing property `{name}`"
            );
        }

        // No unexpected properties leak in (e.g. the global-only indentWidth).
        assert_eq!(
            properties.len(),
            EXPECTED_PROPERTIES.len(),
            "schema lists properties beyond the expected set: {:?}",
            properties.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn every_property_declares_a_type() {
        let schema = parse_schema();
        let properties = schema["properties"].as_object().unwrap();
        for (name, expected_type) in EXPECTED_PROPERTIES {
            let prop = &properties[*name];
            assert_eq!(
                prop["type"].as_str(),
                Some(*expected_type),
                "property `{name}` must declare type `{expected_type}`"
            );
        }
    }

    #[test]
    fn every_property_declares_a_default() {
        let schema = parse_schema();
        let properties = schema["properties"].as_object().unwrap();
        for (name, _) in EXPECTED_PROPERTIES {
            let prop = &properties[*name];
            assert!(
                prop.get("default").is_some(),
                "property `{name}` must document a default value"
            );
        }
    }

    #[test]
    fn property_defaults_match_the_specification() {
        let schema = parse_schema();
        let properties = schema["properties"].as_object().unwrap();

        assert_eq!(properties["lineWidth"]["default"].as_u64(), Some(80));
        assert_eq!(properties["fluidWrapping"]["default"].as_bool(), Some(true));
        assert_eq!(
            properties["attributeSorting"]["default"].as_bool(),
            Some(false)
        );
        assert_eq!(
            properties["attributeOrderScheme"]["default"].as_str(),
            Some("standard")
        );
        assert_eq!(properties["vue"]["default"].as_bool(), Some(false));
        assert_eq!(properties["astro"]["default"].as_bool(), Some(false));
        assert_eq!(properties["svelte"]["default"].as_bool(), Some(false));
    }

    #[test]
    fn line_width_declares_its_allowed_range() {
        let schema = parse_schema();
        let line_width = &schema["properties"]["lineWidth"];
        assert_eq!(
            line_width["minimum"].as_u64(),
            Some(1),
            "lineWidth minimum must be 1"
        );
        assert_eq!(
            line_width["maximum"].as_u64(),
            Some(1000),
            "lineWidth maximum must be 1000"
        );
    }

    #[test]
    fn attribute_order_scheme_declares_its_allowed_values() {
        let schema = parse_schema();
        let order_scheme = &schema["properties"]["attributeOrderScheme"];
        let allowed = order_scheme["enum"]
            .as_array()
            .expect("attributeOrderScheme must declare an `enum` of allowed values");
        let values: Vec<&str> = allowed.iter().filter_map(Value::as_str).collect();
        assert_eq!(
            values,
            vec!["standard"],
            "the only allowed order scheme is `standard`"
        );
    }
}

/// Property-based tests for configuration diagnostics and default fallback.
///
/// These validate Property 17 from the design: for all resolved configurations,
/// the plugin emits exactly one diagnostic per unrecognized property and exactly
/// one per known property whose value has an invalid type or is out of range,
/// every offending property falls back to its default, and every valid property
/// retains its given value (Requirements 6.5, 6.6, 6.7).
///
/// Gated on the `wasm` feature because resolution depends on the dprint
/// `plugins`/`configuration` types that only exist with that feature.
#[cfg(all(test, feature = "wasm"))]
mod diagnostics_property_tests {
    use super::*;
    use dprint_core::configuration::{ConfigKeyMap, ConfigKeyValue, GlobalConfiguration};
    use proptest::prelude::*;

    /// A global configuration with explicit width/indent so fallbacks are
    /// observable (the `lineWidth` default is the global line width, 120 here).
    const GLOBAL_LINE_WIDTH: u32 = 120;

    fn global() -> GlobalConfiguration {
        GlobalConfiguration {
            line_width: Some(GLOBAL_LINE_WIDTH),
            use_tabs: Some(false),
            indent_width: Some(2),
            new_line_kind: None,
        }
    }

    /// The seven known plugin configuration properties. `indentWidth`/`useTabs`
    /// are intentionally absent: they are sourced from the global configuration,
    /// not the plugin config map.
    const KNOWN_KEYS: &[&str] = &[
        "lineWidth",
        "fluidWrapping",
        "attributeSorting",
        "attributeOrderScheme",
        "vue",
        "astro",
        "svelte",
    ];

    /// How a single known property appears in a generated config map.
    #[derive(Clone, Debug)]
    enum KnownSlot {
        /// The property is omitted entirely.
        Absent,
        /// The property is present with a valid value.
        Valid(ConfigKeyValue),
        /// The property is present with an invalid type or out-of-range value.
        Invalid(ConfigKeyValue),
    }

    /// A valid value generator for a given known key.
    fn valid_value(key: &str) -> BoxedStrategy<ConfigKeyValue> {
        match key {
            // Any integer within the accepted 1..=1000 range.
            "lineWidth" => (1i32..=1000)
                .prop_map(ConfigKeyValue::from_i32)
                .boxed(),
            // The only recognized scheme value.
            "attributeOrderScheme" => Just(ConfigKeyValue::from_str("standard")).boxed(),
            // Every remaining known key is a boolean flag.
            _ => any::<bool>().prop_map(ConfigKeyValue::from_bool).boxed(),
        }
    }

    /// An invalid value generator for a given known key: either the wrong type
    /// or, for `lineWidth`, an out-of-range integer.
    fn invalid_value(key: &str) -> BoxedStrategy<ConfigKeyValue> {
        match key {
            "lineWidth" => prop_oneof![
                // Out-of-range integers (both below and above the bounds).
                prop_oneof![(-1000i32..=0), (1001i32..=100_000)]
                    .prop_map(ConfigKeyValue::from_i32),
                // Wrong type entirely.
                Just(ConfigKeyValue::from_bool(true)),
                Just(ConfigKeyValue::from_str("wide")),
            ]
            .boxed(),
            "attributeOrderScheme" => prop_oneof![
                // Unrecognized scheme name.
                "[a-z]{1,8}"
                    .prop_filter("must not be a known scheme", |s| s != "standard")
                    .prop_map(|s| ConfigKeyValue::from_str(&s)),
                // Wrong type entirely.
                Just(ConfigKeyValue::from_bool(false)),
                Just(ConfigKeyValue::from_i32(7)),
            ]
            .boxed(),
            // Boolean flags: any non-boolean value is invalid.
            _ => prop_oneof![
                Just(ConfigKeyValue::from_i32(1)),
                Just(ConfigKeyValue::from_str("true")),
            ]
            .boxed(),
        }
    }

    /// Strategy for a single known property's slot (absent / valid / invalid).
    fn known_slot(key: &'static str) -> BoxedStrategy<KnownSlot> {
        prop_oneof![
            1 => Just(KnownSlot::Absent),
            2 => valid_value(key).prop_map(KnownSlot::Valid),
            2 => invalid_value(key).prop_map(KnownSlot::Invalid),
        ]
        .boxed()
    }

    /// Strategy for a set of unknown property names, each distinct from every
    /// known key and from each other.
    fn unknown_keys() -> impl Strategy<Value = Vec<String>> {
        prop::collection::hash_set("[a-z]{3,10}", 0..5).prop_map(|set| {
            set.into_iter()
                .filter(|k| !KNOWN_KEYS.contains(&k.as_str()))
                .collect()
        })
    }

    /// Strategy producing one slot per known key, in `KNOWN_KEYS` order, so
    /// every known property is always represented (absent, valid, or invalid).
    fn all_known_slots() -> impl Strategy<Value = Vec<KnownSlot>> {
        let per_key: Vec<BoxedStrategy<KnownSlot>> =
            KNOWN_KEYS.iter().map(|k| known_slot(k)).collect();
        per_key
    }

    proptest! {
        /// Property 17: diagnostics are one-per-offense and fallbacks/retention
        /// behave exactly as specified.
        ///
        /// **Validates: Requirements 6.5, 6.6, 6.7**
        #[test]
        fn diagnostics_are_one_per_offense_with_fallback_and_retention(
            slots in all_known_slots(),
            unknowns in unknown_keys(),
        ) {
            let mut map = ConfigKeyMap::new();
            // Track expectations keyed by known property name.
            let mut invalid_known: Vec<&'static str> = Vec::new();
            let mut valid_known: Vec<(&'static str, ConfigKeyValue)> = Vec::new();

            for (idx, key) in KNOWN_KEYS.iter().enumerate() {
                // `slots` has exactly one entry per known key, in order.
                let slot = &slots[idx];
                match slot {
                    KnownSlot::Absent => {}
                    KnownSlot::Valid(v) => {
                        map.insert((*key).to_string(), v.clone());
                        valid_known.push((key, v.clone()));
                    }
                    KnownSlot::Invalid(v) => {
                        map.insert((*key).to_string(), v.clone());
                        invalid_known.push(key);
                    }
                }
            }

            // Insert the unknown properties with arbitrary values.
            for (i, name) in unknowns.iter().enumerate() {
                // `unknown_keys` already filters out known names.
                map.insert(name.clone(), ConfigKeyValue::from_i32(i32::try_from(i).unwrap()));
            }

            let result = resolve_config(map, &global());
            let diagnostics = &result.diagnostics;
            let config = &result.config;
            let defaults = Configuration::default();
            let line_width_default = GLOBAL_LINE_WIDTH;

            // (6.5) Exactly one diagnostic per unrecognized property, each
            // naming that property.
            for name in &unknowns {
                let count = diagnostics
                    .iter()
                    .filter(|d| &d.property_name == name)
                    .count();
                prop_assert_eq!(
                    count, 1,
                    "expected exactly one diagnostic for unknown property '{}'",
                    name
                );
            }

            // (6.6) Exactly one diagnostic per invalid-typed/out-of-range known
            // property, each naming that property.
            for key in &invalid_known {
                let count = diagnostics
                    .iter()
                    .filter(|d| d.property_name == **key)
                    .count();
                prop_assert_eq!(
                    count, 1,
                    "expected exactly one diagnostic for invalid property '{}'",
                    key
                );
            }

            // No diagnostics for any valid known property.
            for (key, _) in &valid_known {
                let count = diagnostics
                    .iter()
                    .filter(|d| d.property_name == **key)
                    .count();
                prop_assert_eq!(
                    count, 0,
                    "valid property '{}' must not produce a diagnostic",
                    key
                );
            }

            // The total diagnostic count is exactly the number of offenses: one
            // per unknown property plus one per invalid known property. This
            // rules out any extra or missing diagnostics.
            prop_assert_eq!(
                diagnostics.len(),
                unknowns.len() + invalid_known.len(),
                "total diagnostics must equal unknown + invalid offenses"
            );

            // (6.7) Every offending known property fell back to its default.
            for key in &invalid_known {
                assert_fallback_to_default(key, config, &defaults, line_width_default)?;
            }

            // (6.7) Every valid known property retained its given value.
            for (key, value) in &valid_known {
                assert_retained_value(key, value, config)?;
            }
        }
    }

    /// Assert that `key` in `config` equals its default value. For `lineWidth`
    /// the default is the global line width used during resolution.
    fn assert_fallback_to_default(
        key: &str,
        config: &Configuration,
        defaults: &Configuration,
        line_width_default: u32,
    ) -> Result<(), TestCaseError> {
        match key {
            "lineWidth" => prop_assert_eq!(config.line_width, line_width_default),
            "fluidWrapping" => prop_assert_eq!(config.fluid_wrapping, defaults.fluid_wrapping),
            "attributeSorting" => {
                prop_assert_eq!(config.attribute_sorting, defaults.attribute_sorting);
            }
            "attributeOrderScheme" => {
                prop_assert_eq!(config.attribute_order_scheme, defaults.attribute_order_scheme);
            }
            "vue" => prop_assert_eq!(config.vue, defaults.vue),
            "astro" => prop_assert_eq!(config.astro, defaults.astro),
            "svelte" => prop_assert_eq!(config.svelte, defaults.svelte),
            other => prop_assert!(false, "unexpected known key '{}'", other),
        }
        Ok(())
    }

    /// Assert that `key` in `config` retained the given valid `value`.
    fn assert_retained_value(
        key: &str,
        value: &ConfigKeyValue,
        config: &Configuration,
    ) -> Result<(), TestCaseError> {
        match (key, value) {
            ("lineWidth", ConfigKeyValue::Number(n)) => {
                prop_assert_eq!(i64::from(config.line_width), i64::from(*n));
            }
            ("fluidWrapping", ConfigKeyValue::Bool(b)) => {
                prop_assert_eq!(config.fluid_wrapping, *b);
            }
            ("attributeSorting", ConfigKeyValue::Bool(b)) => {
                prop_assert_eq!(config.attribute_sorting, *b);
            }
            ("attributeOrderScheme", ConfigKeyValue::String(_)) => {
                // The only valid scheme string is "standard".
                prop_assert_eq!(config.attribute_order_scheme, SchemeId::Standard);
            }
            ("vue", ConfigKeyValue::Bool(b)) => prop_assert_eq!(config.vue, *b),
            ("astro", ConfigKeyValue::Bool(b)) => prop_assert_eq!(config.astro, *b),
            ("svelte", ConfigKeyValue::Bool(b)) => prop_assert_eq!(config.svelte, *b),
            (other, _) => prop_assert!(false, "unexpected valid slot for key '{}'", other),
        }
        Ok(())
    }
}
