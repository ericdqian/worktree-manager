use rand::{Rng, distr::Alphanumeric};
use serde::Deserialize;
use std::{
    fmt,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const CONFIG_FILE_NAME: &str = "agent-worktree.config.json";
pub const SETUP_REPOSITORY_ROOT_ENV: &str = "WORKTREE_MANAGER_REPO_ROOT";
pub const BACKGROUND_SETUP_WORKER_COMMAND: &str = "__run-background-setup";
const WORKTREE_DIRECTORY_NAME: &str = ".worktrees";
const WORKTREE_IGNORE_PATTERN: &str = "/.worktrees/";
const BACKGROUND_SETUP_DIRECTORY: &str = "worktree-manager/setup";

#[derive(Debug, PartialEq, Eq)]
pub struct WorktreeOutcome {
    pub name: String,
    pub path: PathBuf,
    pub config_missing: bool,
    pub setup_commands_run: usize,
    pub background_setup: Option<BackgroundSetup>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BackgroundSetup {
    pub repository_root: PathBuf,
    pub worktree_path: PathBuf,
    pub commands: Vec<String>,
    pub log_path: PathBuf,
    pub status_path: PathBuf,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeConfig {
    #[serde(default)]
    pub setup_commands: Vec<String>,
    #[serde(default)]
    pub background_setup_commands: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorktreeBase {
    OriginMain,
    LocalBranch(String),
}

impl WorktreeBase {
    fn reference(&self) -> String {
        match self {
            Self::OriginMain => "refs/remotes/origin/main".to_string(),
            Self::LocalBranch(branch_name) => format!("refs/heads/{branch_name}"),
        }
    }
}

impl fmt::Display for WorktreeBase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OriginMain => formatter.write_str("origin/main"),
            Self::LocalBranch(branch_name) => formatter.write_str(branch_name),
        }
    }
}

pub fn list_worktree_bases(cwd: &Path) -> Result<Vec<WorktreeBase>, String> {
    let git_context = discover_git_context(cwd)?;
    let local_branches = git_output(
        &git_context.repo_root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)",
            "refs/heads",
        ],
    )?;

    Ok(local_branches
        .lines()
        .map(|branch_name| WorktreeBase::LocalBranch(branch_name.to_string()))
        .collect())
}

