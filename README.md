# dprint-plugin-fluid-attribute-wrapping

A [dprint](https://dprint.dev/) plugin that applies **fluid (greedy) attribute wrapping** and optional **attribute sorting** to markup element start tags, while preserving everything else byte-for-byte.

It runs as an **additive post-processing pass**: it claims `.html`, `.jsx`, and `.tsx` files (plus `.vue`, `.astro`, and `.svelte` when enabled) and rewrites only the internals of start tags. Text between tags, end tags, comments, and raw-text/opaque regions are left untouched.

## Features

- **Fluid wrapping** - greedily packs as many attributes as fit within the line width, then wraps the rest onto canonically-indented continuation lines.
- **Attribute sorting** (opt-in) - orders attributes by a built-in `standard` scheme, preserving the relative order of behavior-affecting attributes.
- **Structure-preserving** - output is verified structurally equivalent to the input (same nesting, same attribute name/value pairs, same text content).
- **Idempotent** - formatting already-formatted input is a no-op; a pass either reaches a fixed point or returns an error rather than emitting non-idempotent output.
- **Multi-dialect** - HTML, JSX, TSX always; Vue, Astro, and Svelte behind config flags.

## Installation

Add the plugin to your `dprint.json`:

```json
{
  "plugins": [
    "https://github.com/Bertak25/dprint-plugin-fluid-attribute-wrapping/releases/download/v0.1.0/dprint_plugin_fluid_attribute_wrapping.wasm"
  ]
}
```

Replace `v0.1.0` with the release you want to use. Each [GitHub Release](https://github.com/Bertak25/dprint-plugin-fluid-attribute-wrapping/releases) publishes the built `.wasm` and its SHA-256 checksum; dprint lets you pin the plugin by appending `@<checksum>` to the URL:

```json
"https://github.com/Bertak25/dprint-plugin-fluid-attribute-wrapping/releases/download/v0.1.0/dprint_plugin_fluid_attribute_wrapping.wasm@<sha256>"
```

## Configuration

All keys live under the `fluidAttributeWrapping` config key:

```json
{
  "fluidAttributeWrapping": {
    "lineWidth": 80,
    "fluidWrapping": true,
    "attributeSorting": false,
    "attributeOrderScheme": "standard",
    "vue": false,
    "astro": false,
    "svelte": false
  }
}
```

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `lineWidth` | integer (1-1000) | dprint global `lineWidth`, else `80` | Maximum line width in columns. |
| `fluidWrapping` | boolean | `true` | Enable fluid (greedy) attribute wrapping. |
| `attributeSorting` | boolean | `false` | Enable attribute sorting. |
| `attributeOrderScheme` | `"standard"` | `"standard"` | Order scheme applied when sorting is enabled. |
| `vue` | boolean | `false` | Enable the Vue dialect (`.vue` files). |
| `astro` | boolean | `false` | Enable the Astro dialect (`.astro` files). |
| `svelte` | boolean | `false` | Enable the Svelte dialect (`.svelte` files). |

`indentWidth` and `useTabs` are taken from dprint global configuration rather than declared here. Invalid values produce a configuration diagnostic and fall back to the default while retaining every valid value.

The JSON schema for these options lives in [`schema.json`](./schema.json).

## Building

The plugin targets `wasm32-unknown-unknown` (pinned in `rust-toolchain.toml`) and is built behind the `wasm` feature:

```bash
# Build the Wasm module and verify it exports dprint_plugin_version_4
bash scripts/build-wasm.sh
```

This produces a single artifact at `target/wasm32-unknown-unknown/release/dprint_plugin_fluid_attribute_wrapping.wasm` and runs an export-section check via `scripts/check_wasm_exports.py`.

To build manually:

```bash
cargo build --release --target=wasm32-unknown-unknown --features wasm
```

## Development

The crate is organized as a pure, host-testable core wrapped in a thin Wasm export shell, so the test suite runs on the host without a Wasm runtime.

```bash
# Run the full test suite (unit + integration + property + corpora + perf)
cargo test --features wasm

# Lint (pedantic clippy, warnings denied) - both configurations
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features wasm -- -D warnings
```

The suite includes property-based tests (proptest), per-dialect idempotence and structure-equivalence corpora, an end-to-end integration test through the public `format_document` seam, and an advisory performance guard.

## Releasing

Releases are automated by [`.github/workflows/release.yml`](./.github/workflows/release.yml). Pushing a version tag (`v*`) builds, verifies, and publishes a GitHub Release with the `.wasm` attached:

```bash
# 1. Bump `version` in Cargo.toml to match the tag you will push.
# 2. Commit everything to master and push.
git push origin master

# 3. Tag the release commit and push the tag — this triggers the workflow.
git tag v0.1.0
git push origin v0.1.0
```

The workflow checks that the tag matches the `Cargo.toml` version, runs the full test + clippy gate, builds the Wasm module via `scripts/build-wasm.sh`, computes its SHA-256, and creates the release. The published asset is downloadable at:

```
https://github.com/Bertak25/dprint-plugin-fluid-attribute-wrapping/releases/download/<tag>/dprint_plugin_fluid_attribute_wrapping.wasm
```

## Project layout

| Path | Purpose |
| --- | --- |
| `src/config.rs` | Configuration types, resolution, diagnostics, JSON schema wiring |
| `src/dialect.rs` | Extension-to-dialect resolution and scanner profiles |
| `src/scanner.rs` | Start-tag scanner and attribute tokenizer |
| `src/sort.rs` | Attribute sorting with the standard scheme |
| `src/wrap.rs` | Greedy packer / fluid wrapping |
| `src/render.rs` | Canonical start-tag rendering |
| `src/verify.rs` | Structure-equivalence and idempotence fingerprints |
| `src/engine.rs` | Orchestration pipeline (format_once, format_document) |
| `src/handler.rs` | dprint SyncPluginHandler implementation |
| `src/wasm_plugin.rs` | Wasm export shell (generate_plugin_code!) |

## License

[MIT](./LICENSE)
