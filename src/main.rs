use clap::Parser;
use console::{Key, Term};
use dialoguer::Input;
use std::{
    env, fs,
    io::{self, IsTerminal},
    path::Path,
    process::ExitCode,
};
use worktree_manager::{
    CONFIG_FILE_NAME, WorktreeBase, create_and_setup_worktree_from_base, list_worktree_bases,
    resolve_worktree_name,
};

const RECENT_LOCAL_BRANCH_LIMIT: usize = 5;
const BASE_SELECTOR_PROMPT: &str = "Base branch:";
const CONTROL_N: char = '\u{e}';
const CONTROL_P: char = '\u{10}';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectorAction {
    Next,
    Previous,
    Confirm,
    Ignore,
}

const SHELL_INIT: &str = r#"wt() {
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

    let cwd = env::current_dir().map_err(|error| format!("failed to determine cwd: {error}"))?;
    let base = prompt_worktree_base(&cwd)?;
    let worktree_name = prompt_worktree_name()?;
    let outcome = create_and_setup_worktree_from_base(&cwd, &worktree_name, &base)?;

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

fn prompt_worktree_base(cwd: &Path) -> Result<WorktreeBase, String> {
    if !io::stdin().is_terminal() {
        return Ok(WorktreeBase::OriginMain);
    }

    let bases = list_worktree_bases(cwd, RECENT_LOCAL_BRANCH_LIMIT)?;
    let selected_index = interact_with_base_selector(&bases)?;

    Ok(bases[selected_index].clone())
}

fn interact_with_base_selector(bases: &[WorktreeBase]) -> Result<usize, String> {
    if bases.is_empty() {
        return Err("cannot select a base branch from an empty list".to_string());
    }

    let term = Term::stderr();
    term.hide_cursor()
        .map_err(|error| format!("failed to hide cursor for base branch selector: {error}"))?;
    let selection_result = run_base_selector(&term, bases);
    term.show_cursor()
        .map_err(|error| format!("failed to restore cursor after base branch selector: {error}"))?;

    selection_result
}

fn run_base_selector(term: &Term, bases: &[WorktreeBase]) -> Result<usize, String> {
    let mut selected_index = 0;

    render_base_selector(term, bases, selected_index)?;

    loop {
        let key = term
            .read_key()
            .map_err(|error| format!("failed to read base branch selection: {error}"))?;
        let action = selector_action(&key);

        if action == SelectorAction::Confirm {
            clear_base_selector(term, bases.len())?;
            term.write_line(&format!("{BASE_SELECTOR_PROMPT} {}", bases[selected_index]))
                .map_err(|error| format!("failed to report base branch selection: {error}"))?;
            term.flush()
                .map_err(|error| format!("failed to flush base branch selection: {error}"))?;

            return Ok(selected_index);
        }

        let updated_index = updated_selection_index(selected_index, bases.len(), action);
        if updated_index == selected_index {
            continue;
        }

        selected_index = updated_index;
        clear_base_selector(term, bases.len())?;
        render_base_selector(term, bases, selected_index)?;
    }
}

fn render_base_selector(
    term: &Term,
    bases: &[WorktreeBase],
    selected_index: usize,
) -> Result<(), String> {
    term.write_line(BASE_SELECTOR_PROMPT)
        .map_err(|error| format!("failed to render base branch selector: {error}"))?;

    for (index, base) in bases.iter().enumerate() {
        let marker = if index == selected_index { '>' } else { ' ' };
        term.write_line(&format!("{marker} {base}"))
            .map_err(|error| format!("failed to render base branch selector: {error}"))?;
    }

    term.flush()
        .map_err(|error| format!("failed to flush base branch selector: {error}"))
}

fn clear_base_selector(term: &Term, base_count: usize) -> Result<(), String> {
    term.clear_last_lines(base_count + 1)
        .map_err(|error| format!("failed to redraw base branch selector: {error}"))
}

fn selector_action(key: &Key) -> SelectorAction {
    match key {
        Key::ArrowDown | Key::Tab | Key::Char('j' | CONTROL_N) => SelectorAction::Next,
        Key::ArrowUp | Key::BackTab | Key::Char('k' | CONTROL_P) => SelectorAction::Previous,
        Key::Enter | Key::Char(' ') => SelectorAction::Confirm,
        _ => SelectorAction::Ignore,
    }
}

fn updated_selection_index(
    selected_index: usize,
    item_count: usize,
    action: SelectorAction,
) -> usize {
    match action {
        SelectorAction::Next => (selected_index + 1) % item_count,
        SelectorAction::Previous => (selected_index + item_count - 1) % item_count,
        SelectorAction::Confirm | SelectorAction::Ignore => selected_index,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_n_moves_to_the_next_selection() {
        assert_eq!(selector_action(&Key::Char(CONTROL_N)), SelectorAction::Next);
    }

    #[test]
    fn control_p_moves_to_the_previous_selection() {
        assert_eq!(
            selector_action(&Key::Char(CONTROL_P)),
            SelectorAction::Previous
        );
    }

    #[test]
    fn selector_navigation_wraps_at_both_ends() {
        assert_eq!(updated_selection_index(2, 3, SelectorAction::Next), 0);
        assert_eq!(updated_selection_index(0, 3, SelectorAction::Previous), 2);
    }
}
