# worktree-manager

`worktree-manager` is a small CLI for creating Git worktrees for AI agent work.
Run it from inside a Git work tree, choose a base branch and worktree name, and
it creates a sibling worktree with a matching branch before running optional
setup commands.

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
while the list remains ordered by recent activity.

The branch list uses the repository's existing local refs and does not include
or fetch remote branches. When stdin is not a terminal, the CLI skips the
selector and uses `origin/main`.

The CLI then prompts for a worktree name:

```text
Worktree name (blank for random):
```

Enter a path-safe name such as `fix-login` to create:

```text
.worktrees/fix-login
```

Press Enter without a name to generate a random 10-character slug. Generated
names do not use a fixed prefix.

Worktrees are created inside the repository's `.worktrees/` directory. The CLI
adds `/.worktrees/` to the repository's local `.git/info/exclude`, so generated
worktrees do not appear as untracked files without changing the repository's
tracked `.gitignore`.

## Setup Config

Place `agent-worktree.config.json` at the repository root:

```json
{
  "setupCommands": ["cargo fetch"]
}
```

`setupCommands` run sequentially in the new worktree through the platform shell.
Command output streams directly to the terminal. If a command fails, the CLI
returns a nonzero exit code and leaves the worktree in place for inspection.

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

The CLI fails before creating a worktree when:

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
