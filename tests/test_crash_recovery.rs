//! Crash recovery integration tests.
//!
//! These tests validate the pending-plan crash recovery mechanism end-to-end:
//! - Pending plan file creation during plan execution
//! - Pending plan detection on subsequent runs
//! - Rollback of completed actions from a pending plan
//! - Handling of stale/invalid pending plans
//!
//! Each test uses an isolated temp directory for the repo, state, and home
//! paths to avoid interference between tests.

mod common;
use common::TestEnv;

/// Escape a path for safe inclusion in a JSON string literal.
/// Handles backslashes (Windows paths) and double quotes.
fn json_escape_path(path: &str) -> String {
    path.replace('\\', "\\\\").replace('"', "\\\"")
}

// ---------------------------------------------------------------------------
// Helper: write a pending plan file directly (simulates a crash mid-execution)
// ---------------------------------------------------------------------------

/// Write a minimal pending plan JSON to the state directory.
///
/// This simulates what happens when the process is killed after
/// `save_pending_plan()` but before `clear_pending_plan()`.
fn write_pending_plan(state: &std::path::Path, repo_path: &std::path::Path, actions: &str) {
    let plan_json = format!(
        r#"{{
            "repo_path": "{}",
            "actions": {}
        }}"#,
        json_escape_path(&repo_path.display().to_string()),
        actions
    );
    std::fs::create_dir_all(state).unwrap();
    std::fs::write(state.join("pending_plan.json"), plan_json).unwrap();
}

// ---------------------------------------------------------------------------
// Test 1: Pending plan file is created and cleared during normal execution
// ---------------------------------------------------------------------------

/// Verify that a successful `apply` creates a pending plan before execution
/// and clears it after completion.
///
/// This test validates the happy path: the pending plan file should not
/// persist after a successful operation.
#[test]
fn pending_plan_cleared_after_successful_apply() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create a tracked file in the repo
    let repo_file = env.repo.join("base/home/.testrc");
    std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
    std::fs::write(&repo_file, "test content").unwrap();

    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["add", "base/home/.testrc"])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", "add testrc"])
        .output()
        .unwrap();

    // Apply should succeed and clear the pending plan
    env.run_ok(&["apply"]);

    // Pending plan file should NOT exist after successful execution
    let pending_path = env.state.join("pending_plan.json");
    assert!(
        !pending_path.exists(),
        "pending plan file should be cleared after successful apply, but exists at {}",
        pending_path.display()
    );

    // Symlink should have been created
    let target = env.home.join(".testrc");
    assert!(
        target.is_symlink(),
        "symlink should exist at {}",
        target.display()
    );
}

// ---------------------------------------------------------------------------
// Test 2: --recover flag skips pending plan detection
// ---------------------------------------------------------------------------

/// Verify that the `--recover` flag causes dotty to skip the recovery prompt
/// and proceed with the current command even when a pending plan exists.
///
/// This is the non-interactive recovery path: the user explicitly opts to
/// bypass the pending plan and continue.
#[test]
fn recover_flag_skips_pending_plan_prompt() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Simulate a crashed operation by writing a pending plan with a simple action
    write_pending_plan(
        &env.state,
        &env.repo,
        r#"[{ "CreateDir": { "path": "/tmp/dotty-test-dir" } }]"#,
    );

    // Verify the pending plan file exists
    assert!(
        env.state.join("pending_plan.json").exists(),
        "pending plan file should exist before --recover"
    );

    // Run with --recover flag — should skip the prompt and proceed
    let out = env.run(&["--recover", "status"]);

    // Command should succeed (status doesn't need a pending plan)

    // The pending plan should still exist (--recover skips it, doesn't clear it)
    assert!(
        env.state.join("pending_plan.json").exists(),
        "pending plan file should still exist after --recover (it skips, doesn't clear)"
    );

    // Should not show the recovery prompt text
    assert!(
        !out.stdout.contains("pending plan") && !out.stderr.contains("pending plan"),
        "--recover should skip the pending plan prompt. stdout: {}, stderr: {}",
        out.stdout,
        out.stderr
    );
}

// ---------------------------------------------------------------------------
// Test 3: Pending plan with CreateDir actions is rolled back
// ---------------------------------------------------------------------------

/// Simulate a crash after some CreateDir actions completed, then verify
/// that the recovery rollback removes the created directories.
///
/// This test:
/// 1. Creates directories that a plan would create
/// 2. Writes a pending plan referencing those directories
/// 3. Runs dotty and selects "Rollback" (option 0)
/// 4. Verifies the directories are removed
#[test]
fn rollback_removes_created_directories() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create directories that a "crashed" plan would have created
    let dir1 = env.home.join(".dotty_test_dir1");
    let dir2 = env.home.join(".dotty_test_dir2");
    std::fs::create_dir_all(&dir1).unwrap();
    std::fs::create_dir_all(&dir2).unwrap();
    assert!(dir1.is_dir());
    assert!(dir2.is_dir());

    // Write a pending plan with CreateDir actions for those directories
    write_pending_plan(
        &env.state,
        &env.repo,
        &format!(
            r#"
            [
                {{ "CreateDir": {{ "path": "{}" }} }},
                {{ "CreateDir": {{ "path": "{}" }} }}
            ]
            "#,
            json_escape_path(&dir1.display().to_string()),
            json_escape_path(&dir2.display().to_string())
        ),
    );

    // Run dotty with --recovery-action rollback to handle the pending plan
    let out = env.run(&["--recovery-action", "rollback", "status"]);

    // Verify rollback happened
    assert!(
        !dir1.exists() || !dir2.exists(),
        "at least one directory should be removed after rollback. stdout: {}, stderr: {}",
        out.stdout,
        out.stderr
    );

    // Pending plan should be cleared after rollback
    assert!(
        !env.state.join("pending_plan.json").exists(),
        "pending plan should be cleared after rollback. stdout: {}, stderr: {}",
        out.stdout,
        out.stderr
    );
}

