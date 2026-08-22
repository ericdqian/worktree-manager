use assert_cmd::Command;
use predicates::prelude::*;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    thread,
    time::{Duration, Instant},
};
use tempfile::{TempDir, tempdir};
use worktree_manager::{
    CONFIG_FILE_NAME, SETUP_REPOSITORY_ROOT_ENV, WorktreeBase, create_and_setup_worktree_from_base,
    current_worktree_base, list_worktree_bases,
};

#[test]
fn creates_worktree_and_runs_setup_commands() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    write_config(&repo, r#"{"setupCommands":["echo setup > setup.txt"]}"#);

    worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created worktree 'feature-a'"))
        .stdout(predicate::str::contains("ran 1 setup command(s)"));

    assert!(worktree_path(&repo, "feature-a").join("setup.txt").exists());
    assert_worktree_directory_is_ignored(&repo);
}

#[test]
fn setup_commands_receive_primary_repository_root() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    write_config(
        &repo,
        &format!(
            r#"{{"setupCommands":["printf %s \"${{{SETUP_REPOSITORY_ROOT_ENV}}}\" > setup-repo-root.txt"]}}"#
        ),
    );

    worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(worktree_path(&repo, "feature-a").join("setup-repo-root.txt")).unwrap(),
        fs::canonicalize(repo).unwrap().display().to_string(),
    );
}

#[test]
fn starts_background_setup_without_waiting_for_it_to_finish() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    write_config(
        &repo,
        r#"{"backgroundSetupCommands":["echo started; sleep 2; echo finished"]}"#,
    );

    let started_at = Instant::now();
    let output = worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "started 1 background setup command(s)",
        ))
        .get_output()
        .stdout
        .clone();

    assert!(
        started_at.elapsed() < Duration::from_secs(1),
        "CLI waited for the background setup command"
    );

    let stdout = String::from_utf8(output).unwrap();
    let status_path = reported_path(&stdout, "setup status: ");
    let log_path = reported_path(&stdout, "setup log: ");
    wait_for_file_contents(&status_path, "succeeded\n");

    assert_eq!(fs::read_to_string(status_path).unwrap(), "succeeded\n");
    assert_eq!(fs::read_to_string(log_path).unwrap(), "started\nfinished\n");
}

#[test]
fn background_setup_failure_does_not_fail_worktree_creation() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    write_config(
        &repo,
        r#"{"backgroundSetupCommands":["echo failing; exit 7"]}"#,
    );

    let output = worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let status_path = reported_path(&stdout, "setup status: ");
    let log_path = reported_path(&stdout, "setup log: ");
    wait_for_file_prefix(&status_path, "failed\n");

    assert!(
        fs::read_to_string(status_path)
            .unwrap()
            .contains("failed with status")
    );
    assert!(
        fs::read_to_string(log_path)
            .unwrap()
            .contains("setup command 'echo failing; exit 7' failed with status")
    );
    assert!(worktree_path(&repo, "feature-a").exists());
}

#[test]
fn creates_worktree_from_origin_main_instead_of_current_head() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    run_git(
        &repo,
        &["commit", "--allow-empty", "-m", "Local-only commit"],
    );

    worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .success();

    assert_eq!(
        git_output(&repo, "refs/heads/feature-a"),
        git_output(&repo, "refs/remotes/origin/main")
    );
    assert_ne!(git_output(&repo, "HEAD"), git_output(&repo, "feature-a"));
}

#[test]
fn creates_worktree_from_selected_local_branch() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    run_git(&repo, &["switch", "-c", "recent-work"]);
    run_git(&repo, &["commit", "--allow-empty", "-m", "Recent work"]);
    run_git(&repo, &["switch", "main"]);

    create_and_setup_worktree_from_base(
        &repo,
        "feature-a",
        &WorktreeBase::LocalBranch("recent-work".to_string()),
    )
    .unwrap();

    assert_eq!(
        git_output(&repo, "refs/heads/feature-a"),
        git_output(&repo, "refs/heads/recent-work")
    );
}

