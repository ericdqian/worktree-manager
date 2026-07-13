# worktree-manager

`worktree-manager` is a small CLI for creating Git worktrees for AI agent work.
Run it from inside a Git work tree, choose a worktree name, and it creates a
sibling worktree with a matching branch before running optional setup commands.

## Installation

Install the `wt` command from this repository:

```sh
cargo install --path .
```

Cargo installs the compiled binary to `~/.cargo/bin/wt`. Ensure that directory
is on your `PATH`, then run `wt` from any Git work tree.

## Usage

```sh
wt
```

The CLI prompts for a worktree name:

```text
Worktree name (blank for random):
```

Enter a path-safe name such as `fix-login` to create:

```text
../<repo-name>-fix-login
```

Press Enter without a name to generate a random 10-character slug. Generated
names do not use a fixed prefix.

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

The config file is optional. If it is missing, the CLI prints a warning and skips
setup commands. If it exists but cannot be parsed, the CLI fails before creating
the worktree.

## Failure Behavior

The CLI fails before creating a worktree when:

- the current directory is not inside a Git work tree
- the repository has no commits
- the requested branch already exists
- the target sibling path already exists
- `agent-worktree.config.json` exists but is invalid

Worktrees are created with:

```sh
git worktree add -b <worktree-name> ../<repo-name>-<worktree-name>
```
