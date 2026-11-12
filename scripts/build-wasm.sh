#!/usr/bin/env bash
# Build the Fluid Attribute Wrapping Plugin as a dprint Wasm module and verify
# its exports (task 16.1; Requirements 1.1, 1.6, 9.2).
#
# Steps:
#   1. Build `--release --target=wasm32-unknown-unknown --features wasm`.
#   2. Assert exactly one `.wasm` artifact is produced.
#   3. Inspect that artifact's export section and assert it exports
#      `dprint_plugin_version_4` (the Schema Version 4 entry point).
#
# This script is the single source of truth for the Wasm packaging step and is
# what CI invokes. It is dependency-free beyond a Rust toolchain (with the
# pinned wasm32-unknown-unknown target) and python3 for the export inspection.
set -euo pipefail

TARGET="wasm32-unknown-unknown"
OUT_DIR="target/${TARGET}/release"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${REPO_ROOT}"

echo "==> Building Wasm module (release, --features wasm)"
cargo build --release --target="${TARGET}" --features wasm

echo "==> Locating the .wasm artifact"
shopt -s nullglob
artifacts=("${OUT_DIR}"/*.wasm)
shopt -u nullglob

if [ "${#artifacts[@]}" -eq 0 ]; then
    echo "error: no .wasm artifact found under ${OUT_DIR}" >&2
    exit 1
fi
if [ "${#artifacts[@]}" -ne 1 ]; then
    echo "error: expected exactly one .wasm artifact, found ${#artifacts[@]}:" >&2
    printf '    %s\n' "${artifacts[@]}" >&2
    exit 1
fi

WASM="${artifacts[0]}"
echo "    single artifact: ${WASM}"

echo "==> Inspecting exports"
python3 "${SCRIPT_DIR}/check_wasm_exports.py" "${WASM}"

echo "==> Wasm build and export check passed"