#[test]
fn lists_all_local_branches_by_most_recent_activity() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);

    for branch_number in 1..=6 {
        let branch_name = format!("activity-{branch_number}");
        run_git(&repo, &["switch", "-c", &branch_name]);
        commit_with_date(
            &repo,
            &format!("Activity {branch_number}"),
            &format!("2001-01-0{branch_number}T00:00:00Z"),
        );
        run_git(&repo, &["switch", "main"]);
    }

    assert_eq!(
        list_worktree_bases(&repo).unwrap(),
        vec![
            WorktreeBase::LocalBranch("activity-6".to_string()),
            WorktreeBase::LocalBranch("activity-5".to_string()),
            WorktreeBase::LocalBranch("activity-4".to_string()),
            WorktreeBase::LocalBranch("activity-3".to_string()),
            WorktreeBase::LocalBranch("activity-2".to_string()),
            WorktreeBase::LocalBranch("activity-1".to_string()),
            WorktreeBase::LocalBranch("main".to_string()),
        ]
    );
}

#[test]
fn identifies_the_current_local_branch() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    run_git(&repo, &["switch", "-c", "current-work"]);

    assert_eq!(
        current_worktree_base(&repo).unwrap(),
        Some(WorktreeBase::LocalBranch("current-work".to_string()))
    );
}

#[test]
fn detached_head_has_no_current_local_branch() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    run_git(&repo, &["switch", "--detach"]);

    assert_eq!(current_worktree_base(&repo).unwrap(), None);
}

#[test]
fn default_name_is_random_and_does_not_consume_stdin() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);

    let output = worktree_command(&repo)
        .write_stdin("stdin-name-is-ignored\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let name = created_worktree_name(&stdout);

    assert_eq!(name.len(), 10);
    assert!(!name.starts_with("agent-"));
    assert_ne!(name, "stdin-name-is-ignored");
    assert!(worktree_path(&repo, name).exists());
}

#[test]
fn writes_created_worktree_path_for_shell_integration() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    let path_file = temp_dir.path().join("created-worktree-path");

    worktree_command(&repo)
        .args([
            "--name",
            "feature-a",
            "--created-path-file",
            path_file.to_str().unwrap(),
        ])
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
        .stdout(predicate::str::contains("wt() {"))
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
        .args(["--name", "feature-a"])
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
        .args(["--name", "feature-a"])
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
        .args(["--name", "feature-a"])
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
        .args(["--name", "feature-a"])
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
        .args(["--name", "feature-a"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("target path already exists"));
}

#[test]
fn missing_origin_main_fails_before_creating_worktree() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    run_git(&repo, &["update-ref", "-d", "refs/remotes/origin/main"]);

    worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "base branch 'origin/main' does not exist or has no commits",
        ));

    assert!(!worktree_path(&repo, "feature-a").exists());
}

#[test]
fn setup_command_failure_leaves_worktree_for_inspection() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    write_config(&repo, r#"{"setupCommands":["exit 7"]}"#);

    worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "setup command 'exit 7' failed with status",
        ));

    assert!(worktree_path(&repo, "feature-a").exists());
}

#[test]
fn clean_removes_a_worktree_and_deletes_its_merged_branch() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);

    worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .success();

    worktree_command(&repo)
        .args(["clean", "--name", "feature-a"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed worktree 'feature-a'"))
        .stdout(predicate::str::contains("deleted branch 'feature-a'"));

    assert!(!worktree_path(&repo, "feature-a").exists());
    assert!(!worktree_is_registered(&repo, "feature-a"));
    assert!(!branch_exists(&repo, "feature-a"));
}

#[test]
fn clean_keeps_a_branch_that_still_holds_unmerged_commits() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);

    worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .success();
    run_git(
        &worktree_path(&repo, "feature-a"),
        &["commit", "--allow-empty", "-m", "agent work"],
    );

    worktree_command(&repo)
        .args(["clean", "--name", "feature-a"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed worktree 'feature-a'"))
        .stdout(predicate::str::contains(
            "kept branch 'feature-a': the branch 'feature-a' is not fully merged",
        ));

    assert!(!worktree_path(&repo, "feature-a").exists());
    assert!(branch_exists(&repo, "feature-a"));
}

#[test]
fn clean_refuses_a_worktree_with_uncommitted_changes() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);

    worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .success();
    fs::write(
        worktree_path(&repo, "feature-a").join("in-progress.txt"),
        "unsaved work\n",
    )
    .unwrap();

    worktree_command(&repo)
        .args(["clean", "--name", "feature-a"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "contains modified or untracked files",
        ))
        .stderr(predicate::str::contains(
            "failed to remove 1 of 1 worktree(s)",
        ));

    assert!(worktree_path(&repo, "feature-a").exists());
    assert!(branch_exists(&repo, "feature-a"));
}

