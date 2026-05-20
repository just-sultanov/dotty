use indexmap::IndexMap;

/// Rebuild the managed map from tracked files.
pub(crate) fn rebuild_managed_map(tracked_files: &[String]) -> IndexMap<String, String> {
    let mut managed = IndexMap::new();

    for file in tracked_files {
        let repo_path = std::path::PathBuf::from(file);
        if let Ok(target) = crate::convention::repo_to_target(&repo_path) {
            let target_str = crate::convention::format_target_display(&target);
            managed.insert(file.clone(), target_str);
        }
    }

    managed
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn with_test_home<F: FnOnce(&PathBuf)>(test: F)
    where
        F: FnOnce(&PathBuf),
    {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            test(&home);
        });
    }

    #[test]
    fn test_rebuild_managed_map() {
        with_test_home(|home| {
            let files = vec!["base/home/.vimrc".into(), "base/home/.gitconfig".into()];
            let managed = rebuild_managed_map(&files);

            assert_eq!(managed.len(), 2);
            assert!(managed.contains_key("base/home/.vimrc"));
            assert!(managed.contains_key("base/home/.gitconfig"));
            assert!(managed.get("base/home/.vimrc").unwrap().starts_with("~"));
            let _ = home;
        });
    }
}
