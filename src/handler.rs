//! The dprint `SyncPluginHandler` implementation.
//!
//! [`PluginHandler`] is the thin, Wasm-facing shell over the pure core. It
//! declares the plugin's identity and configuration key, embeds the license,
//! resolves configuration through [`crate::config`], and delegates every format
//! request to the pure [`crate::engine::format_document`] entry point. It holds
//! no state of its own — `#[derive(Default)]` is sufficient, and the
//! `generate_plugin_code!` macro constructs it with `PluginHandler::default()`.
//!
//! The entire module is gated on the `wasm` feature because the
//! `SyncPluginHandler` trait, the `SyncFormatRequest`/`SyncHostFormatRequest`
//! request types, `PluginInfo`, `FormatResult`, and the `FormatError` type it
//! returns all live behind `dprint-core`'s `wasm` feature. In a plain host
//! build (`cargo build`, no `wasm`) the module compiles to nothing, which keeps
//! the non-wasm build and its pedantic-deny clippy pass clean.

#![cfg(feature = "wasm")]

use dprint_core::configuration::{ConfigKeyMap, GlobalConfiguration};
use dprint_core::plugins::{
    CheckConfigUpdatesMessage, ConfigChange, FormatError, FormatResult, PluginInfo,
    PluginResolveConfigurationResult, SyncFormatRequest, SyncHostFormatRequest, SyncPluginHandler,
};

use crate::config::{resolve_config, Configuration};
use crate::engine::format_document;

/// The plugin's help URL, surfaced to users by the dprint CLI. Must be an
/// `https://` URL (Requirement 1.2).
const HELP_URL: &str = "https://github.com/Bertak25/dprint-plugin-fluid-attribute-wrapping";

/// The published JSON configuration schema URL. Must be an `https://` URL
/// (Requirement 1.2).
const CONFIG_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/Bertak25/dprint-plugin-fluid-attribute-wrapping/master/schema.json";

/// The `SyncPluginHandler` implementation for the Fluid Attribute Wrapping
/// Plugin.
///
/// Stateless: it carries no fields, so the derived `Default` is the sole
/// constructor and is what `generate_plugin_code!(PluginHandler,
/// PluginHandler::default())` uses (task 13).
#[derive(Default)]
pub struct PluginHandler;

impl SyncPluginHandler<Configuration> for PluginHandler {
    /// Resolve the plugin configuration by delegating to [`crate::config`],
    /// which reads the known keys, emits diagnostics, applies defaults, and
    /// builds the additive [`dprint_core::plugins::FileMatchingInfo`]
    /// (Requirements 1.4, 2.1, 6).
    fn resolve_config(
        &mut self,
        config: ConfigKeyMap,
        global_config: &GlobalConfiguration,
    ) -> PluginResolveConfigurationResult<Configuration> {
        resolve_config(config, global_config)
    }

    /// Report the plugin's identity (Requirement 1.2).
    ///
    /// The name and version come from the crate manifest via `env!`, so they
    /// are always a non-empty name and a `MAJOR.MINOR.PATCH` version. The
    /// config key is the published `"fluidAttributeWrapping"`, and both URLs are
    /// `https://`. There is no auto-update feed, so `update_url` is `None`.
    fn plugin_info(&mut self) -> PluginInfo {
        PluginInfo {
            name: env!("CARGO_PKG_NAME").to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            config_key: "fluidAttributeWrapping".to_string(),
            help_url: HELP_URL.to_string(),
            config_schema_url: CONFIG_SCHEMA_URL.to_string(),
            update_url: None,
        }
    }

    /// Return the embedded license text (Requirement 1.3).
    ///
    /// The `LICENSE` file is embedded at compile time with `include_str!` and is
    /// guaranteed non-empty from the first build (it is seeded with license
    /// text), so the returned string is always non-empty.
    fn license_text(&mut self) -> String {
        include_str!("../LICENSE").to_string()
    }

    /// Return the set of configuration migrations to apply after a plugin
    /// upgrade.
    ///
    /// There are no migrations today, so this returns an empty `Vec`. This is
    /// the hook where future `Attribute_Order_Scheme` migrations — for example
    /// renaming or splitting an order scheme — would be emitted as
    /// [`ConfigChange`]s.
    fn check_config_updates(
        &self,
        _message: CheckConfigUpdatesMessage,
    ) -> Result<Vec<ConfigChange>, FormatError> {
        Ok(Vec::new())
    }

