#!/usr/bin/env python3
"""Assert the built Wasm plugin exports the dprint Schema Version 4 entry point.

This is the automated export-inspection step for task 16.1 (Requirements 1.1,
1.6). It parses the WebAssembly module's export section (section id 7) directly
rather than grepping for a byte string, so the check is a true named-export
assertion: the name must appear as an entry in the export section, not merely
somewhere in the binary.

Usage:
    python3 scripts/check_wasm_exports.py [path/to/module.wasm]

If no path is given, the default release artifact location is used. Exits 0 when
`dprint_plugin_version_4` is exported, non-zero (with a diagnostic) otherwise.
"""

from __future__ import annotations

import sys

DEFAULT_WASM = (
    "target/wasm32-unknown-unknown/release/"
    "dprint_plugin_fluid_attribute_wrapping.wasm"
)
REQUIRED_EXPORT = "dprint_plugin_version_4"

# WebAssembly export "kind" byte -> human label (for diagnostics only).
KIND = {0: "func", 1: "table", 2: "memory", 3: "global"}


def read_uleb(data: bytes, pos: int) -> tuple[int, int]:
    """Decode an unsigned LEB128 integer; return (value, next_pos)."""
    result = 0
    shift = 0
    while True:
        byte = data[pos]
        pos += 1
        result |= (byte & 0x7F) << shift
        if not (byte & 0x80):
            return result, pos
        shift += 7


def parse_exports(data: bytes) -> list[tuple[str, int, int]]:
    """Walk the module's sections and return the export entries.

    Each entry is (name, kind_byte, index). Raises ValueError on a malformed
    header.
    """
    if data[0:4] != b"\x00asm":
        raise ValueError("not a WebAssembly module (bad magic number)")
    if data[4:8] != b"\x01\x00\x00\x00":
        raise ValueError("unsupported WebAssembly version")

    pos = 8
    exports: list[tuple[str, int, int]] = []
    while pos < len(data):
        section_id = data[pos]
        pos += 1
        size, pos = read_uleb(data, pos)
        body_end = pos + size
        if section_id == 7:  # Export section.
            cursor = pos
            count, cursor = read_uleb(data, cursor)
            for _ in range(count):
                name_len, cursor = read_uleb(data, cursor)
                name = data[cursor:cursor + name_len].decode("utf-8")
                cursor += name_len
                kind = data[cursor]
                cursor += 1
                _index, cursor = read_uleb(data, cursor)
                exports.append((name, kind, _index))
        pos = body_end
    return exports


def main(argv: list[str]) -> int:
    path = argv[1] if len(argv) > 1 else DEFAULT_WASM
    try:
        with open(path, "rb") as handle:
            data = handle.read()
    except OSError as err:
        print(f"error: cannot read Wasm artifact '{path}': {err}", file=sys.stderr)
        return 2

    try:
        exports = parse_exports(data)
    except (ValueError, IndexError) as err:
        print(f"error: failed to parse '{path}': {err}", file=sys.stderr)
        return 2

    names = {name for name, _kind, _idx in exports}
    if REQUIRED_EXPORT in names:
        print(
            f"ok: '{path}' exports '{REQUIRED_EXPORT}' "
            f"({len(exports)} exports total)"
        )
        return 0

    print(
        f"error: '{path}' does NOT export '{REQUIRED_EXPORT}'",
        file=sys.stderr,
    )
    print("  exports found:", file=sys.stderr)
    for name, kind, idx in exports:
        print(f"    - {name} ({KIND.get(kind, kind)} #{idx})", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
