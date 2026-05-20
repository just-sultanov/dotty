use indexmap::IndexMap;

/// Rebuild the managed map from tracked files.
pub(crate) fn rebuild_managed_map(tracked_files: &[String]) -> IndexMap<String, String> {
    let mut managed = IndexMap::new();

    for file in tracked_files {
        let repo_path = std::path::PathBuf::from(file);
        if let Ok(target) = crate::paths::repo_to_target(&repo_path) {
            let target_str = crate::paths::format_target_display(&target);
            managed.insert(file.clone(), target_str);
        }
    }

    managed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rebuild_managed_map() {
        crate::tests::with_test_home(|home| {
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