    /// Format a file by delegating to the pure engine (Requirements 2.2, 2.4,
    /// 7.1).
    ///
    /// Range validation, UTF-8 decoding, extension/dialect gating, and the
    /// structure-equivalence and idempotence self-checks all live in
    /// [`crate::engine::format_document`], which returns `Ok(None)` for no
    /// change, `Ok(Some(bytes))` for a change, or a [`FormatError`] on a hard
    /// failure. That is exactly dprint's [`FormatResult`], so the result is
    /// returned directly.
    ///
    /// `format_with_host` is unused: this plugin never re-dispatches to other
    /// plugins — it only post-processes the bytes it is handed.
    fn format(
        &mut self,
        request: SyncFormatRequest<Configuration>,
        _format_with_host: impl FnMut(SyncHostFormatRequest) -> FormatResult,
    ) -> FormatResult {
        format_document(
            request.file_path,
            &request.file_bytes,
            request.range,
            request.config,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dprint_core::configuration::{ConfigKeyMap, GlobalConfiguration};
    use dprint_core::plugins::{FormatConfigId, NullCancellationToken, SyncFormatRequest};
    use std::path::Path;

    /// A `GlobalConfiguration` with explicit, benign width/indent values.
    fn global() -> GlobalConfiguration {
        GlobalConfiguration {
            line_width: Some(80),
            use_tabs: Some(false),
            indent_width: Some(2),
            new_line_kind: None,
        }
    }

    #[test]
    fn plugin_info_fields_are_well_formed() {
        let info = PluginHandler.plugin_info();

        // Non-empty name (Requirement 1.2).
        assert!(!info.name.is_empty(), "plugin name must be non-empty");

        // MAJOR.MINOR.PATCH version: three dot-separated numeric components.
        let parts: Vec<&str> = info.version.split('.').collect();
        assert_eq!(parts.len(), 3, "version must be MAJOR.MINOR.PATCH: {}", info.version);
        for part in parts {
            assert!(
                !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()),
                "each version component must be numeric: {}",
                info.version
            );
        }

        // The published config key.
        assert_eq!(info.config_key, "fluidAttributeWrapping");

        // Both URLs must be https:// (Requirement 1.2).
        assert!(info.help_url.starts_with("https://"), "help_url must be https");
        assert!(
            info.config_schema_url.starts_with("https://"),
            "config_schema_url must be https"
        );
    }

    #[test]
    fn license_text_is_non_empty() {
        // The embedded LICENSE is seeded non-empty, so the text is non-empty
        // (Requirement 1.3).
        let text = PluginHandler.license_text();
        assert!(!text.trim().is_empty(), "license text must be non-empty");
    }

    #[test]
    fn resolve_config_reports_additive_file_matching() {
        // The handler delegates to the config resolver, which marks the plugin
        // additive and lists the always-supported extensions (Requirements 1.4,
        // 2.1).
        let result = PluginHandler.resolve_config(ConfigKeyMap::new(), &global());
        assert!(result.file_matching.additive, "plugin must be additive");
        assert!(
            result.file_matching.file_extensions.contains(&"html".to_string()),
            "html must be a matched extension"
        );
    }

    #[test]
    fn check_config_updates_is_empty_today() {
        // No config migrations exist yet.
        let message = CheckConfigUpdatesMessage {
            old_version: None,
            config: ConfigKeyMap::new(),
        };
        let changes = PluginHandler
            .check_config_updates(message)
            .expect("no migrations should never error");
        assert!(changes.is_empty(), "there are no config migrations today");
    }

    #[test]
    fn format_delegates_to_engine_and_wraps() {
        // A wide tag under a narrow line width wraps, so `format` returns
        // `Ok(Some(..))` with changed bytes — proving the delegation to the
        // engine (Requirements 2.2, 2.4).
        let src = "<div class=\"container\" id=\"main\" data-role=\"banner\">x</div>";
        let config = Configuration {
            line_width: 24,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: crate::config::SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: false,
            astro: false,
            svelte: false,
        };
        let token = NullCancellationToken;
        let request = SyncFormatRequest {
            file_path: Path::new("file.html"),
            file_bytes: src.as_bytes().to_vec(),
            config_id: FormatConfigId::uninitialized(),
            config: &config,
            range: None,
            token: &token,
        };

        let result = PluginHandler
            .format(request, |_req| Ok(None))
            .expect("format should succeed");
        let out = result.expect("a wrap must produce changed bytes");
        assert_ne!(out, src.as_bytes(), "wrapped output must differ from input");
        assert!(
            String::from_utf8(out).unwrap().contains('\n'),
            "wrapped output should contain continuation lines"
        );
    }

    #[test]
    fn format_signals_no_change_for_fixed_point() {
        // A tag that already fits is a fixed point, so `format` returns
        // `Ok(None)` (Requirement 2.4).
        let src = "<a href=\"x\">text</a>";
        let config = Configuration::default();
        let token = NullCancellationToken;
        let request = SyncFormatRequest {
            file_path: Path::new("file.html"),
            file_bytes: src.as_bytes().to_vec(),
            config_id: FormatConfigId::uninitialized(),
            config: &config,
            range: None,
            token: &token,
        };

        let result = PluginHandler
            .format(request, |_req| Ok(None))
            .expect("format should succeed");
        assert_eq!(result, None, "a fixed-point input signals no change");
    }
}

#[cfg(test)]
mod handler_unit_tests {
    //! Additional task-12.2 coverage for the handler surface.
    //!
    //! These tests complement the existing `mod tests` by nailing down the
    //! exact `plugin_info` field values and URL shapes (Requirement 1.2), the
    //! embedded license text (Requirement 1.3), and the full set of
    //! always-matched extensions with additive mode (Requirement 2.1). They
    //! assert only facts the existing suite does not already lock down.

