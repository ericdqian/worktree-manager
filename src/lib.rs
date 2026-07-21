use rand::{Rng, distr::Alphanumeric};
use serde::Deserialize;
use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const CONFIG_FILE_NAME: &str = "agent-worktree.config.json";
pub const SETUP_REPOSITORY_ROOT_ENV: &str = "WORKTREE_MANAGER_REPO_ROOT";
const WORKTREE_DIRECTORY_NAME: &str = ".worktrees";
const WORKTREE_IGNORE_PATTERN: &str = "/.worktrees/";

#[derive(Debug, PartialEq, Eq)]
pub struct WorktreeOutcome {
    pub name: String,
    pub path: PathBuf,
    pub config_missing: bool,
    pub setup_commands_run: usize,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeConfig {
    pub setup_commands: Vec<String>,
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

    Ok(WorktreeOutcome {
        name: worktree_name.to_string(),
        path: worktree_path,
        config_missing: config.is_none(),
        setup_commands_run,
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
}
