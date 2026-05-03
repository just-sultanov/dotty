# Implementation Plan

> Based on readme.md spec. MVP scope only.

---

## Phase 0 — Skeleton

**Goal:** project compiles, `dotty --help` works, all subcommands exist as stubs.

| #   | Task                                  | File(s)                                                                                   | Notes                                                                        |
| --- | ------------------------------------- | ----------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| 0.1 | `cargo init`, `Cargo.toml`            | `Cargo.toml`                                                                              | deps: `clap`, `dialoguer`, `serde`, `toml` (for config.toml)                 |
| 0.2 | CLI definition — all commands + flags | `src/cli.rs`                                                                              | `clap` derive: `Init`, `Config`, `Add`, `Remove`, `Apply`, `Status`, `Clean` |
| 0.3 | `main.rs` — dispatch match            | `src/main.rs`                                                                             | match on subcommand → `todo!()`                                              |
| 0.4 | Empty module files                    | `src/commands/*.rs`, `src/convention.rs`, `src/git.rs`, `src/symlink.rs`, `src/prompt.rs` | compile-only stubs                                                           |

**Done when:** `cargo run -- --help` shows all commands and flags.

---

## Phase 1 — Core primitives

**Goal:** path resolution, state I/O, git subprocess, basic prompts work in isolation.

| #   | Task                                                                                           | File(s)                         | Notes                                                                                                              |
| --- | ---------------------------------------------------------------------------------------------- | ------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| 1.1 | Repo path resolution (`$DOTTY_HOME` or `~/.dotty`)                                             | `src/convention.rs`             | `resolve_repo_path()`                                                                                              |
| 1.2 | State path resolution (`$DOTTY_STATE_HOME` → `$XDG_STATE_HOME/dotty` → `~/.local/state/dotty`) | `src/convention.rs`             | `resolve_state_path()`                                                                                             |
| 1.3 | Platform detection (`uname -s` → map)                                                          | `src/convention.rs`             | `detect_platform()` → `Option<String>`                                                                             |
| 1.4 | Path mapping (`<scope>/home/*` → `~/*`, `<scope>/opt/*` → `/opt/*`)                            | `src/convention.rs`             | `repo_to_target()`, `target_to_repo()`                                                                             |
| 1.5 | config.toml read/write                                                                         | `src/convention.rs`             | `read_config()`, `write_config()` (machine + managed map: `HashMap<repo_path, target_path>`)                       |
| 1.6 | Git subprocess wrappers                                                                        | `src/git.rs`                    | `git_init()`, `git_clone()`, `git_add()`, `git_commit()`, `git_ls_files()`, `git_status()`, `git_current_branch()` |
| 1.7 | Interactive prompts                                                                            | `src/prompt.rs`                 | `prompt_confirm()`, `prompt_input()`, `prompt_select()`                                                            |
| 1.8 | Unit tests for convention                                                                      | `src/convention.rs` (mod tests) | path mapping, platform detection                                                                                   |

---

## Phase 2 — Plan-Execute engine

**Goal:** `Action` enum, plan builder, execute + rollback loop. No commands yet — just the engine.

| #   | Task                                        | File(s)                   | Notes                                                                    |
| --- | ------------------------------------------- | ------------------------- | ------------------------------------------------------------------------ |
| 2.1 | `Plan` struct + `Action` enum (8 variants)  | `src/plan.rs`             | `Plan { branch, command, actions }`, `Action { CreateDir, Backup, ... }` |
| 2.2 | `impl Display for Action`                   | `src/plan.rs`             | human-readable output                                                    |
| 2.3 | `impl Action::execute()`                    | `src/plan.rs`             | filesystem/git mutations                                                 |
| 2.4 | `impl Action::rollback() -> Option<Action>` | `src/plan.rs`             | inverse action                                                           |
| 2.5 | `execute_plan(plan, dry_run)`               | `src/plan.rs`             | loop: execute → on error rollback completed in reverse                   |
| 2.6 | Unit tests                                  | `src/plan.rs` (mod tests) | rollback symmetry, dry-run skips execution                               |

**Done**

---

## Phase 3 — `init` + `config`

**Goal:** bootstrap a repo, set machine name.

| #   | Task                           | File(s)                  | Notes                                                                 |
| --- | ------------------------------ | ------------------------ | --------------------------------------------------------------------- |
| 3.1 | `dotty init` — fresh repo      | `src/commands/init.rs`   | `git init`, create `base/home/`, create state dir, prompt/set machine |
| 3.2 | `dotty init <git-url>` — clone | `src/commands/init.rs`   | pre-check dir empty, `git clone`, scan known machines, prompt         |
| 3.3 | `dotty config machine <name>`  | `src/commands/config.rs` | write machine name to config.toml                                     |
| 3.4 | Integration tests              | `tests/init.rs`          | temp dir, verify `git init`, config.toml, `base/home/`                |

**Done**

---

## Phase 4 — `add`

**Goal:** add files to repo with plan-execute, conflict detection, symlinks.