#[test]
fn forced_clean_removes_a_dirty_worktree_and_its_branch() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);

    worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .success();
    run_git(
        &worktree_path(&repo, "feature-a"),
        &["commit", "--allow-empty", "-m", "agent work"],
    );
    fs::write(
        worktree_path(&repo, "feature-a").join("in-progress.txt"),
        "unsaved work\n",
    )
    .unwrap();

    worktree_command(&repo)
        .args(["clean", "--name", "feature-a", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("deleted branch 'feature-a'"));

    assert!(!worktree_path(&repo, "feature-a").exists());
    assert!(!branch_exists(&repo, "feature-a"));
}

#[test]
fn clean_removes_background_setup_files() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);
    write_config(&repo, r#"{"backgroundSetupCommands":["echo setup"]}"#);

    let stdout = worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(stdout).unwrap();
    let status_path = reported_path(&stdout, "setup status: ");
    let log_path = reported_path(&stdout, "setup log: ");
    wait_for_file_contents(&status_path, "succeeded\n");

    worktree_command(&repo)
        .args(["clean", "--name", "feature-a"])
        .assert()
        .success();

    assert!(!status_path.exists());
    assert!(!log_path.exists());
}

#[test]
fn clean_removes_every_named_worktree() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);

    for worktree_name in ["feature-a", "feature-b"] {
        worktree_command(&repo)
            .args(["--name", worktree_name])
            .assert()
            .success();
    }

    worktree_command(&repo)
        .args(["clean", "--name", "feature-a", "--name", "feature-b"])
        .assert()
        .success();

    assert!(!worktree_path(&repo, "feature-a").exists());
    assert!(!worktree_path(&repo, "feature-b").exists());
}

#[test]
fn clean_fails_for_an_unknown_worktree_without_removing_anything() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);

    worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .success();

    worktree_command(&repo)
        .args(["clean", "--name", "missing", "--name", "feature-a"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no worktree named 'missing' in .worktrees/",
        ));

    assert!(worktree_path(&repo, "feature-a").exists());
}

#[test]
fn clean_without_a_name_requires_a_terminal() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);

    worktree_command(&repo)
        .args(["--name", "feature-a"])
        .assert()
        .success();

    worktree_command(&repo)
        .arg("clean")
        .assert()
        .failure()
        .stderr(predicate::str::contains("pass --name to choose one"));

    assert!(worktree_path(&repo, "feature-a").exists());
}

#[test]
fn clean_reports_when_there_is_nothing_to_clean() {
    let temp_dir = tempdir().unwrap();
    let repo = initialized_repo(&temp_dir);

    worktree_command(&repo)
        .arg("clean")
        .assert()
        .success()
        .stdout(predicate::str::contains("no worktrees to clean up"));
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
    commit_with_date(&repo, "Initial commit", "2000-01-01T00:00:00Z");
    run_git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);

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

fn commit_with_date(repo: &Path, message: &str, date: &str) {
    let output = ProcessCommand::new("git")
        .args(["commit", "--allow-empty", "-m", message])
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .current_dir(repo)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(repo: &Path, revision: &str) -> String {
    let output = ProcessCommand::new("git")
        .args(["rev-parse", revision])
        .current_dir(repo)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "git rev-parse {revision} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn created_worktree_name(stdout: &str) -> &str {
    stdout
        .split("created worktree '")
        .nth(1)
        .and_then(|suffix| suffix.split('\'').next())
        .unwrap()
}

fn reported_path(stdout: &str, prefix: &str) -> PathBuf {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .map(PathBuf::from)
        .unwrap()
}

fn wait_for_file_contents(path: &Path, expected_contents: &str) {
    wait_for_file(path, |contents| contents == expected_contents);
}

fn wait_for_file_prefix(path: &Path, expected_prefix: &str) {
    wait_for_file(path, |contents| contents.starts_with(expected_prefix));
}

fn wait_for_file(path: &Path, predicate: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        if fs::read_to_string(path).is_ok_and(|contents| predicate(&contents)) {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn worktree_path(repo: &Path, worktree_name: &str) -> PathBuf {
    repo.join(".worktrees").join(worktree_name)
}

fn worktree_is_registered(repo: &Path, worktree_name: &str) -> bool {
    let output = ProcessCommand::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo)
        .output()
        .unwrap();

    String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        line.strip_prefix("worktree ")
            .is_some_and(|path| Path::new(path).ends_with(worktree_name))
    })
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    ProcessCommand::new("git")
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .current_dir(repo)
        .status()
        .unwrap()
        .success()
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
