//! Wasm export shell.
//!
//! This module hosts the `generate_plugin_code!` invocation that emits the dprint
//! Schema Version 4 exports. The invocation is wired in task 13 and compiled only
//! for the `wasm32-unknown-unknown` target under the `wasm` feature.
// `PluginHandler` is imported only where the macro invocation below actually
// names it, so the `use` carries the SAME triple gate (`wasm32`/`unknown` arch
// plus the `wasm` feature). Any looser gate would leave the import unused on a
// host `--features wasm` build (where the arch gate drops the macro), tripping
// the crate's `deny(warnings)` on `unused_imports`.
#[cfg(all(target_arch = "wasm32", target_os = "unknown", feature = "wasm"))]
use crate::handler::PluginHandler;
// `generate_plugin_code!` expands to a `SyncPluginHandler<Configuration>`-typed
// surface and names `Configuration` unqualified, so the config type must be in
// scope at the macro call site. It carries the SAME triple gate as the macro and
// the `PluginHandler` import above, so a host `--features wasm` build (where the
// arch gate drops the macro) does not see an unused import and trip
// `deny(warnings)`.
#[cfg(all(target_arch = "wasm32", target_os = "unknown", feature = "wasm"))]
use crate::config::Configuration;

// Emit the dprint Schema Version 4 Wasm exports (`dprint_plugin_version_4` and
// friends) by handing the `generate_plugin_code!` macro the stateless
// [`PluginHandler`] and its `Default` constructor.
//
// The invocation is doubly gated:
// - `target_arch = "wasm32", target_os = "unknown"` so the exports are only
//   compiled for the actual plugin target and never pollute host builds/tests;
// - `feature = "wasm"` so the macro — and the `dprint-core` plugin machinery it
//   expands into — is only pulled in when that feature is enabled (the same
//   feature that gates `PluginHandler` and `engine::format_document`).
//
// Keeping both gates in lockstep means a plain `cargo build`/`cargo test` (no
// `wasm`) compiles this module to nothing, and even `--features wasm` on a host
// target stays inert because the `wasm32-unknown-unknown` arch gate is unmet.
//
// The invocation lives in a gated inner module (`exports`) so its one
// documented, minimal lint relaxation is itself gated by the SAME triple cfg as
// the macro. `clippy::default_constructed_unit_structs` fires on
// `PluginHandler::default()` only when the macro is compiled for the real wasm
// target (where `PluginHandler` is a unit struct); we keep the explicit
// `::default()` form the macro contract documents — the stateless `Default`
// constructor the design names, which stays correct if the handler ever gains
// fields. Scoping the allow to this gated module means it is never an unused
// attribute on a host build (where the whole module is cfg'd out), so it does
// not re-trip `deny(warnings)` via `unused_attributes`.
#[cfg(all(target_arch = "wasm32", target_os = "unknown", feature = "wasm"))]
mod exports {
    #![allow(clippy::default_constructed_unit_structs)]

    use super::{Configuration, PluginHandler};

    dprint_core::generate_plugin_code!(PluginHandler, PluginHandler::default());
}