pub fn current_worktree_base(cwd: &Path) -> Result<Option<WorktreeBase>, String> {
    let output = git_status(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;

    if output.status.success() {
        let branch_name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok(Some(WorktreeBase::LocalBranch(branch_name)));
    }

    if output.status.code() == Some(1) {
        return Ok(None);
    }

    Err(format!(
        "failed to determine the checked-out branch: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[derive(Debug, PartialEq, Eq)]
struct GitContext {
    repo_root: PathBuf,
}

pub fn create_and_setup_worktree(
    cwd: &Path,
    worktree_name: &str,
) -> Result<WorktreeOutcome, String> {
    create_and_setup_worktree_from_base(cwd, worktree_name, &WorktreeBase::OriginMain)
}

pub fn create_and_setup_worktree_from_base(
    cwd: &Path,
    worktree_name: &str,
    base: &WorktreeBase,
) -> Result<WorktreeOutcome, String> {
    validate_worktree_name(worktree_name)?;

    let git_context = discover_git_context(cwd)?;
    ensure_head_exists(&git_context.repo_root)?;
    ensure_base_exists(&git_context.repo_root, base)?;
    ensure_branch_available(&git_context.repo_root, worktree_name)?;

    let worktree_path = derive_worktree_path(&git_context.repo_root, worktree_name);
    ensure_target_path_available(&worktree_path)?;

    let config = load_config(&git_context.repo_root)?;

    // Keep generated worktrees out of status without changing tracked project files.
    ensure_worktree_directory_is_ignored(&git_context.repo_root)?;
    create_worktree(&git_context.repo_root, worktree_name, &worktree_path, base)?;

    let setup_commands_run = match &config {
        Some(config) => run_setup_commands(
            &git_context.repo_root,
            &worktree_path,
            &config.setup_commands,
        )?,
        None => 0,
    };
    let background_setup = match &config {
        Some(config) => background_setup_for_commands(
            &git_context.repo_root,
            &worktree_path,
            worktree_name,
            &config.background_setup_commands,
        )?,
        None => None,
    };

    Ok(WorktreeOutcome {
        name: worktree_name.to_string(),
        path: worktree_path,
        config_missing: config.is_none(),
        setup_commands_run,
        background_setup,
    })
}

fn background_setup_for_commands(
    repository_root: &Path,
    worktree_path: &Path,
    worktree_name: &str,
    commands: &[String],
) -> Result<Option<BackgroundSetup>, String> {
    if commands.is_empty() {
        return Ok(None);
    }

    let setup_directory = git_path(repository_root, BACKGROUND_SETUP_DIRECTORY)?;

    Ok(Some(BackgroundSetup {
        repository_root: repository_root.to_path_buf(),
        worktree_path: worktree_path.to_path_buf(),
        commands: commands.to_vec(),
        log_path: setup_directory.join(format!("{worktree_name}.log")),
        status_path: setup_directory.join(format!("{worktree_name}.status")),
    }))
}

pub fn start_background_setup(
    worker_executable: &Path,
    setup: &BackgroundSetup,
) -> Result<u32, String> {
    let setup_directory = setup
        .log_path
        .parent()
        .ok_or_else(|| "background setup log path has no parent directory".to_string())?;
    fs::create_dir_all(setup_directory).map_err(|error| {
        format!(
            "failed to create background setup directory {}: {error}",
            setup_directory.display()
        )
    })?;
    write_background_setup_status(&setup.status_path, "pending\n")?;

    let log_file = File::create(&setup.log_path).map_err(|error| {
        format!(
            "failed to create background setup log {}: {error}",
            setup.log_path.display()
        )
    })?;
    let stdout_log = log_file.try_clone().map_err(|error| {
        format!(
            "failed to open background setup log {} for stdout: {error}",
            setup.log_path.display()
        )
    })?;

    let mut command = Command::new(worker_executable);
    command
        .arg(BACKGROUND_SETUP_WORKER_COMMAND)
        .arg("--repository-root")
        .arg(&setup.repository_root)
        .arg("--worktree-path")
        .arg(&setup.worktree_path)
        .arg("--status-path")
        .arg(&setup.status_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(log_file));

    for setup_command in &setup.commands {
        command.arg(format!("--setup-command={setup_command}"));
    }

    configure_detached_process(&mut command);

    match command.spawn() {
        Ok(child) => Ok(child.id()),
        Err(error) => {
            let message = format!(
                "failed to start background setup worker {}: {error}",
                worker_executable.display()
            );
            write_background_setup_status(&setup.status_path, &format!("failed\n{message}\n"))?;
            Err(message)
        }
    }
}

#[cfg(unix)]
fn configure_detached_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // The worker must outlive this CLI and must not receive terminal signals intended for it.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

#[cfg(windows)]
fn configure_detached_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
}

pub fn run_background_setup_worker(
    repository_root: &Path,
    worktree_path: &Path,
    status_path: &Path,
    setup_commands: &[String],
) -> Result<usize, String> {
    write_background_setup_status(status_path, "running\n")?;

    match run_setup_commands(repository_root, worktree_path, setup_commands) {
        Ok(commands_run) => {
            write_background_setup_status(status_path, "succeeded\n")?;
            Ok(commands_run)
        }
        Err(error) => {
            write_background_setup_status(status_path, &format!("failed\n{error}\n"))?;
            Err(error)
        }
    }
}

fn write_background_setup_status(status_path: &Path, contents: &str) -> Result<(), String> {
    fs::write(status_path, contents).map_err(|error| {
        format!(
            "failed to update background setup status {}: {error}",
            status_path.display()
        )
    })
}

pub fn resolve_worktree_name(input: &str) -> String {
    let trimmed = input.trim();

    if trimmed.is_empty() {
        generate_random_name()
    } else {
        trimmed.to_string()
    }
}

pub fn generate_random_name() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(10)
        .map(char::from)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

pub fn validate_worktree_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("worktree name cannot be empty".to_string());
    }

    if name == "." || name == ".." || name == "@" {
        return Err(format!("'{name}' is not a valid worktree name"));
    }

    if name.starts_with('-')
        || !name.starts_with(|character: char| character.is_ascii_alphanumeric())
    {
        return Err("worktree name must start with an ASCII letter or number".to_string());
    }

    if !name.ends_with(|character: char| character.is_ascii_alphanumeric()) {
        return Err("worktree name must end with an ASCII letter or number".to_string());
    }

    if name.contains("..") || name.contains("@{") || name.ends_with(".lock") {
        return Err(format!("'{name}' is not a valid Git branch name"));
    }

    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(
            "worktree name may only contain ASCII letters, numbers, '-', '_', and '.'".to_string(),
        );
    }

    Ok(())
}