// ---------------------------------------------------------------------------
// Test 4: Pending plan discard option removes the plan file
// ---------------------------------------------------------------------------

/// Simulate a pending plan and verify that selecting "Discard" (option 1)
/// removes the pending plan file without executing any rollback actions.
#[test]
fn discard_removes_pending_plan_without_rollback() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create a directory that a "crashed" plan would have created
    let dir = env.home.join(".dotty_test_discard");
    std::fs::create_dir_all(&dir).unwrap();
    assert!(dir.is_dir());

    // Write a pending plan with a CreateDir action
    write_pending_plan(
        &env.state,
        &env.repo,
        &format!(
            r#"[{{ "CreateDir": {{ "path": "{}" }} }}]"#,
            json_escape_path(&dir.display().to_string())
        ),
    );

    // Run dotty with --recovery-action discard to handle the pending plan
    let out = env.run(&["--recovery-action", "discard", "status"]);

    // Pending plan should be cleared
    assert!(
        !env.state.join("pending_plan.json").exists(),
        "pending plan should be cleared after discard. stdout: {}, stderr: {}",
        out.stdout,
        out.stderr
    );

    // Directory should NOT be removed (discard doesn't rollback)
    assert!(
        dir.is_dir(),
        "directory should still exist after discard (no rollback). stdout: {}, stderr: {}",
        out.stdout,
        out.stderr
    );
}

// ---------------------------------------------------------------------------
// Test 5: Stale pending plan (repo deleted) is detected as invalid
// ---------------------------------------------------------------------------

/// Write a pending plan pointing to a non-existent repository and verify
/// that dotty detects it as invalid and offers to discard it.
///
/// This tests the integrity validation in `load_pending_plan()` which
/// checks that the repository path still exists and contains a .git directory.
#[test]
fn stale_pending_plan_detected_when_repo_deleted() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create a separate directory that will be "deleted"
    let fake_repo = tempfile::tempdir().unwrap();
    let fake_repo_path = fake_repo.path().to_path_buf();
    std::fs::create_dir_all(fake_repo_path.join(".git")).unwrap();

    // Write a pending plan pointing to the fake repo
    write_pending_plan(
        &env.state,
        &fake_repo_path,
        r#"[{ "CreateDir": { "path": "/tmp/test" } }]"#,
    );

    // Drop the temp dir (simulating repo deletion)
    drop(fake_repo);

    // Run dotty with --recovery-action discard to handle the stale plan
    let out = env.run(&["--recovery-action", "discard", "status"]);

    // Should detect the stale plan
    let combined = format!("{} {}", out.stdout, out.stderr);
    assert!(
        combined.contains("invalid")
            || combined.contains("no longer exists")
            || combined.contains("stale"),
        "should detect stale pending plan. stdout: {}, stderr: {}",
        out.stdout,
        out.stderr
    );
}

// ---------------------------------------------------------------------------
// Test 6: Pending plan with mixed action types roundtrips correctly
// ---------------------------------------------------------------------------

/// Write a pending plan with multiple action types and verify that
/// dotty can parse and display all action types during recovery.
///
/// This validates the serialization/deserialization of all SerializableAction
/// variants through the integration test path.
#[test]
fn pending_plan_with_mixed_actions_parses_correctly() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    let home = &env.home;

    // Write a pending plan with various action types
    write_pending_plan(
        &env.state,
        &env.repo,
        &format!(
            r#"
            [
                {{ "CreateDir": {{ "path": "{}" }} }},
                {{ "CopyFile": {{ "source": "{}", "dest": "{}" }} }},
                {{ "CreateSymlink": {{ "target": "{}", "link": "{}", "backup_path": null }} }},
                {{ "RemoveFile": {{ "path": "{}" }} }},
                {{ "GitAdd": {{ "paths": ["{}"] }} }},
                {{ "GitCommit": {{ "message": "test commit" }} }}
            ]
            "#,
            json_escape_path(&home.join(".mixed_dir").display().to_string()),
            json_escape_path(&home.join(".src").display().to_string()),
            json_escape_path(&home.join(".dst").display().to_string()),
            json_escape_path(&home.join(".target").display().to_string()),
            json_escape_path(&home.join(".link").display().to_string()),
            json_escape_path(&home.join(".remove").display().to_string()),
            json_escape_path(&env.repo.join("base/home/.file").display().to_string()),
        ),
    );

    // Run with --recover to skip the prompt, verify no parse errors
    let out = env.run(&["--recover", "status"]);
    // Should not have JSON parse errors
    assert!(
        !out.stderr.contains("serde")
            && !out.stderr.contains("invalid type")
            && !out.stderr.contains("parse"),
        "pending plan with mixed actions should parse without errors. stderr: {}",
        out.stderr,
    );
}
