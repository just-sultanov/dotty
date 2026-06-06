# dotty

> **⚠️ Alpha:** This project is under active development. APIs and behavior may change.

[![CI](https://github.com/just-sultanov/dotty/actions/workflows/ci.yml/badge.svg)](https://github.com/just-sultanov/dotty/actions/workflows/ci.yml)

A minimal dotfiles manager for multiple machines.

## Features

- **Tier-based organization** — `base/`, `<platform>/`, `<machine>/` directories with priority override
- **File-level symlinks** — each managed file gets its own symlink, not whole-directory stow-style
- **No config files** — the repo structure IS the config
- **Crash-safe** — atomic operations with automatic rollback on failure
- **Backup verification** — SHA-256 integrity checks on all backups
- **Symlink safety** — circular detection, traversal prevention, read-only preservation
- **Cross-platform** — macOS, Linux, Windows (including directory junction support)

## Install

**Using cargo:**

```bash
cargo install dotty --locked
```

**Using mise:**

```bash
mise install github:just-sultanov/dotty
```

**Using curl (macOS / Linux):**

```bash
# Install to ~/.local/bin (default)
curl -fsSL https://raw.githubusercontent.com/just-sultanov/dotty/main/install.sh | bash

# Install to a custom directory
curl -fsSL https://raw.githubusercontent.com/just-sultanov/dotty/main/install.sh | bash -s -- --prefix /usr/local/bin
```

**Using PowerShell (Windows):**

```powershell
# Install to $env:USERPROFILE\.local\bin (default)
irm https://raw.githubusercontent.com/just-sultanov/dotty/main/install.ps1 | iex

# Install to a custom directory
irm https://raw.githubusercontent.com/just-sultanov/dotty/main/install.ps1 | iex -ArgumentList '-Prefix', 'C:\tools'
```

**Using pre-built binaries:**

Download the latest release from [GitHub Releases](https://github.com/just-sultanov/dotty/releases).

| Platform               | File                                     |
| ---------------------- | ---------------------------------------- |
| macOS (Apple Silicon)  | `dotty-aarch64-apple-darwin.tar.gz`      |
| macOS (Intel)          | `dotty-x86_64-apple-darwin.tar.gz`       |
| Linux (x86_64, static) | `dotty-x86_64-unknown-linux-musl.tar.gz` |
| Windows (x86_64)       | `dotty-x86_64-pc-windows-msvc.zip`       |

## Quick Start

```bash
# 1. Bootstrap a new dotty repository
dotty init --machine macbook

# 2. Add your first config file (added to base/ tier)
dotty add ~/.vimrc

# 3. Create symlinks for all tracked files
dotty apply

# 4. Check status
dotty status
```

To clone an existing dotty repository from GitHub:

```bash
dotty init git@github.com:user/dotfiles.git --machine macbook
```

The `--machine` flag sets the machine name for the current host.
To change it later or set it without reinitializing:

```bash
dotty config machine <name>
```

## How It Works

Config files live in a git repository organized by priority tiers:

```
~/.dotty/
├── base/                    # Shared across all machines
│   └── home/
│       ├── .config/nvim/init.lua
│       └── .vimrc
├── linux/                   # Linux-specific
│   └── home/
│       └── .config/kitty/kitty.conf
├── macbook/                 # Machine-specific: MacBook
│   └── home/
│       └── .config/nvim/init.lua   ← overrides base
├── macos/                   # macOS-specific
│   └── home/
│       └── .config/kitty/kitty.conf
├── windows/                 # Windows-specific
│   └── home/
│       └── .config/powershell/Microsoft.PowerShell_profile.ps1
└── work/                    # Machine-specific: work machine
    └── home/
        └── .gitconfig
```

Platform tiers (`linux`, `macos`, `windows`) are detected automatically.
Machine tiers (`work`, `macbook`) are set by the user via `dotty config machine <name>`.

`dotty apply --dry-run` previews all planned changes without modifying anything:

```
$ dotty apply --dry-run
[dry-run] symlink created - ~/.vimrc → ~/.dotty/macos/home/.vimrc

Overrides:
[dry-run] macos - ~/.vimrc

1 would be applied, 1 override, 0 skipped (unchanged)
```

The `Overrides:` block lists files that override lower-priority tiers
(in the example above, the `macos` platform tier replaces `base` for
`~/.vimrc`). The actual symlink action is shown in the regular
`[dry-run] <action>` line; the override block highlights which tier
wins. Run without `--dry-run` to actually apply — output looks similar
but adds a `done` line and drops the `[dry-run]` prefix:

```
$ dotty apply
✓ symlink created - ~/.vimrc → ~/.dotty/macos/home/.vimrc

Overrides:
macos - ~/.vimrc

done
1 applied, 1 override, 0 skipped (unchanged)
```

## Tier Priority

| Tier         | Priority | Scope                         |
| ------------ | -------- | ----------------------------- |
| `<machine>`  | Highest  | Single machine (e.g. macbook) |
| `<platform>` | Medium   | OS family (e.g. macos, linux) |
| `base`       | Lowest   | Shared across all machines    |

## Commands

| Command                                                                            | Description                                            |
| ---------------------------------------------------------------------------------- | ------------------------------------------------------ |
| `dotty init [<git_url>] [--machine <name>]`                                        | Bootstrap a new repo or clone an existing one          |
| `dotty add <path> [--machine <name>] [--platform <os>] [--commit <msg>] [--force]` | Add a file or directory to the repo                    |
| `dotty remove <path> [--machine <name>] [--platform <os>] [--commit <msg>]`        | Remove a file from management (restores original)      |
| `dotty apply [--dry-run] [--force] [--follow-symlinks] [--platform <os>]`          | Create symlinks for all tracked files                  |
| `dotty status`                                                                     | Show repo status, conflicts, broken links, backup size |
| `dotty clean [--keep <n>] [--before <date>] [-y]`                                  | Remove old backups                                     |
| `dotty config machine <name>`                                                      | Set the current machine name                           |

## Safety

- **Atomic writes** — config files are written to a temp file then renamed into place
- **Backup verification** — files > 1KB are SHA-256 verified after backup
- **Circular detection** — symlink chains are checked before creation (max 15 hops)
- **Rollback** — plan-based execution rolls back completed actions on any failure
- **Orphan detection** — `dotty apply` detects and removes managed files no longer in the repo
- **Symlink traversal prevention** — directory walkers skip symlinked directories

## Environment Variables

| Variable           | Default           | Description                                    |
| ------------------ | ----------------- | ---------------------------------------------- |
| `DOTTY_HOME`       | `~/.dotty`        | Repository path                                |
| `DOTTY_STATE_HOME` | platform-specific | State directory (config, backups)              |
| `XDG_STATE_HOME`   | `~/.local/state`  | Used on Linux when `DOTTY_STATE_HOME` is unset |

## Crash Recovery

If `dotty` is interrupted (SIGINT, power loss, etc.) during a multi-step operation,
a pending plan is saved to the state directory. On the next run, you'll be prompted
to rollback or continue.

Use `--recover` to skip the prompt, or `--recovery-action rollback|discard|ignore` for
non-interactive environments.

## Philosophy

Convention over configuration. No config files, no templates, no hooks.
The repo structure tells dotty what to do. Encryption is up to you — dotty doesn't encrypt anything.
Just use whatever tool you're already comfortable with
(e.g. git-crypt, GPG, SOPS) to protect sensitive files.

## License

MIT — see [LICENSE](LICENSE) for details.
