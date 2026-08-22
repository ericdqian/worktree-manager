use clap::Parser;
use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};
use worktree_manager::{
    BACKGROUND_SETUP_WORKER_COMMAND, BranchOutcome, CONFIG_FILE_NAME, ManagedWorktree,
    WorktreeBase, create_and_setup_worktree_from_base, current_worktree_base, generate_random_name,
    list_managed_worktrees, list_worktree_bases, main_worktree_root, prune_worktree_metadata,
    remove_worktree, resolve_worktree_name, run_background_setup_worker, start_background_setup,
};

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

    /// Name the new worktree instead of generating one.
    #[arg(short, long, value_name = "NAME")]
    name: Option<String>,

    #[arg(long, hide = true, value_name = "PATH")]
    created_path_file: Option<std::path::PathBuf>,
}

#[derive(clap::Subcommand)]
enum CliCommand {
    /// Remove worktrees created by this tool, along with their branches.
    Clean {
        /// Remove the named worktree instead of selecting one interactively.
        #[arg(short, long, value_name = "NAME")]
        name: Vec<String>,

        /// Remove worktrees with uncommitted changes and delete unmerged branches.
        #[arg(long)]
        force: bool,
    },

    /// Print the zsh/bash integration script.
    ShellInit,

    #[command(name = BACKGROUND_SETUP_WORKER_COMMAND, hide = true)]
    RunBackgroundSetup {
        #[arg(long)]
        repository_root: PathBuf,

        #[arg(long)]
        worktree_path: PathBuf,

        #[arg(long)]
        status_path: PathBuf,

        #[arg(long = "setup-command", required = true)]
        setup_commands: Vec<String>,
    },
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
    let cwd = env::current_dir().map_err(|error| format!("failed to determine cwd: {error}"))?;

    match cli.command {
        Some(CliCommand::Clean { name, force }) => {
            return run_clean(&cwd, &name, force);
        }
        Some(CliCommand::ShellInit) => {
            println!("{SHELL_INIT}");
            return Ok(());
        }
        Some(CliCommand::RunBackgroundSetup {
            repository_root,
            worktree_path,
            status_path,
            setup_commands,
        }) => {
            run_background_setup_worker(
                &repository_root,
                &worktree_path,
                &status_path,
                &setup_commands,
            )?;
            return Ok(());
        }
        None => {}
    }

    let Some(base) = prompt_worktree_base(&cwd)? else {
        return Ok(());
    };
    let worktree_name = cli
        .name
        .as_deref()
        .map(resolve_worktree_name)
        .unwrap_or_else(generate_random_name);
    let outcome = create_and_setup_worktree_from_base(&cwd, &worktree_name, &base)?;
    let background_setup = outcome
        .background_setup
        .as_ref()
        .map(|setup| {
            let worker_executable = env::current_exe()
                .map_err(|error| format!("failed to locate current executable: {error}"))?;
            let worker_pid = start_background_setup(&worker_executable, setup)?;

            Ok::<_, String>((setup, worker_pid))
        })
        .transpose()?;

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

    if let Some((setup, worker_pid)) = background_setup {
        println!(
            "started {} background setup command(s) (pid {worker_pid})",
            setup.commands.len()
        );
        println!("setup status: {}", setup.status_path.display());
        println!("setup log: {}", setup.log_path.display());
    }

    Ok(())
}

fn prompt_worktree_base(cwd: &Path) -> Result<Option<WorktreeBase>, String> {
    if !io::stdin().is_terminal() {
        return Ok(Some(WorktreeBase::OriginMain));
    }

    let bases = list_worktree_bases(cwd)?;
    let current_base = current_worktree_base(cwd)?;
    select_worktree_base_with_fzf(&bases, current_base.as_ref())
}

fn select_worktree_base_with_fzf(
    bases: &[WorktreeBase],
    default_base: Option<&WorktreeBase>,
) -> Result<Option<WorktreeBase>, String> {
    let mut fzf_command = Command::new("fzf");
    run_fzf_base_selector(bases, default_base, &mut fzf_command)
}

