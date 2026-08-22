# worktree-manager

`worktree-manager` is a small CLI for creating Git worktrees for AI agent work.
Run it from inside a Git work tree, choose a base branch, and it creates a
sibling worktree with a random matching branch before running optional setup
commands. Pass `--name` when you want to choose that name yourself.

## Installation

Install the `worktree-manager` command from this repository:

```sh
cargo install --path .
```

Cargo installs the compiled binary to `~/.cargo/bin/worktree-manager`. Ensure
that directory and an `fzf` installation are on your `PATH`, then run
`worktree-manager` from any Git work tree.

## Shell Integration

A standalone CLI cannot change its parent shell's current directory. To enter
each newly created worktree automatically, add this to your zsh or bash startup
file:

```sh
eval "$(command worktree-manager shell-init)"
```

After opening a new shell, `wt` creates the worktree and changes into it when
the command succeeds. The wrapper invokes `command worktree-manager`, so the
underlying binary remains unambiguous and can be run directly without the
directory change.

## Usage

```sh
worktree-manager
```

The CLI first opens `fzf` with every local branch, ordered by the most recent
tip-commit activity:

```text
╭──────────────────────────────╮
│ Base branch:                 │
│ > recent-local-branch        │
│   another-local-branch       │
│   main                       │
╰──────────────────────────────╯
```

Type to filter the branches, move through matches with the arrow keys or
`Ctrl+N`/`Ctrl+P`, and press Enter to confirm the highlighted branch. Press
Escape or `Ctrl+C` to cancel without creating a worktree. The branch checked
out in the work tree where the command is running is highlighted by default
while the list remains ordered by recent activity. Once you start or change a
search, the highlighted cursor moves to the top match.

The branch list uses the repository's existing local refs and does not include
or fetch remote branches. When stdin is not a terminal, the CLI skips the
selector and uses `origin/main`.

The CLI then generates a random 10-character slug for the worktree name; it
does not prompt for one. Generated names do not use a fixed prefix.

To explicitly choose a name, pass `--name` (or `-n`) when invoking the command:

```sh
worktree-manager --name fix-login
```

This creates:

```text
.worktrees/fix-login
```

Worktrees are created inside the repository's `.worktrees/` directory. The CLI
adds `/.worktrees/` to the repository's local `.git/info/exclude`, so generated
worktrees do not appear as untracked files without changing the repository's
tracked `.gitignore`.

## Cleanup

Remove worktrees this tool created with:

```sh
worktree-manager clean
```

The CLI opens `fzf` with every worktree under `.worktrees/`, showing each worktree name
next to the branch checked out in it:

```text
╭─────────────────────────────────────────────╮
│ Remove worktrees:                           │
│ Tab selects multiple                        │
│ > cleanup-demo  cleanup-demo                │
│   fpf4hmert5    eq/feat/fzf-local-branches  │
╰─────────────────────────────────────────────╯
```

Mark several worktrees with Tab, press Enter, then answer the confirmation:

```text
/Users/you/project/.worktrees/cleanup-demo
Remove 1 worktree(s)? [y/N] y
removed worktree 'cleanup-demo'
deleted branch 'cleanup-demo'
```

Only `y` or `Y` proceeds; anything else cancels, as does Escape or `Ctrl+C` in the
selector. The highlighted cursor starts on the worktree the command runs from. Removing
the worktree you are standing in is allowed, and the shell stays in the deleted directory
until you `cd` elsewhere. The `wt` wrapper forwards subcommands, so `wt clean` works too
and leaves the shell where it is.

Pass `--name` (or `-n`) to skip the selector, repeating it to remove several at once:

```sh
worktree-manager clean --name cleanup-demo --name fix-login
```

`--name` is required when stdin is not a terminal. The confirmation is skipped there as
well, so scripted runs never block.

Removing a worktree deletes its directory, its branch, and its background setup log and
status files. Git decides what is safe to delete:

- `git worktree remove` refuses a worktree that holds uncommitted changes or untracked
  files.
- `git branch -d` refuses a branch that holds unmerged commits, so the branch outlives its
  worktree and the CLI reports it as kept:

```text
removed worktree 'fix-login'
kept branch 'fix-login': the branch 'fix-login' is not fully merged
```

`--force` overrides both, removing dirty worktrees and force-deleting their branches. A
refusal never stops the rest of the selection; the CLI reports each one and exits nonzero
once the remaining worktrees are removed.

The primary checkout and worktrees living outside `.worktrees/` are never listed or
removed. Before listing, `clean` runs `git worktree prune`, so worktrees whose directories
were deleted by hand stop appearing in `git worktree list`.

## Setup Config

Place `agent-worktree.config.json` at the repository root:

```json
{
  "setupCommands": [
    "cp \"$WORKTREE_MANAGER_REPO_ROOT/.env\" ./.env"
  ],
  "backgroundSetupCommands": ["cargo fetch"]
}
```

`setupCommands` run sequentially in the new worktree through the platform shell.
Command output streams directly to the terminal. If a command fails, the CLI
returns a nonzero exit code and leaves the worktree in place for inspection.

`backgroundSetupCommands` also run sequentially, but in a detached worker after
all foreground setup commands succeed. This is useful for slow dependency
operations such as `cargo fetch`, `npm install`, or `pnpm install`. The CLI
returns as soon as the worker starts, allowing the `wt` shell integration to
enter the new worktree while dependencies install.

Background command output is written to a log under the repository's Git
metadata instead of being mixed into the terminal. The CLI prints the log and
status paths when it starts the worker. The status file contains `pending`,
`running`, `succeeded`, or `failed`; on failure, it also includes the error.
Because failures can happen after the CLI returns, they do not change the
worktree creation command's exit code.

Each setup command receives `WORKTREE_MANAGER_REPO_ROOT`, the absolute path of
the primary repository checkout from which the worktree was created. Use it to
copy untracked local files without hardcoding a checkout path:

```json
{
  "setupCommands": [
    "cp \"$WORKTREE_MANAGER_REPO_ROOT/.env\" ./.env"
  ]
}
```

The config file is optional. If it is missing, the CLI prints a warning and skips
setup commands. If it exists but cannot be parsed, the CLI fails before creating
the worktree.

## Failure Behavior

Worktree creation fails before creating a worktree when:

- the current directory is not inside a Git work tree
- the repository has no commits
- `fzf` is unavailable for interactive branch selection
- the repository has no local branches to select
- the selected base branch does not exist or has no commits
- the requested branch already exists
- the target sibling path already exists
- `agent-worktree.config.json` exists but is invalid

Worktrees are created with:

```sh
git worktree add -b <worktree-name> .worktrees/<worktree-name> <base-ref>
```

Cleanup fails when:

- the current directory is not inside a Git work tree
- `--name` names a worktree that is not under `.worktrees/`
- stdin is not a terminal and no `--name` is given
- `fzf` is unavailable for interactive selection
- Git refuses to remove a selected worktree, which is reported per worktree while the
  rest of the selection still proceeds
