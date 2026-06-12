# Agent PR Orchestration Requirements

## Purpose

This fork should turn `tuicr` from a code-review TUI into the review surface for
an agent-driven pull request workflow.

The target workflow is:

1. A human starts one or more Codex sessions against local projects.
2. Each session creates or updates a draft pull request.
3. The human and automated review tools leave comments on GitHub.
4. This tool discovers actionable comments, dispatches agents to address them,
   updates the code, pushes commits, and replies to the relevant GitHub comments.
5. The tool polls CI for the PR, dispatches agents to fix failing checks when
   failures appear, and keeps watching until checks pass or a human decision is
   needed.
6. The human gets a clear notification when work completes, fails, or needs a
   decision.

`tuicr` should become the single local UI where a human can inspect PRs,
comments, diffs, and active agent work from the terminal. New automation should
build on its existing PR review, remote-comment, review-session, and CLI
surfaces.

Implementation may live directly inside this fork's binary and TUI, but the
agent-orchestration modules should stay isolated enough that routine upstream
rebases remain practical.

## Design Principles

- **Human stays in control.** Default to draft PRs, visible state, and explicit
  notification when agents act or get stuck.
- **GitHub is the source of truth for PR feedback.** Local state may suppress
  duplicate work, but it must not hide newer GitHub comments.
- **Do not lose local work.** Agent runs must preserve unrelated changes and use
  worktrees when a checkout is busy or dirty.
- **Small loops beat large loops.** Each agent run should receive a narrow set of
  comments and a specific PR/head SHA.
- **Block defaults, general extension points.** The fork may ship
  Block-oriented defaults, but repository filters, robot logins, notification
  sinks, and agent commands should be configurable.
- **Agent messages are explicit.** Replies posted on the user's behalf must be
  prefixed with `🤖`.
- **GitHub replies are the durable dedupe signal.** Local state can cache what
  happened, but the latest GitHub comment/reply state decides whether feedback
  still needs work.

## Users

- **Owner/reviewer:** Starts agent work, reviews draft PRs, leaves comments, and
  wants notifications when the next human action is needed.
- **Coding agent:** Receives a PR URL plus specific comment threads, edits code,
  runs focused validation, commits, pushes, and replies.
- **Automated reviewer:** Leaves GitHub PR comments or review-thread comments
  that should be triaged and, when actionable, sent to an agent.

## Core Requirements

### PR Dashboard

- Show a terminal dashboard of relevant pull requests.
- Support filtering by owner, repo, author, draft status, review state, and
  comment/action state.
- Ship PR and comment browsing as a first-class feature of this fork, not only
  as an optional automation add-on.
- Show at least:
  - repository and PR number
  - title
  - draft/open/merged/closed state
  - branch, base branch, and head SHA
  - CI status summary when available
  - unresolved review-thread count
  - unhandled actionable feedback count
  - active agent run status
- Open a selected PR in the existing `tuicr pr <target>` review flow.
- Open the selected PR in a browser or via `gh pr view`.

### GitHub Comment Discovery

- Fetch top-level PR comments and inline review threads.
- Distinguish:
  - unresolved review threads
  - resolved review threads
  - outdated review threads
  - top-level PR comments
  - comments already answered by an agent
- Treat a comment as actionable when:
  - it was authored by the user or a configured robot reviewer
  - it is not known generated metadata
  - it is newer than the latest agent reply in that thread
  - it is still unaddressed on the PR
- Outdated review threads are threads whose original diff anchors no longer map
  cleanly to the current PR diff after later commits. Do not ignore them solely
  because GitHub marks them outdated. Instead, include their original path, hunk,
  original line, current path/line if available, and full body so the agent can
  check whether the feedback is still relevant.
- Ignore common non-actionable metadata such as Graphite stack comments, owner
  routing comments, and dependency footers.
- Persist handled feedback IDs locally, but re-check GitHub before dispatch.
- Expose discovery through a machine-readable CLI command.

### Agent Dispatch

- Dispatch an agent run for one PR and one bounded feedback set.
- Dispatch automatically only for PRs authored by the configured user and only
  for comments authored by that user or configured robot reviewers.
