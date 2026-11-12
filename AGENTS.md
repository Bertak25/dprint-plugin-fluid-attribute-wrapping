# AI Agent Guidelines

This repository uses explicit specification files to drive project context, architecture, and feature development. These specs are the single source of truth for every assistant - Claude, Codex, Kiro, or any other - as well as for human contributors.

## Core Directives

1. **Read Specifications First:** Before planning, refactoring, or writing code, review the specification files located in `docs/specs/`.
2. **Adhere to Design Rules:** Implement features strictly following the architecture, interfaces, and patterns detailed in `docs/specs/design.md`.
3. **Satisfy Requirements:** Ensure all proposed changes fulfill the functional criteria defined in `docs/specs/requirements.md`.
4. **Divergence Policy:** If existing codebase patterns conflict with the specification files, prioritize the design documents in `docs/specs/` or prompt the user for clarification before modifying code.

## Specification Structure

All documentation and system context are organized within `docs/specs/`:

* `docs/specs/requirements.md` - User stories, scope, and functional acceptance criteria (EARS format).
* `docs/specs/design.md` - Technical design, architecture, data models, module layout, and design decisions.

## Project Conventions

* Language/toolchain: Rust, targeting `wasm32-unknown-unknown` behind the `wasm` feature (see `rust-toolchain.toml`).
* Before considering a change complete, run: `cargo test --features wasm`, `cargo clippy --all-targets -- -D warnings`, and `cargo clippy --all-targets --features wasm -- -D warnings`.
* The crate must stay warning-free and clippy-clean (pedantic). See `docs/specs/design.md` for the full quality bar.
* See `README.md` for build, configuration, and usage details.

## Keeping the Specs Current (Required at End of Changes)

`docs/specs/requirements.md` and `docs/specs/design.md` are the durable, committed source of truth. They are not write-once: whenever a change, bugfix, or new spec alters observable behavior, architecture, interfaces, data models, configuration, or acceptance criteria, you MUST update `docs/specs/` as part of that same change so the specs never drift from the code.

Do this at the END of the work, once the implementation and tests are green:

1. **Reconcile before you finish.** Re-read `docs/specs/requirements.md` and `docs/specs/design.md` and compare them against what you actually built. If any statement is now inaccurate, incomplete, or missing, update it.
2. **Fold in behavioral changes.** New or changed behavior gets a new/updated EARS acceptance criterion in `requirements.md`; new or changed architecture, algorithms, interfaces, data models, or design decisions get folded into `design.md`.
3. **Record bugfix outcomes.** When a bugfix changes how the system behaves, update the affected requirement(s) and design section(s) so the fixed behavior is the documented behavior — do not leave the old, buggy description in place.
4. **Keep them authoritative and consistent.** `docs/specs/` must agree with the code and with itself. Local `.kiro/specs/` drafts are working state; when they disagree with `docs/specs/`, reconcile in favor of `docs/specs/`.
5. **Commit the spec updates alongside the code.** Treat a spec update as part of "done", the same way tests and clippy-cleanliness are part of done.

A change is not complete until `docs/specs/` reflects it.

## Note on Kiro

The `.kiro/` directory holds Kiro-specific working state (spec drafts, the implementation task plan, and task execution metadata) and is intentionally not tracked in source control. Kiro reads the same canonical specs in `docs/specs/` via a committed steering pointer (`.kiro/steering/specs.md`). Treat `docs/specs/` as authoritative; `.kiro/` is a local tool workspace.
