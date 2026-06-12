//! CLI argument parsing, backed by `clap`.
//!
//! The struct [`Cli`] is the clap-derived parser; [`CliArgs`] is the simple
//! POJO the rest of the binary consumes. Conversion lives in `From<Cli>`.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use crate::theme::{AppearanceArg, ThemeArg};

/// CLI arguments consumed by the rest of the binary.
#[derive(Debug, Clone, Default)]
pub struct CliArgs {
    pub theme: Option<String>,
    pub appearance: Option<AppearanceArg>,
    /// Output to stdout instead of clipboard when exporting.
    pub output_to_stdout: bool,
    /// Skip checking for updates on startup.
    pub no_update_check: bool,
    /// Commit/revision range to review.
    pub revisions: Option<String>,
    /// Skip commit selector and review uncommitted changes directly.
    pub working_tree: bool,
    /// Filter diff to a specific file or directory path.
    pub path_filter: Option<String>,
    /// Open a single file or directory for annotation (no VCS required).
    pub file_path: Option<String>,
    /// Whole-repo annotation mode.
    pub all_files: bool,
    /// Direct PR target from `tuicr pr <target>`.
    pub pr_target: Option<String>,
    /// Override the GitHub repo used for PR operations.
    pub repo_url: Option<String>,
    /// Non-interactive review session operation.
    pub review_command: Option<ReviewCommand>,
    /// Non-interactive PR automation operation.
    pub prs_command: Option<PrsCommand>,
}

#[derive(Parser, Debug)]
#[command(
    name = "tuicr",
    version,
    about = "A code review TUI with vim keybindings. Export to GitHub or clipboard.",
    after_help = "Press ? in the application for keybinding help.",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(flatten)]
    tui_options: TuiOptions,

    #[command(subcommand)]
    command: Option<Subcmd>,
}

/// Options that launch or configure the interactive TUI.
#[derive(Args, Debug, Clone, Default)]
struct TuiOptions {
    /// Commit range / revset to review (syntax depends on VCS backend).
    #[arg(
        short = 'r',
        long = "revisions",
        value_name = "REVSET",
        allow_hyphen_values = true
    )]
    revisions: Option<String>,

    /// Color theme to use. Bundled themes resolve first; local themes are
    /// loaded from the config `themes/` directory.
    #[arg(long, value_name = "THEME", value_parser = non_empty_theme_name)]
    theme: Option<String>,

    /// Appearance mode (light/dark/system); used when no explicit theme is set.
    #[arg(long, value_name = "MODE", value_parser = parse_appearance_arg)]
    appearance: Option<AppearanceArg>,

    /// Filter diff to a specific file or directory.
    #[arg(
        short = 'p',
        long = "path",
        value_name = "PATH",
        value_parser = non_empty_path,
        conflicts_with_all = ["file_path", "all_files"],
    )]
    path_filter: Option<String>,

    /// Include uncommitted changes (skip commit selector when used alone;
    /// combine with commits when used with -r).
    #[arg(
        short = 'w',
        long = "working-tree",
        action = ArgAction::SetTrue,
        conflicts_with_all = ["file_path", "all_files"],
    )]
    working_tree: bool,

    /// Open a file or directory for annotation (no VCS required).
    #[arg(
        long = "file",
        value_name = "PATH",
        value_parser = non_empty_path,
        conflicts_with_all = ["path_filter", "revisions", "working_tree", "all_files"],
    )]
    file_path: Option<String>,

    /// Review every tracked file in the cwd's git repo.
    #[arg(
        short = 'A',
        long = "all-files",
        action = ArgAction::SetTrue,
        conflicts_with_all = ["path_filter", "revisions", "working_tree", "file_path"],
    )]
    all_files: bool,

    /// Output to stdout instead of clipboard when exporting.
    #[arg(long = "stdout", action = ArgAction::SetTrue)]
    stdout: bool,

    /// Skip checking for updates on startup.
    #[arg(long = "no-update-check", action = ArgAction::SetTrue)]
    no_update_check: bool,

    /// Override the GitHub repo for PR operations (HTTPS, SCP-style SSH,
    /// or ssh:// URLs accepted).
    #[arg(
        long = "repo-url",
        value_name = "URL",
        value_parser = parse_repo_url
    )]
    repo_url: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Subcmd {
    /// Open the interactive TUI.
    Tui(TuiCommand),
    /// Review a GitHub pull request or GitLab merge request.
    #[command(visible_alias = "mr")]
    Pr(PrCommand),
    /// Inspect or update persisted review sessions.
    Review {
        #[command(subcommand)]
        command: ReviewCommand,
    },
    /// Inspect PR feedback and agent orchestration state.
    Prs {
        #[command(subcommand)]
        command: PrsCommand,
    },
}

/// Explicit `tuicr tui` entrypoint. With no nested command, opens the local
/// target selector / local diff TUI. `tuicr tui pr <target>` opens PR mode.
#[derive(Args, Debug, Clone, Default)]
struct TuiCommand {
    #[command(flatten)]
    options: TuiOptions,

    #[command(subcommand)]
    command: Option<TuiSubcmd>,
}

#[derive(Subcommand, Debug, Clone)]
enum TuiSubcmd {
    /// Review a GitHub pull request or GitLab merge request in the TUI.
    #[command(visible_alias = "mr")]
    Pr(PrCommand),
}

#[derive(Args, Debug, Clone, Default)]
struct PrCommand {
    /// PR target: <number>, <owner/repo#N>, or a PR URL.
    target: String,

    #[command(flatten)]
    options: TuiOptions,
}