pub fn derive_worktree_path(repo_root: &Path, worktree_name: &str) -> PathBuf {
    repo_root.join(WORKTREE_DIRECTORY_NAME).join(worktree_name)
}

pub fn load_config(repo_root: &Path) -> Result<Option<WorktreeConfig>, String> {
    let config_path = repo_root.join(CONFIG_FILE_NAME);
    let contents = match fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!("failed to read {}: {error}", config_path.display()));
        }
    };

    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|error| format!("failed to parse {}: {error}", config_path.display()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedWorktree {
    pub name: String,
    pub path: PathBuf,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchOutcome {
    Deleted(String),
    Kept { branch: String, reason: String },
    NoBranch,
}

pub fn main_worktree_root(cwd: &Path) -> Result<PathBuf, String> {
    let git_context = discover_git_context(cwd)?;
    let git_common_dir = git_output(
        &git_context.repo_root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;

    PathBuf::from(&git_common_dir)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("Git directory '{git_common_dir}' has no parent work tree"))
}

pub fn prune_worktree_metadata(main_worktree_root: &Path) -> Result<(), String> {
    git_output(main_worktree_root, &["worktree", "prune"]).map(|_| ())
}

pub fn list_managed_worktrees(main_worktree_root: &Path) -> Result<Vec<ManagedWorktree>, String> {
    let worktree_records = git_output(main_worktree_root, &["worktree", "list", "--porcelain"])?;

    Ok(managed_worktrees_from_porcelain(&worktree_records))
}

fn managed_worktrees_from_porcelain(porcelain_output: &str) -> Vec<ManagedWorktree> {
    // Git always lists the main work tree first, so it both locates the managed worktree
    // directory and drops out of the removable set.
    let mut records = porcelain_output.split("\n\n");
    let Some(worktrees_directory) = records
        .next()
        .and_then(worktree_record_path)
        .map(|main_worktree_path| main_worktree_path.join(WORKTREE_DIRECTORY_NAME))
    else {
        return Vec::new();
    };

    records
        .filter_map(|record| managed_worktree_from_record(record, &worktrees_directory))
        .collect()
}

fn worktree_record_path(record: &str) -> Option<PathBuf> {
    record
        .lines()
        .find_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
}

fn managed_worktree_from_record(
    record: &str,
    worktrees_directory: &Path,
) -> Option<ManagedWorktree> {
    let path = worktree_record_path(record)?;

    if path.parent() != Some(worktrees_directory) {
        return None;
    }

    let branch = record
        .lines()
        .find_map(|line| line.strip_prefix("branch "))
        .map(|reference| {
            reference
                .strip_prefix("refs/heads/")
                .unwrap_or(reference)
                .to_string()
        });

    Some(ManagedWorktree {
        name: path.file_name()?.to_string_lossy().into_owned(),
        path,
        branch,
    })
}

pub fn remove_worktree(
    main_worktree_root: &Path,
    worktree: &ManagedWorktree,
    force: bool,
) -> Result<BranchOutcome, String> {
    remove_worktree_directory(main_worktree_root, &worktree.path, force)?;
    remove_background_setup_artifacts(main_worktree_root, &worktree.name)?;

    match &worktree.branch {
        Some(branch) => delete_worktree_branch(main_worktree_root, branch, force),
        None => Ok(BranchOutcome::NoBranch),
    }
}

fn remove_worktree_directory(
    main_worktree_root: &Path,
    worktree_path: &Path,
    force: bool,
) -> Result<(), String> {
    let mut command = Command::new("git");
    command.args(["worktree", "remove"]);

    if force {
        command.arg("--force");
    }

    let output = command
        .arg(worktree_path)
        .current_dir(main_worktree_root)
        .output()
        .map_err(|error| format!("failed to start git worktree remove: {error}"))?;

    if output.status.success() {
        return Ok(());
    }

    Err(format!(
        "failed to remove worktree {}: {}",
        worktree_path.display(),
        git_message(&output.stderr)
    ))
}

fn remove_background_setup_artifacts(
    main_worktree_root: &Path,
    worktree_name: &str,
) -> Result<(), String> {
    let setup_directory = git_path(main_worktree_root, BACKGROUND_SETUP_DIRECTORY)?;

    for extension in ["log", "status"] {
        let artifact_path = setup_directory.join(format!("{worktree_name}.{extension}"));

        match fs::remove_file(&artifact_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "failed to remove background setup file {}: {error}",
                    artifact_path.display()
                ));
            }
        }
    }

    Ok(())
}

