use std::{fs, path::PathBuf};

/// Converts the `PathBuf` into a [`String`](https://doc.rust-lang.org/stable/alloc/string/struct.String.html)
pub fn as_string(path: PathBuf) -> String {
    return path.into_os_string().into_string().unwrap();
}

/// Removes a directory at this path, after removing all its contents.
/// Use carefully!
/// This function does **not** follow symbolic links and it will simply remove the
/// symbolic link itself.
pub fn remove_dir_all(path: PathBuf) {
    let _ = fs::remove_dir_all(path);
}