/// Non-interactive review session commands.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum ReviewCommand {
    /// List persisted review sessions for a checkout or forge repo.
    List {
        /// Repo selector: a checkout path, or a forge coordinate like
        /// `owner/repo`, `host/owner/repo`, or a repo/PR URL. A path also
        /// surfaces PR sessions for that checkout's origin repo.
        #[arg(long, value_name = "PATH|OWNER/REPO", default_value = ".")]
        repo: PathBuf,

        /// List every persisted session (local and PR), ignoring --repo.
        #[arg(long)]
        all: bool,
    },

    /// Add a local draft comment to a persisted session.
    Add {
        /// Session slug from `tuicr review list` (local or PR), or path to a
        /// session JSON file.
        #[arg(long, value_name = "SESSION")]
        session: String,

        /// JSON payload. Use literal JSON, @path/to/file.json, or - for stdin.
        #[arg(long, value_name = "JSON|@FILE|-")]
        input: Option<String>,

        /// Repo selector used to resolve a local session slug (path or
        /// `owner/repo`). PR slugs and JSON paths resolve without it.
        #[arg(long, value_name = "PATH|OWNER/REPO", default_value = ".")]
        repo: PathBuf,

        /// Comment classification.
        #[arg(long = "type", value_name = "TYPE", default_value = "note", value_parser = non_empty_comment_type)]
        comment_type: String,

        /// File path for a file, line, or range comment. Omit for a review comment.
        #[arg(long = "target-file", value_name = "PATH")]
        file: Option<PathBuf>,

        /// Line number for a line or range comment. Requires --target-file.
        #[arg(long, value_name = "LINE", requires = "file")]
        line: Option<u32>,

        /// End line for a range comment. Requires --line.
        #[arg(long = "end-line", value_name = "LINE", requires = "line")]
        end_line: Option<u32>,

        /// Diff side for line and range comments.
        #[arg(long, value_enum, default_value_t = LineSideArg::New)]
        side: LineSideArg,

        /// Author stamped on the new comment. Pass an explicit value when
        /// invoking from an agent (e.g. `--username "Claude Opus 4.7"`) so
        /// human and agent comments are visually distinguished in the TUI.
        /// Falls back to the config `username` setting, then to `"user"`.
        #[arg(long, value_name = "NAME")]
        username: Option<String>,

        /// Comment text.
        #[arg(
            value_name = "COMMENT",
            required_unless_present = "input",
            value_parser = non_empty_comment_text,
            allow_hyphen_values = true
        )]
        content: Option<String>,
    },

    /// Print comments stored in a persisted session.
    #[command(alias = "get")]
    Comments {
        /// Session slug from `tuicr review list` (local or PR), or path to a
        /// session JSON file.
        #[arg(long, value_name = "SESSION")]
        session: String,

        /// Repo selector used to resolve a local session slug (path or
        /// `owner/repo`). PR slugs and JSON paths resolve without it.
        #[arg(long, value_name = "PATH|OWNER/REPO", default_value = ".")]
        repo: PathBuf,
    },
}

