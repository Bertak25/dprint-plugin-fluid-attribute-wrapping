# Claude Code Guidelines

This project keeps a single, tool-neutral source of truth for agent guidance in **[`AGENTS.md`](./AGENTS.md)**.

Please read `AGENTS.md` and follow it. In short:

- The canonical specifications live in `docs/specs/` (`requirements.md`, `design.md`). Read them before planning or writing code.
- Keep changes aligned with the design and requirements; if the codebase conflicts with the specs, prefer the specs or ask.
- Before considering a change complete, run `cargo test --features wasm` and clippy (`cargo clippy --all-targets -- -D warnings` and `--features wasm`). The crate must stay warning-free.
- Keep the specs current: at the end of any change, bugfix, or new spec, update `docs/specs/requirements.md` and `docs/specs/design.md` so they reflect the behavior, architecture, and acceptance criteria you actually shipped. A change is not done until the specs match the code (see the "Keeping the Specs Current" section in `AGENTS.md`).

This file exists so Claude Code picks up the shared guidance by its conventional name; the authoritative content is in `AGENTS.md` to avoid duplication and drift.