- Include in the prompt:
  - PR URL
  - repository coordinate
  - local checkout or worktree path
  - PR number
  - title
  - branch and base branch
  - head SHA at dispatch time
  - feedback JSON with thread/comment IDs, URLs, file paths, line numbers, and
    full comment bodies
- Require the agent to:
  - inspect live PR state before editing
  - preserve unrelated local changes
  - create or reuse an isolated worktree when needed
  - address only dispatched feedback
  - run focused validation
  - commit and push changes when edits are made
  - reply to every handled comment with the `🤖` prefix
  - explain when no code change is appropriate
- Detect stale PR heads before pushing or replying.
- Mark a feedback item handled only after a successful push/reply or an explicit
  no-change explanation.
- Automatically push agent commits and reply to comments only inside the
  configured ownership boundary.
- Retry transient dispatch, GitHub, push, or notification failures with bounded
  attempts and visible run status. Do not silently loop forever.

### Multiple Agent Runs

- Track multiple concurrent runs across projects.
- Show status per run:
  - queued
  - starting
  - running
  - waiting for user
  - pushed
  - replied
  - failed
  - cancelled
- Allow configurable concurrency globally and per repository.
- Prevent two agents from editing the same PR branch concurrently unless
  explicitly allowed.
- Run local agents in tmux-backed sessions by default so the user can attach to
  live Codex sessions.
- Place agent worktrees according to the repository's existing local convention
  when one can be detected; otherwise fall back to the configured worktree root.
- Record run logs and the final summary for later inspection.

### CI Monitoring And Repair

- Poll CI/check status for watched PRs after each agent push and while a PR has
  pending or failing checks.
- Surface per-check state in the PR dashboard:
  - pending
  - passing
  - failing
  - cancelled
  - skipped
  - unknown
- When a check fails on an owned PR, collect the failing check name, URL, summary,
  annotations, and logs when available, then dispatch an agent to diagnose and
  fix the failure.
- Keep CI-fix dispatch inside the same ownership boundary as review feedback:
  only auto-fix checks for PRs authored by the configured user.
- After pushing a CI fix, continue polling the updated head SHA.
- Bound CI repair retries per check/head SHA and notify the user when the same
  failure repeats after retry.
- Treat missing or inaccessible check logs as a visible blocked state, not as a
  successful repair.
- Use GitHub Checks/Status APIs as the generic source. Allow repo-specific CI
  adapters later for systems with richer logs.

### Review Surface

- Reuse `tuicr`'s existing PR review UI for inspecting PR diffs and remote
  comments.
- Make remote GitHub comments visible alongside local review comments.
- Preserve navigation from the comment sidebar to the corresponding diff anchor.
- Add enough metadata to differentiate:
  - human-authored comments
  - robot-authored comments
  - agent-authored replies
  - local draft `tuicr` comments
- Show agent run status in the same PR-oriented interface rather than requiring
  a separate dashboard for normal use.
- Support actions from the TUI where practical:
  - copy comment/thread URL
  - reply to a thread
  - resolve a thread
  - dispatch an agent for a selected thread
  - dispatch an agent for all actionable PR feedback

### Notifications

- Notify the user when:
  - an agent run starts
  - a run pushes changes
  - a run replies to GitHub comments
  - a run fails
  - a run needs human input
  - all currently actionable feedback is handled
- Initial sinks:
  - terminal status/dashboard
  - desktop notification or local command hook
- Notification messages sent on the user's behalf must use the `🤖` prefix.
- Notifications should include PR URL, repo, run status, and a concise summary.
- Slack notifications are a later extension point, not required for the first
  local workflow.

### Configuration

- Support a config file for:
  - default workspace root
  - repository include/exclude filters
  - GitHub owners/orgs
  - robot reviewer logins
  - ignored comment regexes
  - agent command templates
  - concurrency limits
  - notification sinks
  - preferred worktree root or auto-detected repo worktree convention
  - outdated-thread relevance behavior
  - CI polling interval and retry limits
- Support command-line overrides for one-off runs.
- Keep Block-specific values as defaults in a local profile rather than baking
  them into general GitHub logic.

## Block Workflow Requirements

The first production profile should optimize for the user's Block workflow:

- Default workspace root: `/Users/tbedor/Development`.
- Default GitHub owner filters should include Block-managed organizations such
  as `squareup` when configured locally.