/// Non-interactive PR automation commands.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum PrsCommand {
    /// List open PRs authored by a user.
    List {
        /// GitHub owner/org to search. May be repeated. Defaults to squareup.
        #[arg(long = "owner", value_name = "OWNER")]
        owners: Vec<String>,

        /// Repository to include, as owner/repo. May be repeated.
        #[arg(long = "repository", value_name = "OWNER/REPO")]
        repositories: Vec<String>,

        /// Author login. Defaults to @me.
        #[arg(long, value_name = "LOGIN", default_value = "@me")]
        author: String,

        /// Show only draft PRs.
        #[arg(long, conflicts_with = "ready")]
        draft: bool,

        /// Show only non-draft PRs.
        #[arg(long, conflicts_with = "draft")]
        ready: bool,

        /// Filter by GitHub review state.
        #[arg(long, value_enum)]
        review: Option<ReviewFilterArg>,

        /// Maximum PRs per owner.
        #[arg(long, value_name = "N", default_value_t = 50)]
        limit: usize,

        /// Emit JSON instead of a readable summary.
        #[arg(long)]
        json: bool,
    },

    /// Show a PR dashboard with feedback, CI, and agent run status.
    Dashboard {
        /// GitHub owner/org to search. May be repeated. Defaults to squareup.
        #[arg(long = "owner", value_name = "OWNER")]
        owners: Vec<String>,

        /// Repository to include, as owner/repo. May be repeated.
        #[arg(long = "repository", value_name = "OWNER/REPO")]
        repositories: Vec<String>,

        /// Author login. Defaults to @me.
        #[arg(long, value_name = "LOGIN", default_value = "@me")]
        author: String,

        /// Show only draft PRs.
        #[arg(long, conflicts_with = "ready")]
        draft: bool,

        /// Show only non-draft PRs.
        #[arg(long, conflicts_with = "draft")]
        ready: bool,

        /// Filter by GitHub review state.
        #[arg(long, value_enum)]
        review: Option<ReviewFilterArg>,

        /// Show only PRs with actionable feedback or failing checks.
        #[arg(long)]
        needs_action: bool,

        /// Maximum PRs per owner.
        #[arg(long, value_name = "N", default_value_t = 50)]
        limit: usize,

        /// Emit JSON instead of a readable summary.
        #[arg(long, conflicts_with = "tui")]
        json: bool,

        /// Open the interactive PR dashboard.
        #[arg(long, conflicts_with = "json")]
        tui: bool,

        /// Allow dashboard enrichment for PRs not authored by the configured user.
        #[arg(long)]
        allow_non_owned: bool,
    },

    /// List normalized GitHub check status for one pull request.
    Checks {
        /// Repo selector: a checkout path, owner/repo, host/owner/repo, or GitHub URL.
        #[arg(long, value_name = "PATH|OWNER/REPO", default_value = ".")]
        repo: String,

        /// Pull request number.
        #[arg(long, value_name = "NUMBER", value_parser = parse_positive_pr_number)]
        pr: u64,

        /// Emit JSON instead of a readable summary.
        #[arg(long)]
        json: bool,
    },

    /// Dispatch a tmux-backed agent run for actionable feedback or failing checks.
    Dispatch {
        /// Repo selector: a checkout path, owner/repo, host/owner/repo, or GitHub URL.
        #[arg(long, value_name = "PATH|OWNER/REPO", default_value = ".")]
        repo: String,

        /// Pull request number.
        #[arg(long, value_name = "NUMBER", value_parser = parse_positive_pr_number)]
        pr: u64,

        /// Write the prompt/run record but do not start tmux.
        #[arg(long)]
        dry_run: bool,

        /// Emit JSON instead of a readable summary.
        #[arg(long)]
        json: bool,

        /// Allow dispatch for PRs not authored by the configured user.
        #[arg(long)]
        allow_non_owned: bool,

        /// Agent command to run inside tmux. The prompt is passed on stdin.
        #[arg(long, value_name = "COMMAND")]
        agent_command: Option<String>,
    },

    /// Refuse to continue if a PR head SHA moved since dispatch.
    GuardHead {
        /// Repo selector: a checkout path, owner/repo, host/owner/repo, or GitHub URL.
        #[arg(long, value_name = "PATH|OWNER/REPO", default_value = ".")]
        repo: String,

        /// Pull request number.
        #[arg(long, value_name = "NUMBER", value_parser = parse_positive_pr_number)]
        pr: u64,

        /// Expected PR head SHA.
        #[arg(long, value_name = "SHA")]
        expected_head_sha: String,

        /// Emit JSON instead of a readable summary.
        #[arg(long)]
        json: bool,

        /// Allow checking PRs not authored by the configured user.
        #[arg(long)]
        allow_non_owned: bool,
    },

    /// Reply to actionable PR feedback on GitHub.
    Reply {
        /// Repo selector: a checkout path, owner/repo, host/owner/repo, or GitHub URL.
        #[arg(long, value_name = "PATH|OWNER/REPO", default_value = ".")]
        repo: String,

        /// Pull request number.
        #[arg(long, value_name = "NUMBER", value_parser = parse_positive_pr_number)]
        pr: u64,

        /// Feedback item id from `tuicr prs feedback --json`.
        #[arg(long, value_name = "ID", conflicts_with = "thread_id")]
        feedback_id: Option<String>,

        /// Review thread node id to reply to directly.
        #[arg(long, value_name = "ID", conflicts_with = "feedback_id")]
        thread_id: Option<String>,

        /// Reply body. The required agent prefix is added automatically.
        #[arg(long, value_name = "TEXT", conflicts_with = "input")]
        body: Option<String>,

        /// Reply body as literal text, @path, or - for stdin.
        #[arg(long, value_name = "TEXT|@FILE|-", conflicts_with = "body")]
        input: Option<String>,

        /// Resolve the review thread after posting the reply.
        #[arg(long)]
        resolve: bool,

        /// Refuse to post if the PR head SHA no longer matches this value.
        #[arg(long, value_name = "SHA")]
        expected_head_sha: Option<String>,

        /// Validate target/body but do not post to GitHub.
        #[arg(long)]
        dry_run: bool,

        /// Emit JSON instead of a readable summary.
        #[arg(long)]
        json: bool,

        /// Allow replying on PRs not authored by the configured user.
        #[arg(long)]
        allow_non_owned: bool,
    },

    /// Resolve a GitHub review thread.
    Resolve {
        /// Repo selector: a checkout path, owner/repo, host/owner/repo, or GitHub URL.
        #[arg(long, value_name = "PATH|OWNER/REPO", default_value = ".")]
        repo: String,

        /// Pull request number.
        #[arg(long, value_name = "NUMBER", value_parser = parse_positive_pr_number)]
        pr: u64,

        /// Review thread node id.
        #[arg(long, value_name = "ID")]
        thread_id: String,

        /// Refuse to resolve if the PR head SHA no longer matches this value.
        #[arg(long, value_name = "SHA")]
        expected_head_sha: Option<String>,

        /// Validate ownership/target but do not mutate GitHub.
        #[arg(long)]
        dry_run: bool,

        /// Emit JSON instead of a readable summary.
        #[arg(long)]
        json: bool,

        /// Allow resolving threads on PRs not authored by the configured user.
        #[arg(long)]
        allow_non_owned: bool,
    },

    /// Inspect local agent run records.
    Runs {
        #[command(subcommand)]
        command: PrsRunsCommand,
    },

    /// Poll authored PRs and dispatch agents for feedback or failing CI.
    Watch {
        /// GitHub owner/org to search. May be repeated. Defaults to squareup.
        #[arg(long = "owner", value_name = "OWNER")]
        owners: Vec<String>,

        /// Repository to include, as owner/repo. May be repeated.
        #[arg(long = "repository", value_name = "OWNER/REPO")]
        repositories: Vec<String>,

        /// Author login. Defaults to @me.
        #[arg(long, value_name = "LOGIN", default_value = "@me")]
        author: String,

        /// Show only draft PRs.
        #[arg(long, conflicts_with = "ready")]
        draft: bool,

        /// Show only non-draft PRs.
        #[arg(long, conflicts_with = "draft")]
        ready: bool,

        /// Filter by GitHub review state.
        #[arg(long, value_enum)]
        review: Option<ReviewFilterArg>,

        /// Maximum PRs per owner.
        #[arg(long, value_name = "N", default_value_t = 50)]
        limit: usize,

        /// Write prompts/run records but do not start tmux sessions.
        #[arg(long)]
        dry_run: bool,

        /// Emit JSON instead of a readable summary.
        #[arg(long)]
        json: bool,

        /// Run one polling iteration and exit.
        #[arg(long)]
        once: bool,

        /// Seconds to wait between polling iterations.
        #[arg(long, value_name = "SECONDS", default_value_t = 300)]
        interval_seconds: u64,

        /// Stop after this many polling iterations.
        #[arg(long, value_name = "N")]
        max_iterations: Option<usize>,

        /// Maximum CI repair dispatches per check name and PR head SHA.
        #[arg(long, value_name = "N", default_value_t = 2)]
        max_ci_retries: u32,

        /// Allow dispatch for PRs not authored by the configured user.
        #[arg(long)]
        allow_non_owned: bool,

        /// Agent command to run inside tmux. The prompt is passed on stdin.
        #[arg(long, value_name = "COMMAND")]
        agent_command: Option<String>,
    },

    /// List actionable GitHub PR feedback for one pull request.
    Feedback {
        /// Repo selector: a checkout path, owner/repo, host/owner/repo, or GitHub URL.
        #[arg(long, value_name = "PATH|OWNER/REPO", default_value = ".")]
        repo: String,

        /// Pull request number.
        #[arg(long, value_name = "NUMBER", value_parser = parse_positive_pr_number)]
        pr: u64,

        /// Emit JSON instead of a readable summary.
        #[arg(long)]
        json: bool,

        /// Override the viewer login. Defaults to `gh api user --jq .login`.
        #[arg(long, value_name = "LOGIN")]
        user: Option<String>,

        /// Treat this login as an automated reviewer. May be repeated.
        #[arg(long = "robot-login", value_name = "LOGIN")]
        robot_logins: Vec<String>,

        /// Allow feedback discovery for PRs not authored by the configured user.
        #[arg(long)]
        allow_non_owned: bool,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum PrsRunsCommand {
    /// List local agent run records.
    List {
        /// Emit JSON instead of a readable summary.
        #[arg(long)]
        json: bool,
    },

    /// Show one local agent run record by id or unique prefix.
    Show {
        /// Run id or unique prefix.
        run_id: String,

        /// Emit JSON instead of a readable summary.
        #[arg(long)]
        json: bool,
    },

    /// Mark one local agent run complete.
    Complete {
        /// Run id or unique prefix.
        run_id: String,

        /// Process exit code from the agent command.
        #[arg(long, value_name = "CODE")]
        exit_code: i32,

        /// Emit JSON instead of a readable summary.
        #[arg(long)]
        json: bool,
    },

    /// Update one local agent run's non-terminal lifecycle status.
    Status {
        /// Run id or unique prefix.
        run_id: String,

        /// New non-terminal status.
        #[arg(long, value_enum)]
        status: RunStatusArg,

        /// Human-readable status message.
        #[arg(long, value_name = "TEXT")]
        message: Option<String>,

        /// Emit JSON instead of a readable summary.
        #[arg(long)]
        json: bool,
    },

    /// Cancel one local agent run and kill its tmux session if still present.
    Cancel {
        /// Run id or unique prefix.
        run_id: String,

        /// Emit JSON instead of a readable summary.
        #[arg(long)]
        json: bool,
    },

    /// Attach to one local agent run's tmux session.
    Attach {
        /// Run id or unique prefix.
        run_id: String,
    },
}

/// Diff side accepted by `tuicr review add --side`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineSideArg {
    Old,
    #[default]
    New,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatusArg {
    Started,
    Running,
    Pushed,
    Replied,
    WaitingForUser,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewFilterArg {
    None,
    Required,
    Approved,
    ChangesRequested,
}

impl ReviewFilterArg {
    pub fn as_gh_value(self) -> &'static str {
        match self {
            ReviewFilterArg::None => "none",
            ReviewFilterArg::Required => "required",
            ReviewFilterArg::Approved => "approved",
            ReviewFilterArg::ChangesRequested => "changes_requested",
        }
    }
}

impl From<Cli> for CliArgs {
    fn from(cli: Cli) -> Self {
        let bare_startup = cli.command.is_none() && !cli.tui_options.has_any_explicit_value();
        let (options, pr_target, review_command, prs_command) = match cli.command {
            Some(Subcmd::Tui(command)) => match command.command {
                Some(TuiSubcmd::Pr(pr)) => (
                    cli.tui_options.merge(command.options).merge(pr.options),
                    Some(pr.target),
                    None,
                    None,
                ),
                None => (cli.tui_options.merge(command.options), None, None, None),
            },
            Some(Subcmd::Pr(pr)) => (
                cli.tui_options.merge(pr.options),
                Some(pr.target),
                None,
                None,
            ),
            Some(Subcmd::Review { command }) => (TuiOptions::default(), None, Some(command), None),
            Some(Subcmd::Prs { command }) => (TuiOptions::default(), None, None, Some(command)),
            None if bare_startup => (
                cli.tui_options,
                None,
                None,
                Some(default_startup_pr_dashboard_command()),
            ),
            None => (cli.tui_options, None, None, None),
        };
        Self {
            theme: options.theme,
            appearance: options.appearance,
            output_to_stdout: options.stdout,
            no_update_check: options.no_update_check,
            revisions: options.revisions,
            working_tree: options.working_tree,
            path_filter: options.path_filter,
            file_path: options.file_path,
            all_files: options.all_files,
            pr_target,
            repo_url: options.repo_url,
            review_command,
            prs_command,
        }
    }
}

fn default_startup_pr_dashboard_command() -> PrsCommand {
    PrsCommand::Dashboard {
        owners: vec!["squareup".to_string()],
        repositories: Vec::new(),
        author: "@me".to_string(),
        draft: false,
        ready: false,
        review: None,
        needs_action: false,
        limit: 50,
        json: false,
        tui: true,
        allow_non_owned: false,
    }
}

impl TuiOptions {
    fn has_any_explicit_value(&self) -> bool {
        self.theme.is_some()
            || self.appearance.is_some()
            || self.stdout
            || self.no_update_check
            || self.revisions.is_some()
            || self.working_tree
            || self.path_filter.is_some()
            || self.file_path.is_some()
            || self.all_files
            || self.repo_url.is_some()
    }

    fn merge(self, later: TuiOptions) -> Self {
        Self {
            theme: later.theme.or(self.theme),
            appearance: later.appearance.or(self.appearance),
            stdout: self.stdout || later.stdout,
            no_update_check: self.no_update_check || later.no_update_check,
            revisions: later.revisions.or(self.revisions),
            working_tree: self.working_tree || later.working_tree,
            path_filter: later.path_filter.or(self.path_filter),
            file_path: later.file_path.or(self.file_path),
            all_files: self.all_files || later.all_files,
            repo_url: later.repo_url.or(self.repo_url),
        }
    }
}

impl Cli {
    fn try_into_args(self) -> std::result::Result<CliArgs, clap::Error> {
        if matches!(
            self.command,
            Some(Subcmd::Review { .. }) | Some(Subcmd::Prs { .. })
        ) && self.tui_options.has_any_explicit_value()
        {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "TUI options cannot be used with non-interactive commands; run the subcommand with --help for command-specific options",
            ));
        }
        Ok(self.into())
    }
}