fn run_fzf_base_selector(
    bases: &[WorktreeBase],
    default_base: Option<&WorktreeBase>,
    fzf_command: &mut Command,
) -> Result<Option<WorktreeBase>, String> {
    if bases.is_empty() {
        return Err(
            "cannot select a base branch because the repository has no local branches".to_string(),
        );
    }

    fzf_command.args([
        "--height=40%",
        "--layout=reverse",
        "--border",
        "--no-multi",
        "--prompt=Base branch: ",
        "--bind=change:first",
    ]);

    if let Some(position) =
        default_base.and_then(|default_base| bases.iter().position(|base| base == default_base))
    {
        fzf_command.arg(format!("--bind=load:pos({})", position + 1));
    }

    let mut fzf = fzf_command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                "fzf is required for interactive branch selection; install fzf and try again"
                    .to_string()
            } else {
                format!("failed to start fzf for base branch selection: {error}")
            }
        })?;

    let write_result: Result<(), String> = (|| {
        let mut fzf_input = fzf
            .stdin
            .take()
            .ok_or_else(|| "failed to open fzf input".to_string())?;

        for base in bases {
            writeln!(fzf_input, "{base}")
                .map_err(|error| format!("failed to send branches to fzf: {error}"))?;
        }

        Ok(())
    })();

    let output = fzf
        .wait_with_output()
        .map_err(|error| format!("failed to wait for fzf base branch selection: {error}"))?;

    if output.status.code() == Some(130) {
        return Ok(None);
    }

    if !output.status.success() {
        return Err(format!(
            "fzf base branch selection failed with status {}",
            output.status
        ));
    }

    write_result?;

    let selected_branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    bases
        .iter()
        .find(|base| base.to_string() == selected_branch)
        .cloned()
        .map(Some)
        .ok_or_else(|| format!("fzf returned unknown base branch '{selected_branch}'"))
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

fn run_clean(cwd: &Path, names: &[String], force: bool) -> Result<(), String> {
    let repository_root = main_worktree_root(cwd)?;
    prune_worktree_metadata(&repository_root)?;

    let worktrees = list_managed_worktrees(&repository_root)?;
    if worktrees.is_empty() {
        println!("no worktrees to clean up");
        return Ok(());
    }

    let selected_worktrees = select_worktrees_to_remove(cwd, &worktrees, names)?;
    if selected_worktrees.is_empty() || !confirm_removal(&selected_worktrees, force)? {
        return Ok(());
    }

    remove_selected_worktrees(&repository_root, &selected_worktrees, force)
}

fn select_worktrees_to_remove(
    cwd: &Path,
    worktrees: &[ManagedWorktree],
    names: &[String],
) -> Result<Vec<ManagedWorktree>, String> {
    if !names.is_empty() {
        return names
            .iter()
            .map(|name| find_worktree_by_name(worktrees, name))
            .collect();
    }

    if !io::stdin().is_terminal() {
        return Err(
            "cannot select worktrees without a terminal; pass --name to choose one".to_string(),
        );
    }

    let current_directory = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let current_worktree = worktrees
        .iter()
        .find(|worktree| current_directory.starts_with(&worktree.path));

    select_worktrees_with_fzf(worktrees, current_worktree)
}

fn find_worktree_by_name(
    worktrees: &[ManagedWorktree],
    name: &str,
) -> Result<ManagedWorktree, String> {
    worktrees
        .iter()
        .find(|worktree| worktree.name == name)
        .cloned()
        .ok_or_else(|| format!("no worktree named '{name}' in .worktrees/"))
}

fn select_worktrees_with_fzf(
    worktrees: &[ManagedWorktree],
    current_worktree: Option<&ManagedWorktree>,
) -> Result<Vec<ManagedWorktree>, String> {
    let mut fzf_command = Command::new("fzf");
    run_fzf_worktree_selector(worktrees, current_worktree, &mut fzf_command)
}

