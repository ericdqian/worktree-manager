use clap::Parser;
use dialoguer::Input;
use std::{
    env, fs,
    io::{self, IsTerminal},
    path::Path,
    process::ExitCode,
};
use worktree_manager::{CONFIG_FILE_NAME, create_and_setup_worktree, resolve_worktree_name};

const SHELL_INIT: &str = r#"worktree-manager() {
  local wt_path_file worktree_path wt_result
  wt_path_file="$(mktemp)" || return
  command worktree-manager --created-path-file "$wt_path_file" "$@"
  wt_result=$?
  if [ "$wt_result" -eq 0 ]; then
    worktree_path="$(cat "$wt_path_file")"
    wt_result=$?
    rm -f "$wt_path_file"
    [ "$wt_result" -eq 0 ] || return "$wt_result"
    cd -- "$worktree_path"
  else
    rm -f "$wt_path_file"
    return "$wt_result"
  fi
}"#;

#[derive(Parser)]
#[command(
    author,
    version,
    about = "Create and set up Git worktrees for AI agent work"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,

    #[arg(long, hide = true, value_name = "PATH")]
    created_path_file: Option<std::path::PathBuf>,
}

#[derive(clap::Subcommand)]
enum CliCommand {
    /// Print the zsh/bash integration script.
    ShellInit,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    if let Some(CliCommand::ShellInit) = cli.command {
        println!("{SHELL_INIT}");
        return Ok(());
    }

    let worktree_name = prompt_worktree_name()?;
    let cwd = env::current_dir().map_err(|error| format!("failed to determine cwd: {error}"))?;
    let outcome = create_and_setup_worktree(&cwd, &worktree_name)?;

    if outcome.config_missing {
        eprintln!("warning: {CONFIG_FILE_NAME} not found; skipping setup commands");
    }

    if let Some(created_path_file) = cli.created_path_file {
        write_created_path(&created_path_file, &outcome.path)?;
    }

    println!("created worktree '{}'", outcome.name);
    println!("path: {}", outcome.path.display());

    if outcome.setup_commands_run > 0 {
        println!("ran {} setup command(s)", outcome.setup_commands_run);
    }

    Ok(())
}

fn write_created_path(created_path_file: &Path, worktree_path: &Path) -> Result<(), String> {
    fs::write(
        created_path_file,
        worktree_path.as_os_str().as_encoded_bytes(),
    )
    .map_err(|error| {
        format!(
            "failed to write created worktree path to {}: {error}",
            created_path_file.display()
        )
    })
}

fn prompt_worktree_name() -> Result<String, String> {
    if !io::stdin().is_terminal() {
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .map_err(|error| format!("failed to read worktree name: {error}"))?;

        return Ok(resolve_worktree_name(&input));
    }

    let input = Input::<String>::new()
        .with_prompt("Worktree name (blank for random)")
        .allow_empty(true)
        .interact_text()
        .map_err(|error| format!("failed to read worktree name: {error}"))?;

    Ok(resolve_worktree_name(&input))
}