fn parse_appearance_arg(s: &str) -> Result<AppearanceArg, String> {
    AppearanceArg::parse_name(s).ok_or_else(|| {
        let valid = AppearanceArg::valid_values_display();
        format!("Unknown appearance '{s}'. Valid options: {valid}")
    })
}

fn non_empty_theme_name(s: &str) -> Result<String, String> {
    if s.is_empty() {
        let valid = ThemeArg::valid_values_display();
        Err(format!("--theme requires a value ({valid})"))
    } else {
        Ok(s.to_string())
    }
}

/// Reject `--repo-url` values that don't parse as a GitHub remote URL so the
/// failure is surfaced at startup rather than when the PR tab is opened.
fn parse_repo_url(s: &str) -> Result<String, String> {
    if crate::forge::github::gh::parse_github_remote_url(s).is_some() {
        Ok(s.to_string())
    } else {
        Err(format!(
            "--repo-url value '{s}' is not a recognized GitHub URL. \
             Expected forms: https://github.com/owner/repo, git@github.com:owner/repo, \
             or ssh://git@github.com/owner/repo"
        ))
    }
}

fn non_empty_path(s: &str) -> Result<String, String> {
    if s.is_empty() {
        Err("a file or directory path is required".to_string())
    } else {
        Ok(s.to_string())
    }
}

fn non_empty_comment_type(s: &str) -> Result<String, String> {
    if s.is_empty() {
        Err("a comment type is required".to_string())
    } else {
        Ok(s.to_string())
    }
}

fn non_empty_comment_text(s: &str) -> Result<String, String> {
    if s.trim().is_empty() {
        Err("comment text cannot be empty".to_string())
    } else {
        Ok(s.to_string())
    }
}

fn parse_positive_pr_number(s: &str) -> Result<u64, String> {
    let number = s
        .parse::<u64>()
        .map_err(|_| format!("invalid PR number '{s}'"))?;
    if number == 0 {
        Err("PR number must be greater than zero".to_string())
    } else {
        Ok(number)
    }
}