    use super::*;
    use dprint_core::configuration::{ConfigKeyMap, GlobalConfiguration};

    /// A `GlobalConfiguration` with explicit, benign width/indent values, so the
    /// config resolver never falls back on absent globals.
    fn global() -> GlobalConfiguration {
        GlobalConfiguration {
            line_width: Some(80),
            use_tabs: Some(false),
            indent_width: Some(2),
            new_line_kind: None,
        }
    }

    /// The config key is exactly the published `"fluidAttributeWrapping"`
    /// (Requirement 1.2). dprint keys resolved config under this exact string,
    /// so an inexact value would silently drop all user configuration.
    #[test]
    fn plugin_info_config_key_is_exact() {
        let info = PluginHandler.plugin_info();
        assert_eq!(
            info.config_key, "fluidAttributeWrapping",
            "config key must match the published value byte-for-byte"
        );
        assert!(!info.config_key.is_empty(), "config key must be non-empty");
    }

    /// Both surfaced URLs are absolute `https://` URLs with a non-empty host
    /// (Requirement 1.2 accepts `http://` or `https://`; this plugin publishes
    /// `https://`). Asserting a non-empty authority after the scheme guards
    /// against a bare scheme like `https://`.
    #[test]
    fn plugin_info_urls_are_absolute_https() {
        let info = PluginHandler.plugin_info();
        for (label, url) in [("help_url", &info.help_url), ("config_schema_url", &info.config_schema_url)] {
            let authority = url
                .strip_prefix("https://")
                .unwrap_or_else(|| panic!("{label} must be an absolute https URL: {url}"));
            // The authority (and the rest of the path) must be present, so the
            // URL is not a bare scheme.
            assert!(
                !authority.is_empty() && authority.contains('.'),
                "{label} must have a real host after the scheme: {url}"
            );
        }
    }

    /// The config schema URL points at a JSON schema document, matching the
    /// published `schema.json` the plugin ships (Requirement 1.2 / 6.8).
    #[test]
    fn plugin_info_schema_url_targets_a_json_schema() {
        let info = PluginHandler.plugin_info();
        assert!(
            info.config_schema_url.ends_with("schema.json"),
            "config_schema_url should point at the published schema.json: {}",
            info.config_schema_url
        );
    }

    /// The embedded license matches the on-disk `LICENSE` byte-for-byte and is
    /// non-empty (Requirement 1.3). This is stronger than the existing
    /// non-empty check: it proves the handler surfaces the real embedded file
    /// rather than any placeholder string.
    #[test]
    fn license_text_matches_embedded_file() {
        let text = PluginHandler.license_text();
        assert!(!text.trim().is_empty(), "license text must be non-empty");
        assert_eq!(
            text,
            include_str!("../LICENSE"),
            "license text must be the embedded LICENSE file verbatim"
        );
    }

    /// With the default configuration the plugin matches exactly the three
    /// always-supported extensions (`html`, `jsx`, `tsx`), lists no file names,
    /// and runs additive (Requirements 2.1, 1.4). The existing suite only
    /// checks `html` and `additive`; this pins the complete baseline set so a
    /// dropped or extra default extension is caught.
    #[test]
    fn resolve_config_default_matches_html_jsx_tsx_additive() {
        let result = PluginHandler.resolve_config(ConfigKeyMap::new(), &global());
        let fm = &result.file_matching;

        assert!(fm.additive, "plugin must report additive file matching");
        assert_eq!(
            fm.file_extensions,
            vec!["html".to_string(), "jsx".to_string(), "tsx".to_string()],
            "default matched extensions must be exactly html, jsx, tsx"
        );
        for ext in ["html", "jsx", "tsx"] {
            assert!(
                fm.file_extensions.contains(&ext.to_string()),
                "{ext} must be an always-matched extension"
            );
        }
        assert!(
            fm.file_names.is_empty(),
            "no files are matched by exact name, only by extension"
        );
    }
}
