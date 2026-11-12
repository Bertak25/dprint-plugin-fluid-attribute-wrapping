---
inclusion: always
---

# Source of Truth: docs/specs/

The canonical specifications for this project live in `docs/specs/`, NOT in `.kiro/`. They are committed to source control and shared by every agent (Claude, Codex, Kiro) and human contributors.

When planning, refactoring, or writing code:

- Read `#[[file:docs/specs/requirements.md]]` for user stories and acceptance criteria.
- Read `#[[file:docs/specs/design.md]]` for architecture, interfaces, and design decisions.

The implementation task plan (`tasks.md`) stays local under `.kiro/specs/` and is not committed; `docs/specs/` holds the durable requirements and design.

**Keep them current.** `docs/specs/` is not write-once. At the END of any change, bugfix, or new spec — once the implementation and tests are green — reconcile `docs/specs/requirements.md` and `docs/specs/design.md` against what you actually built and update them so the documented behavior, architecture, and acceptance criteria match the code. A change is not complete until the specs reflect it. See the "Keeping the Specs Current" section in `AGENTS.md` for the full checklist.

If anything under `.kiro/specs/` disagrees with `docs/specs/`, treat `docs/specs/` as authoritative. The rest of `.kiro/` is local tool workspace state and is intentionally git-ignored.

See also `AGENTS.md` at the repository root for the full, tool-neutral agent guidelines.