/// Parse CLI arguments from `std::env::args`. On `--help`/`--version`/parse
/// errors, clap prints to stdout/stderr and exits the process.
pub fn parse_cli_args() -> CliArgs {
    match Cli::parse().try_into_args() {
        Ok(args) => args,
        Err(err) => err.exit(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    fn parse_for_test(args: &[&str]) -> Result<CliArgs, clap::Error> {
        Cli::try_parse_from(args).and_then(Cli::try_into_args)
    }

    #[test]
    fn should_parse_theme_when_provided() {
        let parsed = parse_for_test(&["tuicr", "--theme", "light"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("light".to_string()));
    }

    #[test]
    fn should_parse_catppuccin_themes() {
        let parsed = parse_for_test(&["tuicr", "--theme", "catppuccin-mocha"])
            .expect("parse should succeed");
        assert_eq!(parsed.theme, Some("catppuccin-mocha".to_string()));

        let parsed =
            parse_for_test(&["tuicr", "--theme=catppuccin-latte"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("catppuccin-latte".to_string()));
    }

    #[test]
    fn should_parse_ayu_light_theme() {
        let parsed =
            parse_for_test(&["tuicr", "--theme", "ayu-light"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("ayu-light".to_string()));
    }

    #[test]
    fn should_parse_onedark_theme() {
        let parsed =
            parse_for_test(&["tuicr", "--theme", "onedark"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("onedark".to_string()));
    }

    #[test]
    fn should_parse_gruvbox_themes() {
        let parsed =
            parse_for_test(&["tuicr", "--theme", "gruvbox-dark"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("gruvbox-dark".to_string()));

        let parsed =
            parse_for_test(&["tuicr", "--theme=gruvbox-light"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("gruvbox-light".to_string()));
    }

    #[test]
    fn should_parse_everforest_themes() {
        let parsed =
            parse_for_test(&["tuicr", "--theme", "everforest-dark"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("everforest-dark".to_string()));

        let parsed =
            parse_for_test(&["tuicr", "--theme=everforest-light"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("everforest-light".to_string()));
    }

    #[test]
    fn should_leave_theme_none_when_not_provided() {
        let parsed = parse_for_test(&["tuicr"]).expect("parse should succeed");
        assert_eq!(parsed.theme, None);
    }

    #[test]
    fn should_default_bare_startup_to_squareup_pr_dashboard_tui() {
        let parsed = parse_for_test(&["tuicr"]).expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Dashboard {
                owners: vec!["squareup".to_string()],
                repositories: Vec::new(),
                author: "@me".to_string(),
                draft: false,
                ready: false,
                review: None,
                needs_action: false,
                limit: 50,
                json: false,
                tui: true,
                allow_non_owned: false,
            })
        );
    }

    #[test]
    fn should_keep_explicit_tui_command_as_local_tui() {
        let parsed = parse_for_test(&["tuicr", "tui"]).expect("parse should succeed");
        assert_eq!(parsed.prs_command, None);
        assert_eq!(parsed.pr_target, None);
    }

    #[test]
    fn should_keep_explicit_root_tui_options_as_local_tui() {
        let parsed = parse_for_test(&["tuicr", "-w"]).expect("parse should succeed");
        assert_eq!(parsed.prs_command, None);
        assert!(parsed.working_tree);
    }

    #[test]
    fn should_parse_working_tree_short_flag() {
        let parsed = parse_for_test(&["tuicr", "-w"]).expect("parse should succeed");
        assert!(parsed.working_tree);
    }

    #[test]
    fn should_parse_working_tree_long_flag() {
        let parsed = parse_for_test(&["tuicr", "--working-tree"]).expect("parse should succeed");
        assert!(parsed.working_tree);
    }

    #[test]
    fn should_default_working_tree_to_false() {
        let parsed = parse_for_test(&["tuicr"]).expect("parse should succeed");
        assert!(!parsed.working_tree);
    }

    #[test]
    fn should_parse_working_tree_with_revisions() {
        let parsed =
            parse_for_test(&["tuicr", "-w", "-r", "HEAD~3..HEAD"]).expect("parse should succeed");
        assert!(parsed.working_tree);
        assert_eq!(parsed.revisions, Some("HEAD~3..HEAD".to_string()));
    }

    #[test]
    fn should_allow_custom_theme_name_in_separate_arg() {
        let parsed = parse_for_test(&["tuicr", "--theme", "tuicr-teal"])
            .expect("custom theme parse should succeed");
        assert_eq!(parsed.theme, Some("tuicr-teal".to_string()));
    }

    #[test]
    fn should_allow_custom_theme_name_in_equals_arg() {
        let parsed = parse_for_test(&["tuicr", "--theme=tuicr-teal"])
            .expect("custom theme parse should succeed");
        assert_eq!(parsed.theme, Some("tuicr-teal".to_string()));
    }

    #[test]
    fn should_error_when_theme_value_missing() {
        let err = parse_for_test(&["tuicr", "--theme"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn should_parse_appearance_when_provided() {
        let parsed =
            parse_for_test(&["tuicr", "--appearance", "system"]).expect("parse should succeed");
        assert_eq!(parsed.appearance, Some(AppearanceArg::System));
    }

    #[test]
    fn should_error_for_invalid_appearance() {
        let err =
            parse_for_test(&["tuicr", "--appearance", "nope"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(err.to_string().contains("Unknown appearance 'nope'"));
    }

    #[test]
    fn should_parse_path_short_flag() {
        let parsed = parse_for_test(&["tuicr", "-p", "src/main.rs"]).expect("parse should succeed");
        assert_eq!(parsed.path_filter, Some("src/main.rs".to_string()));
    }

    #[test]
    fn should_parse_path_long_flag() {
        let parsed = parse_for_test(&["tuicr", "--path", "src/"]).expect("parse should succeed");
        assert_eq!(parsed.path_filter, Some("src/".to_string()));
    }

    #[test]
    fn should_parse_path_equals_syntax() {
        let parsed = parse_for_test(&["tuicr", "--path=plans/current-plan.md"])
            .expect("parse should succeed");
        assert_eq!(
            parsed.path_filter,
            Some("plans/current-plan.md".to_string())
        );
    }

    #[test]
    fn should_error_when_path_value_missing() {
        let err = parse_for_test(&["tuicr", "--path"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn should_error_when_path_equals_empty() {
        let err = parse_for_test(&["tuicr", "--path="]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
    }

    #[test]
    fn should_default_path_filter_to_none() {
        let parsed = parse_for_test(&["tuicr"]).expect("parse should succeed");
        assert_eq!(parsed.path_filter, None);
    }

    #[test]
    fn should_parse_path_with_working_tree() {
        let parsed =
            parse_for_test(&["tuicr", "-p", "file.md", "-w"]).expect("parse should succeed");
        assert_eq!(parsed.path_filter, Some("file.md".to_string()));
        assert!(parsed.working_tree);
    }

    #[test]
    fn should_parse_path_with_revisions() {
        let parsed = parse_for_test(&["tuicr", "--path", "src/", "-r", "HEAD~3.."])
            .expect("parse should succeed");
        assert_eq!(parsed.path_filter, Some("src/".to_string()));
        assert_eq!(parsed.revisions, Some("HEAD~3..".to_string()));
    }

    #[test]
    fn should_reject_file_combined_with_path() {
        let err = parse_for_test(&["tuicr", "--file", "f.md", "--path", "src/"])
            .expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn should_reject_file_combined_with_revisions() {
        let err = parse_for_test(&["tuicr", "--file", "f.md", "-r", "HEAD~1.."])
            .expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn should_reject_file_combined_with_working_tree() {
        let err =
            parse_for_test(&["tuicr", "--file", "f.md", "-w"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn should_reject_all_files_combined_with_path() {
        let err =
            parse_for_test(&["tuicr", "-A", "--path", "src/"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn should_reject_all_files_combined_with_file() {
        let err =
            parse_for_test(&["tuicr", "-A", "--file", "f.md"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn should_parse_all_files_short_flag() {
        let parsed = parse_for_test(&["tuicr", "-A"]).expect("parse should succeed");
        assert!(parsed.all_files);
    }

    #[test]
    fn should_parse_all_files_long_flag() {
        let parsed = parse_for_test(&["tuicr", "--all-files"]).expect("parse should succeed");
        assert!(parsed.all_files);
    }

    #[test]
    fn should_parse_stdout_flag() {
        let parsed = parse_for_test(&["tuicr", "--stdout"]).expect("parse should succeed");
        assert!(parsed.output_to_stdout);
    }

    #[test]
    fn should_parse_no_update_check_flag() {
        let parsed = parse_for_test(&["tuicr", "--no-update-check"]).expect("parse should succeed");
        assert!(parsed.no_update_check);
    }

    #[test]
    fn should_parse_pr_target_as_bare_number() {
        let parsed = parse_for_test(&["tuicr", "pr", "125"]).expect("parse should succeed");
        assert_eq!(parsed.pr_target, Some("125".to_string()));
    }

    #[test]
    fn should_parse_mr_alias_like_pr() {
        let parsed = parse_for_test(&["tuicr", "mr", "125"]).expect("parse should succeed");
        assert_eq!(parsed.pr_target, Some("125".to_string()));
    }

    #[test]
    fn should_parse_tui_mr_alias_like_pr() {
        let parsed = parse_for_test(&["tuicr", "tui", "mr", "125"]).expect("parse should succeed");
        assert_eq!(parsed.pr_target, Some("125".to_string()));
    }

    #[test]
    fn should_parse_pr_target_as_owner_repo_hash() {
        let parsed =
            parse_for_test(&["tuicr", "pr", "agavra/tuicr#125"]).expect("parse should succeed");
        assert_eq!(parsed.pr_target, Some("agavra/tuicr#125".to_string()));
    }

    #[test]
    fn should_parse_pr_target_as_full_url() {
        let parsed = parse_for_test(&["tuicr", "pr", "https://github.com/agavra/tuicr/pull/125"])
            .expect("parse should succeed");
        assert_eq!(
            parsed.pr_target,
            Some("https://github.com/agavra/tuicr/pull/125".to_string()),
        );
    }

    #[test]
    fn should_error_when_pr_target_is_missing() {
        let err = parse_for_test(&["tuicr", "pr"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn should_combine_pr_target_with_theme_flag() {
        // Legacy `tuicr pr` still accepts TUI flags on the subcommand.
        let parsed = parse_for_test(&["tuicr", "pr", "125", "--theme", "dark"])
            .expect("parse should succeed");
        assert_eq!(parsed.pr_target, Some("125".to_string()));
        assert_eq!(parsed.theme, Some("dark".to_string()));
    }

    #[test]
    fn should_allow_root_tui_options_before_legacy_pr_subcommand() {
        let parsed = parse_for_test(&["tuicr", "--theme", "dark", "pr", "125"])
            .expect("parse should succeed");
        assert_eq!(parsed.pr_target, Some("125".to_string()));
        assert_eq!(parsed.theme, Some("dark".to_string()));
    }

    #[test]
    fn should_parse_explicit_tui_command() {
        let parsed = parse_for_test(&["tuicr", "tui", "-w", "--theme", "dark"])
            .expect("parse should succeed");
        assert!(parsed.working_tree);
        assert_eq!(parsed.theme, Some("dark".to_string()));
        assert_eq!(parsed.pr_target, None);
        assert_eq!(parsed.review_command, None);
    }

    #[test]
    fn should_parse_explicit_tui_pr_command() {
        let parsed = parse_for_test(&["tuicr", "tui", "pr", "125", "--theme", "dark"])
            .expect("parse should succeed");
        assert_eq!(parsed.pr_target, Some("125".to_string()));
        assert_eq!(parsed.theme, Some("dark".to_string()));
    }

    #[test]
    fn should_reject_root_tui_options_before_subcommands() {
        let err = parse_for_test(&["tuicr", "--theme", "dark", "review", "list"])
            .expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);

        let err = parse_for_test(&["tuicr", "--theme", "dark", "prs", "feedback", "--pr", "125"])
            .expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn should_parse_prs_feedback_command() {
        let parsed = parse_for_test(&[
            "tuicr",
            "prs",
            "feedback",
            "--repo",
            "squareup/java",
            "--pr",
            "480718",
            "--json",
            "--user",
            "tbedor-square2",
            "--robot-login",
            "chatgpt-codex-connector[bot]",
            "--allow-non-owned",
        ])
        .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Feedback {
                repo: "squareup/java".to_string(),
                pr: 480718,
                json: true,
                user: Some("tbedor-square2".to_string()),
                robot_logins: vec!["chatgpt-codex-connector[bot]".to_string()],
                allow_non_owned: true,
            })
        );
    }

    #[test]
    fn should_parse_prs_checks_command() {
        let parsed = parse_for_test(&[
            "tuicr",
            "prs",
            "checks",
            "--repo",
            "squareup/java",
            "--pr",
            "480718",
            "--json",
        ])
        .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Checks {
                repo: "squareup/java".to_string(),
                pr: 480718,
                json: true,
            })
        );
    }

    #[test]
    fn should_parse_prs_list_command() {
        let parsed = parse_for_test(&[
            "tuicr",
            "prs",
            "list",
            "--owner",
            "squareup",
            "--author",
            "@me",
            "--limit",
            "25",
            "--repository",
            "squareup/java",
            "--draft",
            "--review",
            "changes-requested",
            "--json",
        ])
        .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::List {
                owners: vec!["squareup".to_string()],
                repositories: vec!["squareup/java".to_string()],
                author: "@me".to_string(),
                draft: true,
                ready: false,
                review: Some(ReviewFilterArg::ChangesRequested),
                limit: 25,
                json: true,
            })
        );
    }

    #[test]
    fn should_parse_prs_dashboard_command() {
        let parsed = parse_for_test(&[
            "tuicr",
            "prs",
            "dashboard",
            "--owner",
            "squareup",
            "--repository",
            "squareup/java",
            "--author",
            "@me",
            "--ready",
            "--review",
            "approved",
            "--needs-action",
            "--limit",
            "25",
            "--json",
            "--allow-non-owned",
        ])
        .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Dashboard {
                owners: vec!["squareup".to_string()],
                repositories: vec!["squareup/java".to_string()],
                author: "@me".to_string(),
                draft: false,
                ready: true,
                review: Some(ReviewFilterArg::Approved),
                needs_action: true,
                limit: 25,
                json: true,
                tui: false,
                allow_non_owned: true,
            })
        );
    }

    #[test]
    fn should_parse_prs_dispatch_command() {
        let parsed = parse_for_test(&[
            "tuicr",
            "prs",
            "dispatch",
            "--repo",
            "squareup/java",
            "--pr",
            "480718",
            "--dry-run",
            "--json",
            "--agent-command",
            "codex exec -",
        ])
        .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Dispatch {
                repo: "squareup/java".to_string(),
                pr: 480718,
                dry_run: true,
                json: true,
                allow_non_owned: false,
                agent_command: Some("codex exec -".to_string()),
            })
        );
    }

    #[test]
    fn should_parse_prs_reply_command() {
        let parsed = parse_for_test(&[
            "tuicr",
            "prs",
            "reply",
            "--repo",
            "squareup/java",
            "--pr",
            "480718",
            "--feedback-id",
            "PRRC_1",
            "--body",
            "Fixed.",
            "--resolve",
            "--expected-head-sha",
            "abc123",
            "--dry-run",
            "--json",
            "--allow-non-owned",
        ])
        .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Reply {
                repo: "squareup/java".to_string(),
                pr: 480718,
                feedback_id: Some("PRRC_1".to_string()),
                thread_id: None,
                body: Some("Fixed.".to_string()),
                input: None,
                resolve: true,
                expected_head_sha: Some("abc123".to_string()),
                dry_run: true,
                json: true,
                allow_non_owned: true,
            })
        );
    }

    #[test]
    fn should_parse_prs_guard_head_command() {
        let parsed = parse_for_test(&[
            "tuicr",
            "prs",
            "guard-head",
            "--repo",
            "squareup/java",
            "--pr",
            "480718",
            "--expected-head-sha",
            "abc123",
            "--json",
            "--allow-non-owned",
        ])
        .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::GuardHead {
                repo: "squareup/java".to_string(),
                pr: 480718,
                expected_head_sha: "abc123".to_string(),
                json: true,
                allow_non_owned: true,
            })
        );
    }

    #[test]
    fn should_parse_prs_resolve_command() {
        let parsed = parse_for_test(&[
            "tuicr",
            "prs",
            "resolve",
            "--repo",
            "squareup/java",
            "--pr",
            "480718",
            "--thread-id",
            "PRRT_1",
            "--expected-head-sha",
            "abc123",
            "--dry-run",
            "--json",
        ])
        .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Resolve {
                repo: "squareup/java".to_string(),
                pr: 480718,
                thread_id: "PRRT_1".to_string(),
                expected_head_sha: Some("abc123".to_string()),
                dry_run: true,
                json: true,
                allow_non_owned: false,
            })
        );
    }

    #[test]
    fn should_parse_prs_watch_command() {
        let parsed = parse_for_test(&[
            "tuicr",
            "prs",
            "watch",
            "--owner",
            "squareup",
            "--repository",
            "squareup/java",
            "--author",
            "@me",
            "--ready",
            "--review",
            "required",
            "--limit",
            "25",
            "--dry-run",
            "--json",
            "--once",
            "--interval-seconds",
            "60",
            "--max-iterations",
            "3",
            "--max-ci-retries",
            "4",
            "--allow-non-owned",
            "--agent-command",
            "codex exec -",
        ])
        .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Watch {
                owners: vec!["squareup".to_string()],
                repositories: vec!["squareup/java".to_string()],
                author: "@me".to_string(),
                draft: false,
                ready: true,
                review: Some(ReviewFilterArg::Required),
                limit: 25,
                dry_run: true,
                json: true,
                once: true,
                interval_seconds: 60,
                max_iterations: Some(3),
                max_ci_retries: 4,
                allow_non_owned: true,
                agent_command: Some("codex exec -".to_string()),
            })
        );
    }

    #[test]
    fn should_parse_prs_runs_commands() {
        let parsed = parse_for_test(&["tuicr", "prs", "runs", "list", "--json"])
            .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Runs {
                command: PrsRunsCommand::List { json: true },
            })
        );

        let parsed = parse_for_test(&["tuicr", "prs", "runs", "show", "abc123", "--json"])
            .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Runs {
                command: PrsRunsCommand::Show {
                    run_id: "abc123".to_string(),
                    json: true,
                },
            })
        );

        let parsed = parse_for_test(&[
            "tuicr",
            "prs",
            "runs",
            "complete",
            "abc123",
            "--exit-code",
            "1",
            "--json",
        ])
        .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Runs {
                command: PrsRunsCommand::Complete {
                    run_id: "abc123".to_string(),
                    exit_code: 1,
                    json: true,
                },
            })
        );

        let parsed = parse_for_test(&[
            "tuicr",
            "prs",
            "runs",
            "status",
            "abc123",
            "--status",
            "waiting-for-user",
            "--message",
            "Needs a human decision",
            "--json",
        ])
        .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Runs {
                command: PrsRunsCommand::Status {
                    run_id: "abc123".to_string(),
                    status: RunStatusArg::WaitingForUser,
                    message: Some("Needs a human decision".to_string()),
                    json: true,
                },
            })
        );

        let parsed = parse_for_test(&["tuicr", "prs", "runs", "cancel", "abc123", "--json"])
            .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Runs {
                command: PrsRunsCommand::Cancel {
                    run_id: "abc123".to_string(),
                    json: true,
                },
            })
        );

        let parsed = parse_for_test(&["tuicr", "prs", "runs", "attach", "abc123"])
            .expect("parse should succeed");
        assert_eq!(
            parsed.prs_command,
            Some(PrsCommand::Runs {
                command: PrsRunsCommand::Attach {
                    run_id: "abc123".to_string(),
                },
            })
        );
    }

    #[test]
    fn should_reject_zero_pr_number_for_prs_feedback() {
        let err = parse_for_test(&["tuicr", "prs", "feedback", "--pr", "0"])
            .expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
    }

    #[test]
    fn should_leave_pr_target_none_when_no_pr_subcommand() {
        let parsed = parse_for_test(&["tuicr"]).expect("parse should succeed");
        assert_eq!(parsed.pr_target, None);
    }

    #[test]
    fn should_parse_repo_url_https() {
        let parsed = parse_for_test(&["tuicr", "--repo-url", "https://github.com/slatedb/slatedb"])
            .expect("parse should succeed");
        assert_eq!(
            parsed.repo_url,
            Some("https://github.com/slatedb/slatedb".to_string())
        );
    }

    #[test]
    fn should_parse_repo_url_equals_form() {
        let parsed = parse_for_test(&["tuicr", "--repo-url=git@github.com:slatedb/slatedb.git"])
            .expect("parse should succeed");
        assert_eq!(
            parsed.repo_url,
            Some("git@github.com:slatedb/slatedb.git".to_string())
        );
    }

    #[test]
    fn should_parse_repo_url_ssh_scheme() {
        let parsed = parse_for_test(&[
            "tuicr",
            "--repo-url",
            "ssh://git@github.com/slatedb/slatedb.git",
        ])
        .expect("parse should succeed");
        assert_eq!(
            parsed.repo_url,
            Some("ssh://git@github.com/slatedb/slatedb.git".to_string())
        );
    }

    #[test]
    fn should_error_when_repo_url_value_missing() {
        let err = parse_for_test(&["tuicr", "--repo-url"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn should_error_when_repo_url_unparseable() {
        let err =
            parse_for_test(&["tuicr", "--repo-url", "not-a-url"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(err.to_string().contains("not a recognized GitHub URL"));
    }

    #[test]
    fn should_error_when_repo_url_equals_empty() {
        let err = parse_for_test(&["tuicr", "--repo-url="]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
    }

    #[test]
    fn should_leave_repo_url_none_when_not_provided() {
        let parsed = parse_for_test(&["tuicr"]).expect("parse should succeed");
        assert_eq!(parsed.repo_url, None);
    }

    #[test]
    fn should_parse_review_list_command() {
        let parsed = parse_for_test(&["tuicr", "review", "list", "--repo", "/tmp/repo"])
            .expect("parse should succeed");
        assert_eq!(
            parsed.review_command,
            Some(ReviewCommand::List {
                repo: PathBuf::from("/tmp/repo"),
                all: false,
            })
        );
    }

    #[test]
    fn should_parse_review_list_all_flag() {
        let parsed =
            parse_for_test(&["tuicr", "review", "list", "--all"]).expect("parse should succeed");
        assert_eq!(
            parsed.review_command,
            Some(ReviewCommand::List {
                repo: PathBuf::from("."),
                all: true,
            })
        );
    }

    #[test]
    fn should_parse_review_list_by_coordinate() {
        let parsed = parse_for_test(&["tuicr", "review", "list", "--repo", "slatedb/slatedb"])
            .expect("parse should succeed");
        assert_eq!(
            parsed.review_command,
            Some(ReviewCommand::List {
                repo: PathBuf::from("slatedb/slatedb"),
                all: false,
            })
        );
    }

    #[test]
    fn should_reject_review_json_flag_because_output_is_always_json() {
        let err =
            parse_for_test(&["tuicr", "review", "list", "--json"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn should_parse_review_add_line_comment() {
        let parsed = parse_for_test(&[
            "tuicr",
            "review",
            "add",
            "--session",
            "agavra/tuicr@main/worktree",
            "--target-file",
            "src/main.rs",
            "--line",
            "42",
            "--type",
            "issue",
            "--side",
            "old",
            "Handle the empty case",
        ])
        .expect("parse should succeed");

        assert_eq!(
            parsed.review_command,
            Some(ReviewCommand::Add {
                session: "agavra/tuicr@main/worktree".to_string(),
                input: None,
                repo: PathBuf::from("."),
                comment_type: "issue".to_string(),
                file: Some(PathBuf::from("src/main.rs")),
                line: Some(42),
                end_line: None,
                side: LineSideArg::Old,
                username: None,
                content: Some("Handle the empty case".to_string()),
            })
        );
    }

    #[test]
    fn should_parse_review_add_json_input() {
        let parsed = parse_for_test(&[
            "tuicr",
            "review",
            "add",
            "--session",
            "agavra/tuicr@main/worktree",
            "--input",
            r#"{"file":"src/main.rs","line":42,"side":"old","content":"note"}"#,
        ])
        .expect("parse should succeed");

        assert_eq!(
            parsed.review_command,
            Some(ReviewCommand::Add {
                session: "agavra/tuicr@main/worktree".to_string(),
                input: Some(
                    r#"{"file":"src/main.rs","line":42,"side":"old","content":"note"}"#.to_string()
                ),
                repo: PathBuf::from("."),
                comment_type: "note".to_string(),
                file: None,
                line: None,
                end_line: None,
                side: LineSideArg::New,
                username: None,
                content: None,
            })
        );
    }

    #[test]
    fn should_parse_review_comments_command() {
        let parsed = parse_for_test(&["tuicr", "review", "comments", "--session", "session.json"])
            .expect("parse should succeed");
        assert_eq!(
            parsed.review_command,
            Some(ReviewCommand::Comments {
                session: "session.json".to_string(),
                repo: PathBuf::from("."),
            })
        );
    }

    #[test]
    fn should_parse_review_comments_get_alias() {
        let parsed = parse_for_test(&["tuicr", "review", "get", "--session", "session.json"])
            .expect("parse should succeed");
        assert_eq!(
            parsed.review_command,
            Some(ReviewCommand::Comments {
                session: "session.json".to_string(),
                repo: PathBuf::from("."),
            })
        );
    }

    #[test]
    fn should_require_file_for_review_add_line() {
        let err = parse_for_test(&[
            "tuicr",
            "review",
            "add",
            "--session",
            "session",
            "--line",
            "42",
            "note",
        ])
        .expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }
}
