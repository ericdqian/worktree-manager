use rand::{Rng, distr::Alphanumeric};
use serde::Deserialize;
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const CONFIG_FILE_NAME: &str = "agent-worktree.config.json";

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

#[derive(Debug, PartialEq, Eq)]
struct GitContext {
    repo_root: PathBuf,
}

pub fn create_and_setup_worktree(
    cwd: &Path,
    worktree_name: &str,
) -> Result<WorktreeOutcome, String> {
    validate_worktree_name(worktree_name)?;

    let git_context = discover_git_context(cwd)?;
    ensure_head_exists(&git_context.repo_root)?;
    ensure_branch_available(&git_context.repo_root, worktree_name)?;

    let worktree_path = derive_worktree_path(&git_context.repo_root, worktree_name)?;
    ensure_target_path_available(&worktree_path)?;

    let config = load_config(&git_context.repo_root)?;
    create_worktree(&git_context.repo_root, worktree_name, &worktree_path)?;

    let setup_commands_run = match &config {
        Some(config) => run_setup_commands(&worktree_path, &config.setup_commands)?,
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

pub fn derive_worktree_path(repo_root: &Path, worktree_name: &str) -> Result<PathBuf, String> {
    let repo_name = repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "could not determine repository name from {}",
                repo_root.display()
            )
        })?;

    let parent = repo_root.parent().ok_or_else(|| {
        format!(
            "could not determine parent directory for {}",
            repo_root.display()
        )
    })?;

    Ok(parent.join(format!("{repo_name}-{worktree_name}")))
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

fn create_worktree(
    repo_root: &Path,
    worktree_name: &str,
    worktree_path: &Path,
) -> Result<(), String> {
    let status = Command::new("git")
        .args(["worktree", "add", "-b", worktree_name])
        .arg(worktree_path)
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

fn run_setup_commands(worktree_path: &Path, setup_commands: &[String]) -> Result<usize, String> {
    for setup_command in setup_commands {
        run_setup_command(worktree_path, setup_command)?;
    }

    Ok(setup_commands.len())
}

fn run_setup_command(worktree_path: &Path, setup_command: &str) -> Result<(), String> {
    let mut command = shell_command(setup_command);
    let status = command
        .current_dir(worktree_path)
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

fn git_status(cwd: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("failed to start git {}: {error}", args.join(" ")))
}