fn delete_worktree_branch(
    main_worktree_root: &Path,
    branch: &str,
    force: bool,
) -> Result<BranchOutcome, String> {
    let delete_flag = if force { "-D" } else { "-d" };
    let output = git_status(main_worktree_root, &["branch", delete_flag, branch])?;

    if output.status.success() {
        return Ok(BranchOutcome::Deleted(branch.to_string()));
    }

    // Git refuses to delete a branch holding unmerged work, which is the safety net that
    // lets removal itself stay unguarded; report why instead of failing the removal.
    Ok(BranchOutcome::Kept {
        branch: branch.to_string(),
        reason: git_message(&output.stderr),
    })
}

fn git_message(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .trim_start_matches("fatal: ")
        .trim_start_matches("error: ")
        .to_string()
}

fn discover_git_context(cwd: &Path) -> Result<GitContext, String> {
    let repo_root = git_output(cwd, &["rev-parse", "--show-toplevel"])
        .map_err(|_| "current directory is not inside a Git work tree".to_string())?;

    let is_bare = git_output(cwd, &["rev-parse", "--is-bare-repository"])?;
    if is_bare == "true" {
        return Err("current directory is inside a bare Git repository".to_string());
    }

    let repo_root = PathBuf::from(repo_root);
    Ok(GitContext { repo_root })
}

fn ensure_head_exists(repo_root: &Path) -> Result<(), String> {
    git_status(repo_root, &["rev-parse", "--verify", "HEAD"])
        .map(|_| ())
        .map_err(|_| "repository has no commits yet; create an initial commit first".to_string())
}

fn ensure_base_exists(repo_root: &Path, base: &WorktreeBase) -> Result<(), String> {
    let commit = format!("{}^{{commit}}", base.reference());

    git_status(repo_root, &["rev-parse", "--verify", "--quiet", &commit]).and_then(|output| {
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| format!("base branch '{base}' does not exist or has no commits"))
    })
}