fn run_fzf_worktree_selector(
    worktrees: &[ManagedWorktree],
    current_worktree: Option<&ManagedWorktree>,
    fzf_command: &mut Command,
) -> Result<Vec<ManagedWorktree>, String> {
    fzf_command.args([
        "--height=40%",
        "--layout=reverse",
        "--border",
        "--multi",
        "--prompt=Remove worktrees: ",
        "--header=Tab selects multiple",
        "--bind=change:first",
    ]);

    if let Some(position) = current_worktree
        .and_then(|current_worktree| worktrees.iter().position(|w| w == current_worktree))
    {
        fzf_command.arg(format!("--bind=load:pos({})", position + 1));
    }

    let rows = worktree_rows(worktrees);
    let mut fzf = fzf_command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                "fzf is required for interactive worktree selection; install fzf and try again"
                    .to_string()
            } else {
                format!("failed to start fzf for worktree selection: {error}")
            }
        })?;

    let write_result: Result<(), String> = (|| {
        let mut fzf_input = fzf
            .stdin
            .take()
            .ok_or_else(|| "failed to open fzf input".to_string())?;

        for (row, _) in &rows {
            writeln!(fzf_input, "{row}")
                .map_err(|error| format!("failed to send worktrees to fzf: {error}"))?;
        }

        Ok(())
    })();

    let output = fzf
        .wait_with_output()
        .map_err(|error| format!("failed to wait for fzf worktree selection: {error}"))?;

    if output.status.code() == Some(130) {
        return Ok(Vec::new());
    }

    if !output.status.success() {
        return Err(format!(
            "fzf worktree selection failed with status {}",
            output.status
        ));
    }

    write_result?;

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|selected_row| {
            rows.iter()
                .find(|(row, _)| row == selected_row)
                .map(|(_, worktree)| (*worktree).clone())
                .ok_or_else(|| format!("fzf returned unknown worktree '{selected_row}'"))
        })
        .collect()
}

fn worktree_rows(worktrees: &[ManagedWorktree]) -> Vec<(String, &ManagedWorktree)> {
    let name_width = worktrees
        .iter()
        .map(|worktree| worktree.name.len())
        .max()
        .unwrap_or_default();

    worktrees
        .iter()
        .map(|worktree| {
            let branch = worktree.branch.as_deref().unwrap_or("(detached)");

            (format!("{:name_width$}  {branch}", worktree.name), worktree)
        })
        .collect()
}

fn confirm_removal(worktrees: &[ManagedWorktree], force: bool) -> Result<bool, String> {
    // Scripted runs pass --name explicitly, so only a terminal session is asked to confirm.
    if !io::stdin().is_terminal() {
        return Ok(true);
    }

    for worktree in worktrees {
        println!("{}", worktree.path.display());
    }

    let force_note = if force { " (force)" } else { "" };
    print!("Remove {} worktree(s){force_note}? [y/N] ", worktrees.len());
    io::stdout()
        .flush()
        .map_err(|error| format!("failed to write confirmation prompt: {error}"))?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("failed to read confirmation: {error}"))?;

    Ok(confirmation_is_affirmative(&answer))
}

fn confirmation_is_affirmative(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y")
}

fn remove_selected_worktrees(
    repository_root: &Path,
    worktrees: &[ManagedWorktree],
    force: bool,
) -> Result<(), String> {
    let mut failures = 0;

    for worktree in worktrees {
        match remove_worktree(repository_root, worktree, force) {
            Ok(branch_outcome) => {
                println!("removed worktree '{}'", worktree.name);

                if let Some(message) = branch_outcome_message(&branch_outcome) {
                    println!("{message}");
                }
            }
            // Keep going so one blocked worktree does not strand the rest of the selection.
            Err(error) => {
                eprintln!("error: {error}");
                failures += 1;
            }
        }
    }

    if failures > 0 {
        return Err(format!(
            "failed to remove {failures} of {} worktree(s)",
            worktrees.len()
        ));
    }

    Ok(())
}

