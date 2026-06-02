# Code Conventions

This document outlines the coding conventions used in the dotty project.

## Variable Naming Conventions

### General Rules

- Use full, descriptive names over abbreviations
- Use snake_case consistently (Rust convention)
- Use consistent prefixes/suffixes for related concepts
- Prefer clarity over brevity

### Path Variables

Use consistent naming for path-related variables:

| Variable Name        | Type    | Description                                                            |
| -------------------- | ------- | ---------------------------------------------------------------------- |
| `repo_relative_path` | String  | Path relative to repo root (e.g., `"base/home/.vimrc"`)                |
| `repo_absolute_path` | PathBuf | Absolute path to file in repo (e.g., `/path/to/repo/base/home/.vimrc`) |
| `target_path`        | PathBuf | Where symlink should point (absolute path)                             |
| `target_file`        | PathBuf | Same as `target_path`, the destination of the symlink                  |
| `link_path`          | PathBuf | Where symlink is located (same as target)                              |
| `backup_path`        | PathBuf | Where backup is stored                                                 |
| `state_path`         | PathBuf | Path to dotty state directory                                          |
| `repo_path`          | PathBuf | Path to dotty repository root                                          |
| `home`               | PathBuf | User's home directory                                                  |

### Tier Variables

| Variable Name | Type           | Description                                        |
| ------------- | -------------- | -------------------------------------------------- |
| `tier`        | String         | Tier name ("base", platform name, or machine name) |
| `machine`     | Option<String> | Machine name if specified, None otherwise          |
| `platform`    | Option<String> | Platform name if detected, None otherwise          |

### Other Variables

| Variable Name   | Type                                | Description                                          |
| --------------- | ----------------------------------- | ---------------------------------------------------- |
| `merged`        | IndexMap<PathBuf, (String, String)> | Merged tier map: target → (tier, repo_relative_path) |
| `override_map`  | IndexMap<PathBuf, String>           | Override map: target → lower tier name               |
| `tracked_files` | Vec<String>                         | List of repo-relative paths from git ls-files        |
| `managed_pairs` | Vec<(PathBuf, String)>              | Pairs of (target_path, repo_relative_path)           |

## Examples

```rust
// Before (inconsistent naming)
let repo_rel = action.repo_rel;
let repo_file = repo_path.join(&repo_rel);

// After (consistent naming)
let repo_relative_path = action.repo_relative_path;
let repo_absolute_path = repo_path.join(&repo_relative_path);
```

```rust
// Path variable usage
for (target_path, (tier, repo_relative_path)) in &input.merged {
    let repo_absolute_path = input.repo_path.join(repo_relative_path);
    let target = target_path.to_path_buf();

    // Use repo_absolute_path for file operations
    // Use target for symlink operations
}
```

## Migration Notes

- `repo_rel` → `repo_relative_path` (String)
- `repo_file` → `repo_absolute_path` (PathBuf)
- `target_rel` → `target_path` (PathBuf, when absolute)

## Related Documents

- See `readme.md` for the full project specification
- See `AGENTS.md` for development workflow guidelines