| #   | Task                                                                                     | File(s)               | Notes                                                                   |
| --- | ---------------------------------------------------------------------------------------- | --------------------- | ----------------------------------------------------------------------- |
| 4.1 | Path resolution for `add`                                                                | `src/commands/add.rs` | `~/*` → `base/home/*`, `/*` → `base/<dir>/*`, `--platform`, `--machine` |
| 4.2 | Conflict detection                                                                       | `src/commands/add.rs` | scan all tiers for same target path                                     |
| 4.3 | Interactive conflict prompts                                                             | `src/commands/add.rs` | per-file or override all                                                |
| 4.4 | Build plan (Backup → CopyFile → CreateSymlink → GitAdd → GitCommit) + update managed map | `src/commands/add.rs` | `build_add_plan()`, insert `repo_path → target_path` into config.toml   |
| 4.5 | Directory recursion                                                                      | `src/commands/add.rs` | walk all files, build actions for each                                  |
| 4.6 | `--dry-run`                                                                              | `src/commands/add.rs` | pass flag to `execute_plan()`                                           |
| 4.7 | Edge cases: self-reference, non-XDG warn, machine dir creation                           | `src/commands/add.rs` | per spec table                                                          |
| 4.8 | Integration tests                                                                        | `tests/add.rs`        | add file, verify symlink, add dir, verify recursion                     |

**Done**

---

## Phase 5 — `apply`

**Goal:** resolve all tiers, merge by priority, create symlinks.

| #    | Task                                                                | File(s)                 | Notes                                                                                                                              |
| ---- | ------------------------------------------------------------------- | ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| 5.1  | `git ls-files` → collect tracked paths                              | `src/commands/apply.rs` |                                                                                                                                    |
| 5.2  | Read machine + detect platform                                      | `src/commands/apply.rs` | fallback if machine missing                                                                                                        |
| 5.3  | Collect paths by tier (base → platform → machine)                   | `src/commands/apply.rs` |                                                                                                                                    |
| 5.4  | Merge by target path (highest tier wins)                            | `src/commands/apply.rs` | `HashMap<TargetPath, (Tier, RepoPath)>`                                                                                            |
| 5.5  | Inspect target state (symlink correct/wrong, regular file, missing) | `src/commands/apply.rs` |                                                                                                                                    |
| 5.6  | Build plan (CreateDir → Backup → CreateSymlink)                     | `src/commands/apply.rs` | `build_apply_plan()`                                                                                                               |
| 5.7  | Orphan symlink detection via managed map                            | `src/commands/apply.rs` | keys in managed map but not in `git ls-files` → remove symlink + remove from managed; after apply, rebuild map from `git ls-files` |
| 5.8  | Console output with tier + override info                            | `src/commands/apply.rs` | summary line                                                                                                                       |
| 5.9  | `--dry-run`                                                         | `src/commands/apply.rs` |                                                                                                                                    |
| 5.10 | Integration tests                                                   | `tests/apply.rs`        | multi-tier override, orphan cleanup                                                                                                |

**Done when:** `dotty apply` resolves all tiers and creates correct symlinks.

---

## Phase 6 — `remove` + `status` + `clean`

**Goal:** remaining commands.

| #   | Task                     | File(s)                                                | Notes                                                                                                 |
| --- | ------------------------ | ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------- |
| 6.1 | `dotty remove <path>`    | `src/commands/remove.rs`                               | scan tiers, restore file from repo, remove from repo, remove entry from managed map                   |
| 6.2 | `dotty remove --dry-run` | `src/commands/remove.rs`                               |                                                                                                       |
| 6.3 | `dotty status`           | `src/commands/status.rs`                               | machine, platform, repo path, current branch, broken symlinks, backup size, git dirty, tier conflicts |
| 6.4 | `dotty clean`            | `src/commands/clean.rs`                                | `--keep`, `--before`, interactive confirm                                                             |
| 6.5 | Integration tests        | `tests/remove.rs`, `tests/status.rs`, `tests/clean.rs` |                                                                                                       |

**Done when:** all 7 commands work.

---

## Phase 7 — Polish

**Goal:** edge cases, error handling, UX.

| #   | Task                               | File(s)                 | Notes                                   |
| --- | ---------------------------------- | ----------------------- | --------------------------------------- |
| 7.1 | ASCII fallback for Unicode symbols | `src/plan.rs` (Display) | detect terminal capability              |
| 7.2 | Permission denied handling         | `src/plan.rs` (execute) | clear error messages                    |
| 7.3 | Circular symlink detection         | `src/plan.rs` (execute) |                                         |
| 7.4 | Error types + `thiserror`          | `src/`                  | custom error enum, `From` impls         |
| 7.5 | `--dry-run` exit code 0 everywhere | `src/plan.rs`           |                                         |
| 7.6 | Full integration test suite        | `tests/`                | happy path + edge cases from spec table |
| 7.7 | `cargo clippy`, `cargo fmt`        | —                       | lint clean                              |

**Done when:** spec edge cases covered, lint clean, tests pass.

---

## Dependencies between phases

```
Phase 0 (skeleton)
    ↓
Phase 1 (primitives) ──────────────────────────────────┐
    ↓                                                  │
Phase 2 (plan-execute engine) ─────────────────────────┤
    ↓                                                  │
Phase 3 (init + config) ── Phase 4 (add) ── Phase 5 (apply) ── Phase 6 (remove/status/clean)
    │                    │            │              │
    └────────────────────┴────────────┴──────────────┘
                     all depend on 1 + 2
    ↓
Phase 7 (polish)
```

Phases 3, 4, 5, 6 all depend on phases 1 and 2. Among themselves they are independent and can be developed in parallel (or any order). Phase 7 is last.

---

## Key design notes

- **`plan.rs`** is the heart — get this right first (Phase 2)
- **`convention.rs`** is pure functions — easy to test, no I/O
- **Commands** are thin — they build a plan and call `execute_plan()`
- **Persistence** (save plan to disk) — not in MVP, added later via `serde` derive on `Action`
- **Non-interactive mode** (`--yes`) — not in MVP, postponed per spec decision #13
