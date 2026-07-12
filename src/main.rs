use clap::Parser;
use dialoguer::Input;
use std::{env, process::ExitCode};
use worktree_manager::{CONFIG_FILE_NAME, create_and_setup_worktree, resolve_worktree_name};

#[derive(Parser)]
#[command(
    author,
    version,
    about = "Create and set up Git worktrees for AI agent work"
)]
struct Cli {}

fn main() -> ExitCode {
    let _cli = Cli::parse();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let worktree_name = prompt_worktree_name()?;
    let cwd = env::current_dir().map_err(|error| format!("failed to determine cwd: {error}"))?;
    let outcome = create_and_setup_worktree(&cwd, &worktree_name)?;

    if outcome.config_missing {
        eprintln!("warning: {CONFIG_FILE_NAME} not found; skipping setup commands");
    }

    println!("created worktree '{}'", outcome.name);
    println!("path: {}", outcome.path.display());

    if outcome.setup_commands_run > 0 {
        println!("ran {} setup command(s)", outcome.setup_commands_run);
    }

    Ok(())
}

fn prompt_worktree_name() -> Result<String, String> {
    let input = Input::<String>::new()
        .with_prompt("Worktree name (blank for random)")
        .allow_empty(true)
        .interact_text()
        .map_err(|error| format!("failed to read worktree name: {error}"))?;

    Ok(resolve_worktree_name(&input))
}