- Account for Graphite-managed PR stacks and ignore Graphite metadata comments.
- Support draft PRs as first-class work items.
- Watch all open PRs authored by the configured user by default.
- Include robot reviewer comments from configured automated tools, including
  `chatgpt-codex-connector[bot]`.
- Preserve the required `🤖` prefix for GitHub, Slack, and other messages sent
  on the user's behalf.
- Work with private GitHub repositories through the authenticated `gh` CLI.
- Keep deployed automation compatible with a GitHub App token flow; local-only
  scripts may use the user's existing `gh` authentication.
- The current fork may remain under the user's account while Block-org repo
  creation is unavailable, but production Block code should still prefer a
  Block-managed destination when permissions allow it.

## CLI Requirements

The CLI should support both human and script workflows.

Candidate command shape:

```bash
tuicr prs list --owner squareup --author @me --json
tuicr prs dashboard --owner squareup --author @me --json
tuicr prs dashboard --owner squareup --author @me --tui
tuicr prs feedback --repo squareup/example --pr 123 --json
tuicr prs checks --repo squareup/example --pr 123 --json
tuicr prs dispatch --repo squareup/example --pr 123
tuicr prs guard-head --repo squareup/example --pr 123 --expected-head-sha abc123
tuicr prs reply --repo squareup/example --pr 123 --feedback-id PRRC_123 --body "Fixed."
tuicr prs resolve --repo squareup/example --pr 123 --thread-id PRRT_123
tuicr prs watch --owner squareup --interval-seconds 300 --max-iterations 2
tuicr prs runs list --json
tuicr prs runs show <run-id>
tuicr prs runs attach <run-id>
tuicr prs runs status <run-id> --status running --message "Inspecting PR"
tuicr prs runs complete <run-id> --exit-code 0
tuicr prs runs cancel <run-id>
```

Existing `tuicr review` commands should keep working. New commands should emit
structured JSON for automation and readable summaries for humans.

### Current Implementation Slice

The first CLI-oriented slice in this fork includes:

- `tuicr prs list`: lists open PRs authored by a user across one or more
  GitHub owners, with filters for repository, draft/ready state, and GitHub
  review state.
- `tuicr prs dashboard`: aggregates authored PRs with actionable feedback
  counts, normalized CI state, failing check counts, and latest local agent run
  status. Dashboard JSON and readable/TUI rows include PR state, draft state,
  head branch, base branch, and head SHA when enrichment succeeds. Dashboard
  supports repository, draft/ready, review-state, and needs-action filters.
- `tuicr prs dashboard --tui`: opens an interactive terminal dashboard. It
  supports keyboard navigation and opens the selected PR through the existing
  `tuicr pr <url>` review flow or `gh pr view --web`. It also supports
  dispatching an agent for the selected PR, attaching to the latest local run,
  cancelling the latest local run, and refreshing the dashboard data.
- In PR review mode, `:agent dispatch` starts the same tmux-backed agent
  dispatch loop for the currently open PR, reusing the CLI ownership checks,
  feedback discovery, CI repair candidates, and notification behavior.
- In PR review mode, `:agent dispatch-thread` starts the same tmux-backed agent
  dispatch loop for only the GitHub review thread under the cursor. The prompt
  includes only matching actionable feedback for that thread and does not pull
  unrelated CI repair candidates into the selected-thread run.
- In PR review mode, `:agent copy-url` copies the selected GitHub review-thread
  comment URL to the clipboard.
- In PR review mode, `:agent status` shows the latest local agent run for the
  currently open PR without leaving the review UI.
- In PR review mode, `:agent resolve` resolves the GitHub review thread under
  the cursor and refreshes local remote-thread annotations.
- In PR review mode, `:agent reply <body>` replies to the GitHub review thread
  under the cursor using the same `🤖` prefix and ownership checks as
  `tuicr prs reply`.
- `tuicr prs feedback`: lists actionable user/robot feedback for one owned PR,
  including outdated review threads that require relevance checks.
- `tuicr prs checks`: lists normalized check/status state for one PR, marks
  failing checks as repair candidates, and enriches failing GitHub CheckRuns
  with REST check-run summaries and annotations when available.
- `tuicr prs dispatch`: writes a scoped prompt/run record and starts a
  tmux-backed agent session, or performs a dry run.
