# dotty

> **⚠️ Alpha:** This project is under active development. APIs and behavior may change.

## Concept

A minimal dotfiles manager for multiple machines. Config files live in a git repository organized
by priority tiers — `base/`, `<platform>/`, `<machine>/` - and are linked to their real locations via file-level symlinks.

**How it works:** `dotty add ~/.vimrc` copies the file into the repo and creates a symlink in its place.
`dotty apply` resolves all tiers, merges them by priority (machine overrides platform overrides base), and creates symlinks.
Higher-priority files simply replace lower-priority symlinks for the same target path.

**Philosophy:** convention over configuration. No config files, no templates, no hooks. The repo structure tells dotty what to do.
Encryption is handled by git-crypt, not dotty.

## License

MIT — see [LICENSE](LICENSE) for details.
