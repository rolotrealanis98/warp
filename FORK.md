# About this fork

This is a personal fork of [Warp](https://github.com/warpdotdev/warp) focused on
third-party CLI coding agents (Claude Code first; Codex and Copilot CLI later).
Everything added here is opt-in configuration that any user can enable. No
private data, prompts, or credentials live in this repository.

## What the fork adds

Each feature is behind its own `FeatureFlag` and Cargo feature. The umbrella
Cargo feature `fork_features` turns them all on and is part of `default`.

| Feature flag        | Cargo feature         | What it does |
|---------------------|-----------------------|--------------|
| `CliAgentChatView`  | `cli_agent_chat_view` | Toggle a running CLI agent pane between the terminal rendering and a chat-style rendering of the same session. |
| `TaskAgentLauncher` | `task_agent_launcher` | "Start agent on task": optional worktree / branch creation from templates, per-repo setup commands, session naming `KEY short-title`. |
| `JiraIntegration`   | `jira_integration`    | Jira issue picker (command palette) feeding the task launcher. Site URL and email in settings, API token in the OS keychain. |
| `PrReviewAgent`     | `pr_review_agent`     | Check out a PR, launch an agent with a user-supplied review prompt, watch the PR for commits / reviews / checks. |
| `PrStackView`       | `pr_stack_view`       | Left-panel ledger of a branch stack: stack-relative diffs with line classification, create PR for the next branch, restack after merges. |

### CLI agent chat view

While Claude Code runs in a pane, switch that pane to a chat rendering with
`Cmd/Ctrl-Shift-L`, the **Chat** button in the CLI agent footer, or the pane
header menu. The chat is read from Claude Code's own session transcript
(`~/.claude/projects/...`, or `$CLAUDE_CONFIG_DIR`); the composer and the key
strip write to the same PTY, so the session keeps running unchanged. The
**Terminal** button (or the same shortcut) switches back. The transcript path
is found most reliably with the Warp plugin for Claude Code installed.

Settings (Settings > Third party CLI agents, or `settings.toml`):

| Key | Default | Meaning |
|-----|---------|---------|
| `cli_chat_view.open_on_session_start` | `false` | Switch to the chat view when a Claude Code session starts. |
| `cli_chat_view.collapse_thinking` | `true` | Thinking blocks start collapsed. |
| `cli_chat_view.collapse_tool_output` | `true` | Successful tool calls start collapsed. |
| `cli_chat_view.show_timestamps` | `false` | Show message times. |

### Task agent launcher

Command palette:

- **Task agent: start on task…** opens a form: key, title, repository, where the
  agent works (new worktree / new branch in the current checkout / current
  checkout as is), base branch (defaults to the remote's default branch, fetched
  first), branch name, agent CLI, initial prompt, and whether to run setup
  commands. "Current checkout" has no git side effects.
- **Task agent: rename this session from task…** names the current tab after a
  key and title without launching anything.
- **Task agent: open settings** (Settings > Agents > Task agents).

The tab is titled from `session_title_template` and the pane header shows the
task key as a chip (click to copy). Claude Code and Codex receive the initial
prompt as their first argument, read from a file in Warp's cache directory;
other agents get it typed into their input once their session starts.

Settings (`task_agents.*` in `settings.toml`, never synced):

| Key | Default |
|-----|---------|
| `branch_template` | `{{type}}/{{key}}-{{slug}}` |
| `worktree_path_template` | `{{repo_parent}}/{{repo}}.worktrees/{{key}}-{{slug}}` |
| `default_cli` | `claude` |
| `push_on_create` | `true` |
| `fetch_before_branch` | `true` |
| `prompt_template` | `Work on this task: {{key}} {{title}}` + body + url |
| `session_title_template` | `{{key}} {{short_title}}` |
| `short_title_max_chars` | `32` |
| `prefer_agent_title` | `false` |
| `per_repo` | `{}` |

Per-repository overrides:

```toml
[task_agents.per_repo."/path/to/octo/repo"]
branch_template = "{{key}}/{{slug}}"
setup_commands = ["npm ci"]
default_cli = "codex"
```

Build without the fork features:

```sh
cargo build -p warp --no-default-features --features "<upstream default list>"
```

or disable a single feature by removing it from `fork_features` in `app/Cargo.toml`.

## PR stack view

Left panel > branch icon. Shows the stack under the focused terminal's repo:
the checked-out branch and the local branches below it down to the target
(origin default branch unless overridden), inferred from git ancestry.

- Row click: code review panel diffed against the branch's parent (the
  branch must be checked out in some worktree).
- Right click: Create PR, Open PR, Sync, Restack from here, pins, copy name.
- Create PR reads the title (first line) and body from `pr_body_file_template`
  (default `.git/warp-pr/{{branch}}.md`). If missing and `pr_prepare_command`
  is set, that text is sent to the branch's agent pane and Warp waits for the
  agent to stop; otherwise a template is used: title `KEY short title` from
  the task (or the branch name / first commit), body from the commit
  messages. A stack footer is kept up to date in every open PR of the stack.
- "The branch's agent pane" is the pane the task agent launcher started on
  that branch; for panes started by hand, the agent pane working in the
  branch's worktree (or the repo) in the active tab.
- Restacks rebase with `git rebase --onto`, force-push (with lease) branches
  that have PRs, and retarget a PR whose parent merged. Conflicts stop the run
  and the hand-off is sent to the agent pane; nothing is resolved by Warp.
- State lives in `.git/warp-stack.json` (untracked): `target`, `pins`
  (child -> parent), and the last known `parents`.

Settings (`[pr_stack]` in the settings file, most also on Settings > PR stack):
`targets` (repo root path -> target branch), `classification` (ordered
`{ pattern, bucket }` rules, bucket = tests | docs | config),
`pr_body_file_template`, `pr_prepare_command`, `auto_restack`,
`poll_interval_secs`, `row_order`.

## Warp's own agent

Warp's native agent, Active AI and related UI are controlled by the existing
setting `agents.warp_agent.is_any_ai_enabled` (Settings > Warp Agent > the
global toggle). Turn it off to hide them. Upstream onboarding sets it to `true`
when an account is created, so the fork does not change the default; the
switch is one click. CLI agent features (footer, session detection, the
features above) do not depend on it.

## Branching and upstream sync

- `master` mirrors `upstream/master` and is never committed to directly.
- `fork/main` is the integration branch; releases build from it.
- Each feature lives on its own branch off `fork/main` and merges back via PR.

Sync recipe:

```sh
git fetch upstream
git switch master && git merge --ff-only upstream/master && git push origin master
git switch fork/main && git merge master   # resolve conflicts, then push
```

## Privacy rules for contributions

- No real issue keys, site URLs, repository names, or prompt content in code,
  tests, fixtures, screenshots, or commit messages. Fixtures use `EXAMPLE-123`,
  `example.atlassian.net`, `octo/repo`.
- Credentials go in the OS keychain. Settings files hold only non-secret config.
- Prompt templates ship with generic defaults; personal versions stay in local
  settings.
