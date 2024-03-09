use std::path::PathBuf;

use auth_git2::GitAuthenticator;
use git2::Repository;

/// Creates a new repository in the specified directory.
pub fn init(path: PathBuf) {
    match Repository::init(path) {
        Ok(_) => println!("Repository has been initialized"),
        Err(e) => panic!("Failed to init: {}", e.message()),
    };
}

/// Clones a repository using the git authenticator.
pub fn clone(remote: String, path: PathBuf) {
    let auth = GitAuthenticator::default();
    match auth.clone_repo(remote, path) {
        Ok(_) => println!("Repository has been cloned"),
        Err(e) => panic!("Failed to clone: {}", e.message()),
    };
}