fn branch_outcome_message(branch_outcome: &BranchOutcome) -> Option<String> {
    match branch_outcome {
        BranchOutcome::Deleted(branch) => Some(format!("deleted branch '{branch}'")),
        BranchOutcome::Kept { branch, reason } => Some(format!("kept branch '{branch}': {reason}")),
        BranchOutcome::NoBranch => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fzf_selects_a_local_branch() {
        let bases = vec![
            WorktreeBase::LocalBranch("main".to_string()),
            WorktreeBase::LocalBranch("feature-a".to_string()),
        ];
        let mut command = shell_test_command("sed -n '2p'");

        assert_eq!(
            run_fzf_base_selector(&bases, None, &mut command).unwrap(),
            Some(WorktreeBase::LocalBranch("feature-a".to_string()))
        );
    }

    #[test]
    fn fzf_defaults_to_the_current_local_branch() {
        let bases = vec![
            WorktreeBase::LocalBranch("newest".to_string()),
            WorktreeBase::LocalBranch("current".to_string()),
            WorktreeBase::LocalBranch("oldest".to_string()),
        ];
        let mut command = shell_test_command(
            r#"for argument do
                if [ "$argument" = "--bind=load:pos(2)" ]; then
                    sed -n '2p'
                    exit
                fi
            done
            exit 1"#,
        );

        assert_eq!(
            run_fzf_base_selector(&bases, Some(&bases[1]), &mut command).unwrap(),
            Some(WorktreeBase::LocalBranch("current".to_string()))
        );
    }

    #[test]
    fn fzf_moves_to_the_first_match_when_the_query_changes() {
        let bases = vec![
            WorktreeBase::LocalBranch("newest".to_string()),
            WorktreeBase::LocalBranch("current".to_string()),
        ];
        let mut command = shell_test_command(
            r#"for argument do
                if [ "$argument" = "--bind=change:first" ]; then
                    sed -n '1p'
                    exit
                fi
            done
            exit 1"#,
        );

        assert_eq!(
            run_fzf_base_selector(&bases, Some(&bases[1]), &mut command).unwrap(),
            Some(WorktreeBase::LocalBranch("newest".to_string()))
        );
    }

    #[test]
    fn cancelling_fzf_cancels_branch_selection() {
        let bases = vec![WorktreeBase::LocalBranch("main".to_string())];
        let mut command = shell_test_command("exit 130");

        assert_eq!(
            run_fzf_base_selector(&bases, None, &mut command).unwrap(),
            None
        );
    }

    #[test]
    fn fzf_cannot_select_an_unknown_branch() {
        let bases = vec![WorktreeBase::LocalBranch("main".to_string())];
        let mut command = shell_test_command("printf 'missing\\n'");

        assert_eq!(
            run_fzf_base_selector(&bases, None, &mut command).unwrap_err(),
            "fzf returned unknown base branch 'missing'"
        );
    }

    #[test]
    fn missing_fzf_reports_how_to_resolve_the_dependency() {
        let bases = vec![WorktreeBase::LocalBranch("main".to_string())];
        let mut command = Command::new("/path/that/does/not/exist/fzf");

        assert_eq!(
            run_fzf_base_selector(&bases, None, &mut command).unwrap_err(),
            "fzf is required for interactive branch selection; install fzf and try again"
        );
    }

    #[test]
    fn fzf_selects_multiple_worktrees() {
        let worktrees = managed_worktrees();
        let mut command = shell_test_command(
            r#"for argument do
                if [ "$argument" = "--multi" ]; then
                    sed -n '1p;2p'
                    exit
                fi
            done
            exit 1"#,
        );

        assert_eq!(
            run_fzf_worktree_selector(&worktrees, None, &mut command).unwrap(),
            worktrees
        );
    }

    #[test]
    fn fzf_defaults_to_the_worktree_the_command_runs_from() {
        let worktrees = managed_worktrees();
        let mut command = shell_test_command(
            r#"for argument do
                if [ "$argument" = "--bind=load:pos(2)" ]; then
                    sed -n '2p'
                    exit
                fi
            done
            exit 1"#,
        );

        assert_eq!(
            run_fzf_worktree_selector(&worktrees, Some(&worktrees[1]), &mut command).unwrap(),
            vec![worktrees[1].clone()]
        );
    }

    #[test]
    fn cancelling_fzf_selects_no_worktrees() {
        let mut command = shell_test_command("exit 130");

        assert_eq!(
            run_fzf_worktree_selector(&managed_worktrees(), None, &mut command).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn fzf_cannot_select_an_unknown_worktree() {
        let mut command = shell_test_command("printf 'missing\\n'");

        assert_eq!(
            run_fzf_worktree_selector(&managed_worktrees(), None, &mut command).unwrap_err(),
            "fzf returned unknown worktree 'missing'"
        );
    }

    #[test]
    fn removal_is_confirmed_only_by_an_explicit_yes() {
        assert!(confirmation_is_affirmative("y\n"));
        assert!(confirmation_is_affirmative("Y\n"));

        for answer in ["", "\n", "n\n", "yes\n", "no\n"] {
            assert!(!confirmation_is_affirmative(answer), "{answer:?} confirmed");
        }
    }

    fn managed_worktrees() -> Vec<ManagedWorktree> {
        vec![
            ManagedWorktree {
                name: "feature-a".to_string(),
                path: PathBuf::from("/repo/.worktrees/feature-a"),
                branch: Some("eq/feat/a".to_string()),
            },
            ManagedWorktree {
                name: "b".to_string(),
                path: PathBuf::from("/repo/.worktrees/b"),
                branch: None,
            },
        ]
    }

    fn shell_test_command(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script, "fzf"]);
        command
    }
}