- Dispatch now prepares the local workdir for real agent runs. It reuses an
  existing git worktree for the PR head branch when one is present, otherwise
  creates a named worktree under configured `worktree_root`, an existing
  sibling `.worktrees` directory, or a repo-specific sibling worktree
  directory. Dry runs and no-action runs do not create worktrees.
- `tuicr prs reply`: posts a `🤖`-prefixed reply to either an actionable
  feedback item or a direct review thread, with dry-run support. Passing
  `--expected-head-sha` refuses the reply if the PR head moved since dispatch.
- `tuicr prs resolve`: resolves a GitHub review thread, with dry-run support.
  Passing `--expected-head-sha` refuses the resolve mutation if the PR head
  moved since dispatch.
- `tuicr prs guard-head`: checks the live PR head SHA against the dispatch-time
  expected SHA and exits non-zero when the PR moved. Delegated agents are
  instructed to run it immediately before pushing.
- `tuicr prs watch`: polls authored PRs, checks feedback plus CI state, and
  dispatches agents when actionable feedback or failing checks are present.
  Watch uses the same repository, draft/ready, and review-state filters as
  listing.
- `tuicr prs runs list/show`: inspects local agent run records.
- `tuicr prs runs attach`: attaches to a run's tmux session when it is still
  available.
- `tuicr prs runs status`: records non-terminal lifecycle transitions such as
  `running`, `pushed`, `replied`, and `waiting-for-user`, with notifications
  and dashboard visibility.
- `tuicr prs runs complete`: records agent command completion with succeeded or
  failed status, exit code, and completion timestamp.
- `tuicr prs runs cancel`: marks a run cancelled and kills its tmux session
  when one is still present.
- Dispatch now uses a per-PR local lock plus active-run detection so concurrent
  watch processes do not start duplicate agent sessions for the same PR.
- Run start and completion now record notification delivery attempts. The
  default local sink is macOS `osascript`; `TUICR_NOTIFY_COMMAND` configures a
  command hook and `TUICR_NOTIFY=0` disables notifications.
- Agent runs now expose richer status than process exit alone. The tmux wrapper
  marks a run `running` before launching the agent, and delegated agents are
  instructed to mark `pushed`, `replied`, or `waiting-for-user` as work
  progresses.
- Agent runs now persist the tmux agent command output to `run.log` and write a
  terminal-state `summary.md` with run status, workdir, counts, exit code, and a
  bounded log tail. `prs runs show` prints prompt, log, summary, workdir, and
  worktree metadata paths.
- Local orchestration cache state is persisted under
  `~/.local/state/tuicr/agent-state.json` with a lock file. It records watched
  PRs, pending feedback IDs, handled feedback IDs, check snapshots by head SHA,
  repo/worktree mappings, notification attempts through run records, and agent
  replies posted to GitHub. GitHub replies remain the authoritative dedupe
  signal.
- `tuicr prs watch --max-ci-retries N` records CI repair attempts per
  repository, PR, head SHA, and check name. When all failing checks for a head
  reach the retry limit, watch reports a blocked state and sends one
  notification for that exhausted check/head set.
- CI checks now include adapter log references for common sources such as
  Buildkite, Kochiku, and GitHub Actions. These references are included in JSON,
  readable `prs checks` output, and dispatch prompts so agents know which log
  surface to inspect.
- Failing GitHub Actions CheckRuns with job URLs now fetch a bounded
  `gh run view --job <id> --log-failed` excerpt. The excerpt, run/job IDs,
  truncation flag, and any log-fetch error are included in check JSON and
  readable `prs checks` output.
- Failing Buildkite/Kochiku CheckRuns can fetch bounded raw log excerpts through
  authenticated local command hooks. Set `TUICR_BUILDKITE_LOG_COMMAND` or
  `TUICR_KOCHIKU_LOG_COMMAND`; the command receives `TUICR_CI_ADAPTER`,
  `TUICR_REPOSITORY`, `TUICR_HEAD_SHA`, `TUICR_CHECK_NAME`, `TUICR_CHECK_URL`,
  `TUICR_CHECK_RUN_ID`, `TUICR_CHECK_HTML_URL`, and
  `TUICR_CHECK_DETAILS_URL`, and stdout is captured into the same
  `log_excerpt` field as GitHub Actions.
