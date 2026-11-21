use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use web_vault::config::VaultConfig;

/// Local knowledge vault & MCP server for documentation sites and Markdown folders.
#[derive(Debug, Parser)]
#[command(name = "web-vault", version, about)]
struct Cli {
    /// Override the vault data directory (also respects WEB_VAULT_HOME).
    #[arg(long, global = true, value_name = "DIR", env = "WEB_VAULT_HOME")]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Register a documentation URL or local Markdown directory.
    Add {
        /// http(s):// root URL or local directory path.
        #[arg(value_name = "URL_OR_PATH")]
        origin: String,

        /// Source slug name (auto-derived if omitted).
        #[arg(long)]
        name: Option<String>,

        /// Comma-separated tags for filtering.
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,

        /// Refresh interval duration (e.g. 24h, 30m, 0).
        #[arg(long)]
        interval: Option<String>,

        /// Maximum number of pages to ingest.
        #[arg(long)]
        max_pages: Option<u32>,

        /// Maximum crawl depth.
        #[arg(long)]
        max_depth: Option<u32>,

        /// Glob or prefix patterns to include.
        #[arg(long = "include", value_name = "PAT")]
        include: Vec<String>,

        /// Glob or prefix patterns to exclude.
        #[arg(long = "exclude", value_name = "PAT")]
        exclude: Vec<String>,

        /// Render JavaScript pages using headless Chrome (requires `chrome` feature).
        #[arg(long)]
        render_js: bool,

        /// Register the source without running an immediate initial sync.
        #[arg(long)]
        no_sync: bool,
    },

    /// List configured sources and health metrics.
    List {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Show source details, sync history, and stale/error pages.
    Show {
        /// Source slug name.
        name: String,
    },

    /// Sync one or all sources manually.
    Sync {
        /// Source slug name (optional when --all is used).
        name: Option<String>,

        /// Sync all registered sources.
        #[arg(long)]
        all: bool,

        /// Remove stale pages instead of retaining them with status='stale'.
        #[arg(long)]
        prune: bool,
    },

    /// Remove a source and cascade delete its stored pages.
    Rm {
        /// Source slug name.
        name: String,

        /// Skip confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Run a read-only SQLite/FTS5 query against the vault.
    Query {
        /// Single read-only SQL SELECT statement.
        sql: String,

        /// Maximum rows to return (default 50, max 200).
        #[arg(long, default_value_t = 50)]
        limit: usize,

        /// Output format.
        #[arg(long, value_enum, default_value_t = QueryFormat::Table)]
        format: QueryFormat,
    },

    /// Run the all-in-one server (daemon + Web UI + MCP Streamable HTTP on /mcp).
    Serve {
        /// Socket address to bind.
        #[arg(long, default_value = "0.0.0.0:4317")]
        bind: String,
    },

    /// Start the stdio MCP server (newline-delimited JSON-RPC 2.0).
    Mcp,

    /// Launch the embedded local Web UI server.
    Ui {
        /// Port to bind on 127.0.0.1.
        #[arg(long)]
        port: Option<u16>,

        /// Do not automatically open the browser.
        #[arg(long)]
        no_open: bool,
    },

    /// Manage the background sync daemon and OS service registration.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },

    /// Install the embedded agent router skill.
    Skill {
        #[command(subcommand)]
        command: SkillCommands,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum QueryFormat {
    Table,
    Json,
    Csv,
}

#[derive(Debug, Subcommand)]
enum DaemonCommands {
    /// Start the background daemon.
    Start {
        /// Run in the foreground rather than detaching.
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the running daemon.
    Stop,
    /// Check daemon status.
    Status,
    /// Install launchd (macOS) or systemd --user (Linux) service.
    Install,
    /// Uninstall the OS background service.
    Uninstall,
}

#[derive(Debug, Subcommand)]
enum SkillCommands {
    /// Install the embedded SKILL.md router file.
    Install {
        /// Target parent directory (installs into <dir>/web-vault/SKILL.md).
        #[arg(long)]
        path: Option<PathBuf>,

        /// Install globally to user agent skill directories.
        #[arg(long)]
        global: bool,

        /// Target agent skill directory when installing globally.
        #[arg(long, value_enum, default_value_t = SkillTarget::All)]
        target: SkillTarget,

        /// Overwrite existing SKILL.md or symlinks.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SkillTarget {
    Agents,
    Claude,
    Copilot,
    Opencode,
    All,
}

fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Add { .. } => "add",
        Commands::List { .. } => "list",
        Commands::Show { .. } => "show",
        Commands::Sync { .. } => "sync",
        Commands::Rm { .. } => "rm",
        Commands::Query { .. } => "query",
        Commands::Serve { .. } => "serve",
        Commands::Mcp => "mcp",
        Commands::Ui { .. } => "ui",
        Commands::Daemon { command } => match command {
            DaemonCommands::Start { .. } => "daemon start",
            DaemonCommands::Stop => "daemon stop",
            DaemonCommands::Status => "daemon status",
            DaemonCommands::Install => "daemon install",
            DaemonCommands::Uninstall => "daemon uninstall",
        },
        Commands::Skill { command } => match command {
            SkillCommands::Install { .. } => "skill install",
        },
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let config = VaultConfig::resolve(cli.data_dir.clone())?;
    tracing::debug!(
        data_dir = %config.data_dir.display(),
        ?cli,
        "parsed CLI invocation"
    );
    let name = command_name(&cli.command);
    anyhow::bail!("subcommand `{name}` is not implemented yet");
}

fn main() -> anyhow::Result<()> {
    web_vault::init_tracing();
    let cli = Cli::parse();
    run(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parse_smoke() {
        let cases: &[&[&str]] = &[
            &[
                "web-vault",
                "add",
                "https://example.com/docs",
                "--name",
                "example-docs",
                "--tags",
                "rust,sqlite",
                "--interval",
                "24h",
                "--max-pages",
                "200",
                "--max-depth",
                "4",
                "--include",
                "docs/*",
                "--exclude",
                "blog/*",
                "--render-js",
                "--no-sync",
            ],
            &["web-vault", "list", "--json"],
            &["web-vault", "show", "example-docs"],
            &["web-vault", "sync", "example-docs", "--prune"],
            &["web-vault", "sync", "--all"],
            &["web-vault", "rm", "example-docs", "--yes"],
            &[
                "web-vault",
                "query",
                "SELECT name FROM v_sources",
                "--limit",
                "25",
                "--format",
                "json",
            ],
            &["web-vault", "serve", "--bind", "0.0.0.0:4317"],
            &["web-vault", "mcp"],
            &["web-vault", "ui", "--port", "4317", "--no-open"],
            &["web-vault", "daemon", "start", "--foreground"],
            &["web-vault", "daemon", "stop"],
            &["web-vault", "daemon", "status"],
            &["web-vault", "daemon", "install"],
            &["web-vault", "daemon", "uninstall"],
            &[
                "web-vault",
                "skill",
                "install",
                "--global",
                "--target",
                "claude",
                "--force",
            ],
            &["web-vault", "skill", "install", "--path", "/tmp/skills"],
        ];

        for args in cases {
            Cli::try_parse_from(*args)
                .unwrap_or_else(|e| panic!("failed to parse {:?}: {e}", args));
        }
    }
}
