//! Fluid Attribute Wrapping Plugin.
//!
//! A sandboxed WebAssembly plugin for [dprint](https://dprint.dev/) that performs a
//! post-processing pass over markup start tags: it applies fluid (greedy) attribute
//! wrapping and optional attribute sorting while preserving everything else
//! byte-for-byte.
//!
//! The crate is organized as a pure, host-testable core (every module below except
//! [`wasm_plugin`]) wrapped in a thin Wasm export shell. This keeps the test suite
//! fast and independent of the Wasm runtime.

// Crate-root lint policy (Requirement 9.2): compile warning-free under pedantic clippy.
#![deny(warnings)]
#![warn(clippy::all, clippy::pedantic)]
// Documented minimal allows. Each is a deliberate, scoped relaxation of a pedantic lint
// that would otherwise add noise without improving safety or clarity for this crate.
//
// `module_name_repetitions`: the layered design intentionally names types after their
// module (e.g. `handler::PluginHandler`), which reads clearly at call sites.
#![allow(clippy::module_name_repetitions)]
// `missing_errors_doc` / `missing_panics_doc`: error and panic conditions are described
// in the design document and module-level docs rather than per-function `# Errors`
// sections; the public surface is small and internal.
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

// ---------------------------------------------------------------------------
// Module tree. Each module is currently a stub; subsequent tasks flesh them out.
// ---------------------------------------------------------------------------

pub mod config;
pub mod dialect;
pub mod engine;
pub mod handler;
pub mod model;
pub mod render;
pub mod scanner;
pub mod sort;
pub mod verify;
pub mod wasm_plugin;
pub mod wrap;

// ---------------------------------------------------------------------------
// Shared-core re-export. Both the Wasm export shell (`handler` → the
// `generate_plugin_code!` machinery) and the host test suite drive the same
// `engine::format_document` entry point, so re-export it at the crate root for
// a single, stable public seam. It is gated on the `wasm` feature because
// `format_document` returns `dprint_core::plugins::FormatError`, which only
// exists under that feature (Requirements 1.1, 1.5, 1.6).
#[cfg(feature = "wasm")]
pub use engine::format_document;
