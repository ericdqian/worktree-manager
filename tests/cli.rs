use assert_cmd::Command;
use predicates::prelude::*;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};
use tempfile::{TempDir, tempdir};
use worktree_manager::CONFIG_FILE_NAME;

#[test]
fn creates_worktree_and_runs_setup_commands() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    write_config(&repo, r#"{"setupCommands":["echo setup > setup.txt"]}"#);

    worktree_command(&repo)
        .write_stdin("feature-a\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("created worktree 'feature-a'"))
        .stdout(predicate::str::contains("ran 1 setup command(s)"));

    assert!(worktree_path(&repo, "feature-a").join("setup.txt").exists());
    assert_worktree_directory_is_ignored(&repo);
}

#[test]
fn blank_name_generates_random_slug_without_fixed_agent_prefix() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);

    let output = worktree_command(&repo)
        .write_stdin("\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let name = created_worktree_name(&stdout);

    assert_eq!(name.len(), 10);
    assert!(!name.starts_with("agent-"));
    assert!(worktree_path(&repo, name).exists());
}

#[test]
fn writes_created_worktree_path_for_shell_integration() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    let path_file = temp_dir.path().join("created-worktree-path");

    worktree_command(&repo)
        .args(["--created-path-file", path_file.to_str().unwrap()])
        .write_stdin("feature-a\n")
        .assert()
        .success();

    assert_eq!(
        fs::read(path_file).unwrap(),
        fs::canonicalize(&repo)
            .unwrap()
            .join(".worktrees/feature-a")
            .as_os_str()
            .as_encoded_bytes()
    );
}

#[test]
fn shell_init_prints_zsh_and_bash_wrapper() {
    Command::cargo_bin("worktree-manager")
        .unwrap()
        .arg("shell-init")
        .assert()
        .success()
        .stdout(predicate::str::contains("worktree-manager() {"))
        .stdout(predicate::str::contains(
            "local wt_path_file worktree_path wt_result",
        ))
        .stdout(predicate::str::contains(
            "command worktree-manager --created-path-file \"$wt_path_file\" \"$@\"",
        ))
        .stdout(predicate::str::contains("cd -- \"$worktree_path\""));
}

#[test]
fn fails_outside_git_work_tree() {
    let temp_dir = tempdir().unwrap();

    worktree_command(temp_dir.path())
        .write_stdin("feature-a\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "current directory is not inside a Git work tree",
        ));
}

#[test]
fn missing_config_warns_and_skips_setup() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);

    worktree_command(&repo)
        .write_stdin("feature-a\n")
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "warning: {CONFIG_FILE_NAME} not found; skipping setup commands"
        )));

    assert!(worktree_path(&repo, "feature-a").exists());
    assert_git_status_is_clean(&repo);
}

#[test]
fn invalid_config_fails_before_creating_worktree() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    write_config(&repo, "{");

    worktree_command(&repo)
        .write_stdin("feature-a\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to parse"));

    assert!(!worktree_path(&repo, "feature-a").exists());
}

#[test]
fn existing_branch_fails_before_creating_worktree() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    run_git(&repo, &["branch", "feature-a"]);

    worktree_command(&repo)
        .write_stdin("feature-a\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "branch 'feature-a' already exists",
        ));

    assert!(!worktree_path(&repo, "feature-a").exists());
}

#[test]
fn existing_target_path_fails_before_creating_worktree() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    fs::create_dir_all(worktree_path(&repo, "feature-a")).unwrap();

    worktree_command(&repo)
        .write_stdin("feature-a\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("target path already exists"));
}

#[test]
fn setup_command_failure_leaves_worktree_for_inspection() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    write_config(&repo, r#"{"setupCommands":["exit 7"]}"#);

    worktree_command(&repo)
        .write_stdin("feature-a\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "setup command 'exit 7' failed with status",
        ));

    assert!(worktree_path(&repo, "feature-a").exists());
}

fn worktree_command(cwd: &Path) -> Command {
    let mut command = Command::cargo_bin("worktree-manager").unwrap();
    command.current_dir(cwd);
    command
}

fn initialized_repo(temp_dir: &TempDir) -> PathBuf {
    let repo = temp_dir.path().join("repo");
    fs::create_dir(&repo).unwrap();

    run_git(&repo, &["init"]);
    run_git(&repo, &["config", "user.email", "agent@example.com"]);
    run_git(&repo, &["config", "user.name", "Agent"]);

    fs::write(repo.join("README.md"), "# Test\n").unwrap();
    run_git(&repo, &["add", "README.md"]);
    run_git(&repo, &["commit", "-m", "Initial commit"]);

    repo
}

fn write_config(repo: &Path, contents: &str) {
    fs::write(repo.join(CONFIG_FILE_NAME), contents).unwrap();
}

fn run_git(repo: &Path, args: &[&str]) {
    let output = ProcessCommand::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn created_worktree_name(stdout: &str) -> &str {
    stdout
        .split("created worktree '")
        .nth(1)
        .and_then(|suffix| suffix.split('\'').next())
        .unwrap()
}

fn worktree_path(repo: &Path, worktree_name: &str) -> PathBuf {
    repo.join(".worktrees").join(worktree_name)
}

fn assert_worktree_directory_is_ignored(repo: &Path) {
    let exclude_file = fs::read_to_string(repo.join(".git/info/exclude")).unwrap();

    assert!(exclude_file.lines().any(|line| line == "/.worktrees/"));
}

fn assert_git_status_is_clean(repo: &Path) {
    let output = ProcessCommand::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "git status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "git status showed untracked files: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
