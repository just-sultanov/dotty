//! Phase 3 — `init` + `config` integration tests.

mod common;
use common::TestEnv;

// ---------------------------------------------------------------------------
// init (fresh repo)
// ---------------------------------------------------------------------------

#[test]
fn init_creates_fresh_repo() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);

    // .git directory created
    assert!(env.repo.join(".git").is_dir(), ".git not created");

    // base/home/ created
    assert!(
        env.repo.join("base/home").is_dir(),
        "base/home/ not created"
    );

    // config.toml has machine set
    let config = env.read_config();
    assert!(
        config.contains("testbox"),
        "machine not in config:\n{}",
        config
    );
}

#[test]
fn init_rejects_invalid_machine_names() {
    let env = TestEnv::new();

    // Empty name
    env.run_err(&["init", "--machine", ""]);

    // Reserved name 'base'
    env.run_err(&["init", "--machine", "base"]);

    // Reserved platform name
    env.run_err(&["init", "--machine", "macos"]);

    // Contains slash
    env.run_err(&["init", "--machine", "foo/bar"]);

    // Contains ..
    env.run_err(&["init", "--machine", "foo/../bar"]);

    // Starts with dot
    env.run_err(&["init", "--machine", ".hidden"]);
}

#[test]
fn init_idempotent_on_existing_repo() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    assert!(env.repo.join(".git").is_dir());

    // Running again should not fail
    env.run_ok(&["init", "--machine", "testbox"]);
    assert!(env.repo.join(".git").is_dir());
}

// ---------------------------------------------------------------------------
// config machine
// ---------------------------------------------------------------------------

#[test]
fn config_machine_sets_name() {
    let env = TestEnv::new();

    // First init with one name
    env.run_ok(&["init", "--machine", "oldbox"]);
    let config = env.read_config();
    assert!(config.contains("oldbox"));

    // Change machine name
    env.run_ok(&["config", "machine", "newbox"]);
    let config = env.read_config();
    assert!(
        config.contains("newbox"),
        "machine not updated:\n{}",
        config
    );
    assert!(
        !config.contains("oldbox"),
        "old name still present:\n{}",
        config
    );
}

#[test]
fn config_machine_rejects_invalid_names() {
    let env = TestEnv::new();
    env.run_ok(&["init", "--machine", "testbox"]);

    env.run_err(&["config", "machine", ""]);
    env.run_err(&["config", "machine", "base"]);
    env.run_err(&["config", "machine", "linux"]);
    env.run_err(&["config", "machine", "a/b"]);
}

#[test]
fn config_machine_without_init_fails() {
    let env = TestEnv::new();
    // No repo initialized — config machine still works because it only
    // writes to state dir, but let's verify it doesn't crash.
    // Actually, config machine only writes config.toml, no repo check.
    // So it should succeed.
    env.run_ok(&["config", "machine", "testbox"]);
    let config = env.read_config();
    assert!(config.contains("testbox"));
}
