use clap::Parser;
use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};
use worktree_manager::{
    BACKGROUND_SETUP_WORKER_COMMAND, CONFIG_FILE_NAME, WorktreeBase,
    create_and_setup_worktree_from_base, current_worktree_base, generate_random_name,
    list_worktree_bases, resolve_worktree_name, run_background_setup_worker,
    start_background_setup,
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
    match cli.command {
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

    let cwd = env::current_dir().map_err(|error| format!("failed to determine cwd: {error}"))?;
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

    fn shell_test_command(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script, "fzf"]);
        command
    }
}