- The existing `~/.config/tuicr/config.toml` parser now supports an `[agent]`
  section with defaults for workspace/worktree roots, GitHub owners, repo
  include/exclude filters, robot logins, ignored comment patterns, agent
  command, concurrency limits, notification command, CI polling interval,
  max CI retries, and outdated-thread behavior. PR orchestration commands now
  apply those defaults across list, dashboard, feedback, dispatch, reply,
  resolve, watch, and in-review `:agent` actions where applicable.
- Configured `repository_include` and `repository_exclude` filters are enforced
  during PR discovery, using exact `owner/repo` matches or `owner/*` wildcards.
- Configured `ignored_comment_patterns` are applied in actionable feedback
  discovery. Patterns support exact `^...$`, prefix `^...`, suffix `...$`, and
  substring matching.
- Configured `outdated_thread_relevance` is applied in actionable feedback
  discovery: `recheck` includes outdated threads with a relevance-check flag,
  `include` includes them without that flag, and `ignore` suppresses them.
- `prs watch` enforces configured global and per-repository concurrency limits
  before starting new dispatches while still reporting PR feedback and CI
  counts.

Remaining work includes replacing local Buildkite/Kochiku log command hooks
with first-class authenticated API adapters if stable internal APIs are
available.

## State Requirements

Persist local orchestration state separately from review sessions.

State should include:

- watched PRs
- pending feedback IDs
- handled feedback IDs
- run records
- run status transitions
- check run/status snapshots
- CI repair attempts by check name and head SHA
- repo/worktree mapping
- notification delivery attempts
- agent replies posted to GitHub

State must be safe under concurrent local processes. Use file locks or another
coordination mechanism before dispatching or mutating shared state.

Handled-feedback state is a cache. The authoritative skip rule is: if GitHub
already has a `🤖` agent reply newer than the latest actionable user or robot
comment in the relevant thread/comment context, the item does not need another
agent run. If a comment is still unaddressed on the PR, the tool should
re-check whether the feedback remains relevant before skipping it.

## GitHub Integration Requirements

- Use `gh` CLI for initial local implementation.
- Use GraphQL for review-thread discovery, thread replies, and resolve actions
  where REST is insufficient.
- Use REST or GraphQL for top-level PR comments as appropriate.
- Include full GitHub error bodies in user-visible errors.
- Avoid passing large payloads as command-line arguments; use stdin for `gh api`
  payloads.
- Handle private repo 404s as possible auth failures, not only missing PRs.

## Non-Goals For The First Version

- Replace GitHub as the source of truth for PR comments.
- Build a general hosted multi-tenant service.
- Automatically merge PRs.
- Automatically approve PRs as the human.
- Infer broad refactors from vague comments.
- Handle every forge beyond GitHub.
- Solve all Slack/GitHub notification use cases already covered by existing
  internal tools.

## Milestones

### Milestone 1: Feedback Discovery

- Add a command that lists actionable feedback for a PR.
- Include unresolved review threads and top-level PR comments.
- Add local state to avoid duplicate handling.
- Support configured robot logins and ignored comment patterns.

### Milestone 2: Local Dispatch Loop

- Dispatch Codex for one PR's actionable feedback.
- Generate a scoped prompt with feedback JSON.
- Track run status and logs.
- Automatically push fixes and reply for owned PRs when the feedback comes from
  the configured user or a configured robot reviewer.

### Milestone 3: Dashboard

- Add a PR dashboard showing feedback counts and run status.
- Open selected PRs in the existing `tuicr pr` flow.
- Attach to active agent sessions or view run logs.

### Milestone 4: Watch Mode And Notifications

- Add polling watch mode with concurrency limits.
- Poll PR checks and dispatch bounded CI repair attempts for owned PRs.
- Send desktop notifications and update the terminal dashboard.
- Add cancellation and retry behavior.

### Milestone 5: TUI Actions

- Dispatch an agent for the selected PR or selected review thread from inside
  the TUI.
- Reply to or resolve selected GitHub threads.
- Show agent run status inside the review surface.

## Open Questions

- Which repo-specific CI adapters should be implemented first for richer logs
  and annotations beyond generic GitHub check URLs?
