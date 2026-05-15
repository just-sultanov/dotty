# Agents Guide

> Read this file before working on the project. All commands must be run through `mise`.

## Project

**dotty** — a minimal dotfiles manager for multiple machines. Config files live in a git repository organized by priority tiers (`base/`, `<platform>/`, `<machine>/`) and are linked to their real locations via file-level symlinks.

- **Language:** Rust (edition 2024)
- **Package manager:** mise (manages Rust toolchain, tasks, scripts)
- **Spec:** `readme.md`

## Rules

1. **Always run commands via `mise run <task>`** — never call `cargo`, `rustup`, or other tools directly.
2. **Never skip lint or tests.** After any code change, run `mise run check` to verify everything passes.
4. **Read `readme.md`** for the full spec before implementing features.

## Mise Tasks

### General

| Command                | Description                        |
| ---------------------- | ---------------------------------- |
| `mise run setup-tools` | Install required development tools |
| `mise run clean`       | Cleanup (remove build artifacts)   |

### Lint & Test

| Command                  | Description                                                                        |
| ------------------------ | ---------------------------------------------------------------------------------- |
| `mise run setup-linters` | Ensure lint tools (rustfmt, clippy) are available                                  |
| `mise run lint`          | Run formatters and linters (`cargo fmt -- --check`, `cargo clippy -- -D warnings`) |
| `mise run test`          | Run all tests (`cargo test -- --nocapture`)                                        |
| `mise run check`         | Run lint **and** tests (combined)                                                  |

### Build

| Command                    | Description                                         |
| -------------------------- | --------------------------------------------------- |
| `mise run build-linux`     | Build static release binary for Linux x86_64 (musl) |
| `mise run build-macos-arm` | Build release binary for macOS aarch64              |
| `mise run build-macos-x86` | Build release binary for macOS x86_64               |
| `mise run build-macos`     | Build release binary for macOS (aarch64 + x86_64)   |
| `mise run build-windows`   | Build release binary for Windows x86_64             |

### Distribution

| Command                   | Description                            |
| ------------------------- | -------------------------------------- |
| `mise run dist-linux`     | Build and package static Linux binary  |
| `mise run dist-macos-arm` | Build and package macOS aarch64 binary |
| `mise run dist-macos-x86` | Build and package macOS x86_64 binary  |
| `mise run dist-windows`   | Build and package Windows binary       |

### Publish

| Command                   | Description                                      |
| ------------------------- | ------------------------------------------------ |
| `mise run bump-version`   | Bump version in Cargo.toml (major\|minor\|patch) |
| `mise run pre-publish`    | Prepare a new version for release                |
| `mise run publish`        | Publish a new release                            |
| `mise run publish-crates` | Publish to crates.io                             |

## Quick Reference

```bash
# After any code change — verify everything
mise run check

# Run only lint
mise run lint

# Run only tests
mise run test

# Build for current platform
mise run build-macos-arm   # or build-macos-x86, build-linux
```