fn ensure_branch_available(repo_root: &Path, worktree_name: &str) -> Result<(), String> {
    let branch_ref = format!("refs/heads/{worktree_name}");
    let output = Command::new("git")
        .args(["show-ref", "--verify", "--quiet"])
        .arg(&branch_ref)
        .current_dir(repo_root)
        .output()
        .map_err(|error| format!("failed to check branch '{worktree_name}': {error}"))?;

    if output.status.success() {
        return Err(format!("branch '{worktree_name}' already exists"));
    }

    if output.status.code() == Some(1) {
        return Ok(());
    }

    Err(format!(
        "failed to check branch '{worktree_name}': {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

fn ensure_target_path_available(worktree_path: &Path) -> Result<(), String> {
    if worktree_path.exists() {
        return Err(format!(
            "target path already exists: {}",
            worktree_path.display()
        ));
    }

    Ok(())
}

fn ensure_worktree_directory_is_ignored(repo_root: &Path) -> Result<(), String> {
    let exclude_path = git_path(repo_root, "info/exclude")?;
    let contents = match fs::read_to_string(&exclude_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(format!(
                "failed to read Git exclude file {}: {error}",
                exclude_path.display()
            ));
        }
    };

    if contents
        .lines()
        .any(|line| line.trim() == WORKTREE_IGNORE_PATTERN)
    {
        return Ok(());
    }

    let separator = if contents.is_empty() || contents.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let updated_contents = format!("{contents}{separator}{WORKTREE_IGNORE_PATTERN}\n");

    fs::write(&exclude_path, updated_contents).map_err(|error| {
        format!(
            "failed to update Git exclude file {}: {error}",
            exclude_path.display()
        )
    })
}

fn create_worktree(
    repo_root: &Path,
    worktree_name: &str,
    worktree_path: &Path,
    base: &WorktreeBase,
) -> Result<(), String> {
    let status = Command::new("git")
        .args(["worktree", "add", "-b", worktree_name])
        .arg(worktree_path)
        .arg(base.reference())
        .current_dir(repo_root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| format!("failed to start git worktree add: {error}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("git worktree add failed with status {status}"))
    }
}

fn run_setup_commands(
    repo_root: &Path,
    worktree_path: &Path,
    setup_commands: &[String],
) -> Result<usize, String> {
    for setup_command in setup_commands {
        run_setup_command(repo_root, worktree_path, setup_command)?;
    }

    Ok(setup_commands.len())
}

fn run_setup_command(
    repo_root: &Path,
    worktree_path: &Path,
    setup_command: &str,
) -> Result<(), String> {
    let mut command = shell_command(setup_command);
    let status = command
        .current_dir(worktree_path)
        .env(SETUP_REPOSITORY_ROOT_ENV, repo_root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| format!("failed to start setup command '{setup_command}': {error}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "setup command '{setup_command}' failed with status {status}"
        ))
    }
}

fn shell_command(setup_command: &str) -> Command {
    if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.args(["/C", setup_command]);
        command
    } else {
        let mut command = Command::new("sh");
        command.args(["-c", setup_command]);
        command
    }
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_status(cwd, args)?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    Err(format!(
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

fn git_path(repo_root: &Path, git_path: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(git_output(
        repo_root,
        &["rev-parse", "--git-path", git_path],
    )?);

    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(repo_root.join(path))
    }
}

fn git_status(cwd: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("failed to start git {}: {error}", args.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn resolve_worktree_name_trims_explicit_input() {
        assert_eq!(resolve_worktree_name("  feature-a  "), "feature-a");
    }

    #[test]
    fn random_name_has_path_safe_shape_without_fixed_agent_prefix() {
        let name = generate_random_name();

        assert_eq!(name.len(), 10);
        assert!(!name.starts_with("agent-"));
        assert!(
            name.chars()
                .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        );
    }

    #[test]
    fn validate_worktree_name_accepts_simple_slugs() {
        assert!(validate_worktree_name("feature-a_1").is_ok());
        assert!(validate_worktree_name("release.2026").is_ok());
    }

    #[test]
    fn validate_worktree_name_rejects_unsafe_names() {
        for name in [
            "",
            "-feature",
            "feature/",
            "feature..a",
            "feature.lock",
            "@",
        ] {
            assert!(validate_worktree_name(name).is_err(), "{name} should fail");
        }
    }

    #[test]
    fn derive_worktree_path_uses_hidden_worktree_directory() {
        let repo_root = Path::new("/tmp/example");

        assert_eq!(
            derive_worktree_path(repo_root, "feature-a"),
            PathBuf::from("/tmp/example/.worktrees/feature-a")
        );
    }

    #[test]
    fn load_config_returns_none_when_file_is_missing() {
        let temp_dir = tempdir().unwrap();

        assert_eq!(load_config(temp_dir.path()).unwrap(), None);
    }

    #[test]
    fn load_config_parses_setup_commands() {
        let temp_dir = tempdir().unwrap();
        fs::write(
            temp_dir.path().join(CONFIG_FILE_NAME),
            r#"{"setupCommands":["cargo fetch","cargo test"]}"#,
        )
        .unwrap();

        assert_eq!(
            load_config(temp_dir.path()).unwrap(),
            Some(WorktreeConfig {
                setup_commands: vec!["cargo fetch".to_string(), "cargo test".to_string()],
                background_setup_commands: Vec::new(),
            })
        );
    }

    #[test]
    fn load_config_parses_background_setup_commands_without_foreground_commands() {
        let temp_dir = tempdir().unwrap();
        fs::write(
            temp_dir.path().join(CONFIG_FILE_NAME),
            r#"{"backgroundSetupCommands":["npm install"]}"#,
        )
        .unwrap();

        assert_eq!(
            load_config(temp_dir.path()).unwrap(),
            Some(WorktreeConfig {
                setup_commands: Vec::new(),
                background_setup_commands: vec!["npm install".to_string()],
            })
        );
    }

    #[test]
    fn load_config_fails_for_invalid_json() {
        let temp_dir = tempdir().unwrap();
        fs::write(temp_dir.path().join(CONFIG_FILE_NAME), "{").unwrap();

        assert!(
            load_config(temp_dir.path())
                .unwrap_err()
                .contains("failed to parse")
        );
    }

    #[test]
    fn managed_worktrees_come_from_the_worktree_directory_of_the_main_work_tree() {
        let porcelain_output = "\
worktree /repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /repo/.worktrees/feature-a
HEAD 2222222222222222222222222222222222222222
branch refs/heads/eq/feat/login

worktree /elsewhere/manual-worktree
HEAD 3333333333333333333333333333333333333333
branch refs/heads/manual";

        assert_eq!(
            managed_worktrees_from_porcelain(porcelain_output),
            vec![ManagedWorktree {
                name: "feature-a".to_string(),
                path: PathBuf::from("/repo/.worktrees/feature-a"),
                branch: Some("eq/feat/login".to_string()),
            }]
        );
    }

    #[test]
    fn managed_worktrees_report_a_detached_head_as_having_no_branch() {
        let porcelain_output = "\
worktree /repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /repo/.worktrees/feature-a
HEAD 2222222222222222222222222222222222222222
detached";

        assert_eq!(
            managed_worktrees_from_porcelain(porcelain_output),
            vec![ManagedWorktree {
                name: "feature-a".to_string(),
                path: PathBuf::from("/repo/.worktrees/feature-a"),
                branch: None,
            }]
        );
    }

    #[test]
    fn managed_worktrees_are_empty_without_any_records() {
        assert_eq!(managed_worktrees_from_porcelain(""), Vec::new());
    }

    #[test]
    fn git_message_uses_the_first_line_without_its_severity_prefix() {
        assert_eq!(
            git_message(b"error: the branch 'feature-a' is not fully merged\nhint: use -D\n"),
            "the branch 'feature-a' is not fully merged"
        );
    }
}
