//! `ujira` — a small MCP server for JIRA Cloud, and the same sixteen tools as a CLI.
//!
//! Why it exists: the official Atlassian MCP announces ~40 tools and answers in raw JIRA JSON, which
//! together cost tens of thousands of context tokens before you've read a single ticket. This one
//! carries sixteen tools and renders compact text (see [`ujira::render`]).
//!
//! `ujira mcp serve` serves MCP over stdio, which is how a client launches it. Every other subcommand
//! is the same operation an MCP tool exposes, over the same [`ujira::ops`] code, printing the same
//! bytes — so an agent can pipe, grep, and loop over `ujira` in a shell script instead of making
//! one tool call per issue, and get exactly what the tool would have returned.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use std::sync::Arc;
use ujira::{Config, JiraClient, JiraMcp, ops, ops::IssueFields};

/// A token-frugal JIRA Cloud client: an MCP server, and the same tools as a CLI.
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// MCP server operations.
    #[command(subcommand)]
    Mcp(McpCommand),

    /// Verify credentials against the live site and report where each setting came from.
    Check,

    /// Write the default config to ~/.config/ujira/config.toml. Never overwrites.
    WriteConfig,

    /// Read a JIRA API token from stdin and store it in the OS keychain.
    SetToken,

    /// Remove the token from the OS keychain.
    DeleteToken,

    /// Search by JQL. One compact line per issue.
    Search {
        /// The JQL, e.g. 'project = PROJ AND status = Open ORDER BY updated DESC'.
        jql: String,
        /// Max issues (clamped to [1,100]).
        #[arg(short, long, default_value_t = 25)]
        limit: u32,
        /// Print the raw JIRA rows instead of the compact rendering.
        #[arg(long)]
        json: bool,
    },

    /// Show one issue.
    Issue {
        /// Issue key, e.g. PROJ-142.
        key: String,
        /// Include the comment thread.
        #[arg(short, long)]
        comments: bool,
        /// Cut each description/comment at this many chars.
        #[arg(long, default_value_t = 6000)]
        max_chars: usize,
        /// Print the full raw issue instead of the compact rendering.
        #[arg(long)]
        json: bool,
    },

    /// Read an issue's comment thread (newest N, printed oldest-first).
    Comments {
        key: String,
        #[arg(short, long, default_value_t = 25)]
        limit: u32,
        #[arg(long, default_value_t = 6000)]
        max_chars: usize,
        #[arg(long)]
        json: bool,
    },

    /// Post a comment. Pass `-` as the body to read it from stdin.
    Comment {
        key: String,
        /// The comment body, or `-` to read stdin (which is how you post a heredoc).
        body: String,
        /// `plain` (default) or `markdown` — markdown is converted to ADF for rich formatting.
        #[arg(long, value_enum, default_value = "plain")]
        description_format: ops::ProseFormat,
    },

    /// Create an issue.
    Create {
        /// Project key.
        #[arg(short, long)]
        project: String,
        /// Issue type NAME, e.g. Story, Bug, Epic.
        #[arg(short = 't', long = "type")]
        issue_type: String,
        /// Summary line.
        #[arg(short, long)]
        summary: String,
        /// Description, or `-` to read stdin.
        #[arg(short, long)]
        description: Option<String>,
        /// Parent issue key (an epic, in a company-managed project).
        #[arg(long)]
        parent: Option<String>,
        /// A label. Repeat for several.
        #[arg(short, long)]
        label: Vec<String>,
        /// Raw fields JSON, merged last: '{"customfield_10001":{"id":"…"}}'.
        #[arg(long)]
        fields: Option<String>,
        /// `plain` (default) or `markdown` — markdown is converted to ADF for rich formatting.
        #[arg(long, value_enum, default_value = "plain")]
        description_format: ops::ProseFormat,
    },

    /// Edit an issue's fields in place.
    Update {
        key: String,
        #[arg(short, long)]
        summary: Option<String>,
        /// New description, or `-` to read stdin.
        #[arg(short, long)]
        description: Option<String>,
        /// A label. Repeat for several. Replaces the existing set.
        #[arg(short, long)]
        label: Vec<String>,
        /// Raw fields JSON, merged last.
        #[arg(long)]
        fields: Option<String>,
        /// `plain` (default) or `markdown` — markdown is converted to ADF for rich formatting.
        #[arg(long, value_enum, default_value = "plain")]
        description_format: ops::ProseFormat,
    },

    /// Move an issue to another status. Omit the target to list what's available.
    Transition {
        key: String,
        /// Transition or status NAME (case-insensitive).
        to: Option<String>,
    },

    /// Link two issues. Omit everything to list the site's link types.
    Link {
        /// Link type name, e.g. Blocks.
        link_type: Option<String>,
        /// Source issue that performs the outward relationship (e.g. the blocker for Blocks).
        source: Option<String>,
        /// Target issue that receives the relationship (e.g. the blocked issue for Blocks).
        target: Option<String>,
    },

    /// Add labels to an issue without removing existing ones.
    AddLabels {
        key: String,
        /// A label to add. Repeat for several.
        #[arg(short, long, required = true)]
        label: Vec<String>,
    },

    /// Remove specific labels from an issue.
    RemoveLabels {
        key: String,
        /// A label to remove. Repeat for several.
        #[arg(short, long, required = true)]
        label: Vec<String>,
    },

    /// Upload a file as an attachment.
    Attach {
        key: String,
        /// Path to the file to attach.
        file: String,
        /// Override the filename sent to JIRA (defaults to the file's basename).
        #[arg(long)]
        filename: Option<String>,
        /// Print the raw JIRA attachment metadata instead of the compact summary.
        #[arg(long)]
        json: bool,
    },

    /// Delete an attachment by id.
    DeleteAttachment {
        /// Attachment id (from `ujira issue --json` or `ujira attach --json`).
        id: String,
    },

    /// Convert markdown to Atlassian Document Format (ADF) JSON. Reads stdin when input is `-`.
    MarkdownToAdf {
        /// Markdown text, or `-` to read stdin.
        #[arg(default_value = "-")]
        input: String,
    },

    /// Look up field ids by name substring, for the --fields escape hatch.
    Fields {
        /// Case-insensitive substring. Omit to list every field.
        #[arg(default_value = "")]
        query: String,
    },

    /// Find users by email, username, or display name. One `accountId  email  name` line per match.
    UserSearch {
        /// Email, username, or display name (substring match).
        query: String,
        /// Max users (clamped to [1,1000]).
        #[arg(short, long, default_value_t = 25)]
        limit: u32,
        /// Print the raw JIRA user objects instead of the compact rendering.
        #[arg(long)]
        json: bool,
    },

    /// List a project's components, one name per line, sorted.
    Components {
        /// Project key, e.g. PROJ.
        project: String,
        /// Print the raw JIRA component objects instead of the names.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Serve MCP over stdio.
    Serve,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    // These three run BEFORE config resolution: each exists to fix a config that doesn't load yet,
    // so requiring a working one would be a circle.
    match &cli.command {
        Command::WriteConfig => return write_config(),
        Command::SetToken => return set_token(),
        Command::DeleteToken => {
            let account = ujira::config::account()?;
            ujira::keychain::delete(&account)?;
            println!("removed the token for {account} from the keychain");
            return Ok(());
        }
        Command::MarkdownToAdf { input } => {
            let md = or_stdin(input.clone())?;
            let adf = ujira::adf::markdown_to_adf(&md);
            println!(
                "{}",
                serde_json::to_string(&adf).unwrap_or_else(|e| format!("error: {e}"))
            );
            return Ok(());
        }
        _ => {}
    }

    let jira = Arc::new(JiraClient::new(Config::load()?));

    let out = match cli.command {
        Command::Mcp(McpCommand::Serve) => return serve(jira).await,
        Command::Check => return check(&jira).await,
        Command::Search { jql, limit, json } => ops::search(&jira, &jql, limit, json).await?,
        Command::Issue {
            key,
            comments,
            max_chars,
            json,
        } => ops::issue(&jira, &key, comments, max_chars, json).await?,
        Command::Comments {
            key,
            limit,
            max_chars,
            json,
        } => ops::comments(&jira, &key, limit, max_chars, json).await?,
        Command::Comment {
            key,
            body,
            description_format,
        } => ops::add_comment(&jira, &key, &or_stdin(body)?, description_format).await?,
        Command::Create {
            project,
            issue_type,
            summary,
            description,
            parent,
            label,
            fields,
            description_format,
        } => {
            ops::create_issue(
                &jira,
                &project,
                &issue_type,
                summary,
                parent,
                IssueFields {
                    description: description.map(or_stdin).transpose()?,
                    labels: labels(label),
                    extra: parse_fields(fields)?,
                    ..Default::default()
                },
                description_format,
            )
            .await?
        }
        Command::Update {
            key,
            summary,
            description,
            label,
            fields,
            description_format,
        } => {
            ops::update_issue(
                &jira,
                &key,
                IssueFields {
                    summary,
                    description: description.map(or_stdin).transpose()?,
                    labels: labels(label),
                    extra: parse_fields(fields)?,
                },
                description_format,
            )
            .await?
        }
        Command::Transition { key, to } => ops::transition(&jira, &key, to.as_deref()).await?,
        Command::Link {
            link_type,
            source,
            target,
        } => {
            ops::link(
                &jira,
                link_type.as_deref(),
                source.as_deref(),
                target.as_deref(),
            )
            .await?
        }
        Command::AddLabels { key, label } => ops::add_labels(&jira, &key, &label).await?,
        Command::RemoveLabels { key, label } => ops::remove_labels(&jira, &key, &label).await?,
        Command::Attach {
            key,
            file,
            filename,
            json,
        } => {
            let path = std::path::Path::new(&file);
            let data =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            let name = filename
                .as_deref()
                .or_else(|| path.file_name().and_then(|n| n.to_str()))
                .unwrap_or("attachment");
            ops::add_attachment(&jira, &key, name, data, json).await?
        }
        Command::DeleteAttachment { id } => ops::delete_attachment(&jira, &id).await?,
        Command::Fields { query } => ops::fields(&jira, &query).await?,
        Command::UserSearch { query, limit, json } => {
            ops::user_search(&jira, &query, limit, json).await?
        }
        Command::Components { project, json } => ops::components(&jira, &project, json).await?,
        Command::WriteConfig
        | Command::SetToken
        | Command::DeleteToken
        | Command::MarkdownToAdf { .. } => unreachable!(),
    };
    println!("{}", out.trim_end());
    Ok(())
}

async fn serve(jira: Arc<JiraClient>) -> Result<()> {
    tracing::info!(site = %jira.config().base, "serving MCP over stdio");
    let service = JiraMcp::new(jira)
        .serve(stdio())
        .await
        .context("starting the MCP stdio service")?;
    service.waiting().await.context("serving MCP over stdio")?;
    Ok(())
}

/// Opt-in only: with neither `UJIRA_LOG` nor `RUST_LOG` set, no subscriber is installed and the
/// binary writes nothing but its answer. An agent running `ujira` inside a skill captures stderr
/// along with stdout, so any default-on log line would land in its context.
///
/// When enabled, output goes to stderr: stdout is the MCP transport under `mcp serve`, and the
/// pipeable answer everywhere else.
fn init_tracing() {
    let Some(filter) = std::env::var("UJIRA_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok()
        .filter(|f| !f.trim().is_empty())
    else {
        return;
    };
    match tracing_subscriber::EnvFilter::try_new(&filter) {
        Ok(filter) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
            .init(),
        Err(e) => eprintln!("ujira: ignoring invalid log filter {filter:?}: {e}"),
    }
}

/// `-` means "read it from stdin", the usual convention, and the only sane way to pass a multi-line
/// description or comment from a shell.
fn or_stdin(value: String) -> Result<String> {
    if value != "-" {
        return Ok(value);
    }
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).context("reading stdin")?;
    Ok(buf.trim_end().to_string())
}

/// An empty `--label` list means "don't touch labels", not "clear them".
fn labels(v: Vec<String>) -> Option<Vec<String>> {
    (!v.is_empty()).then_some(v)
}

fn parse_fields(raw: Option<String>) -> Result<Option<serde_json::Value>> {
    raw.map(|s| serde_json::from_str(&s).context("parsing --fields as JSON"))
        .transpose()
}

/// Drop the shipped template where [`Config::load`] will find it. Refuses to clobber an existing
/// file: the whole value of this command is that it's safe to run twice.
fn write_config() -> Result<()> {
    let path = ujira::config::config_path().context("no config directory (is HOME set?)")?;
    if path.exists() {
        println!("config already exists at {} (untouched)", path.display());
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&path, ujira::config::default_config_toml())
        .with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Store a token from stdin in the OS keychain, filed under the account email.
///
/// stdin rather than an argument on purpose: a token passed as `set-token ATATT…` lands in your
/// shell history and in `ps` output for every other user on the box.
fn set_token() -> Result<()> {
    let account = ujira::config::account()?;
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!("paste the JIRA API token for {account}, then press ctrl-d:");
    }
    let mut token = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut token).context("reading stdin")?;
    let token = token.trim();
    if token.is_empty() {
        anyhow::bail!("no token on stdin");
    }
    ujira::keychain::set(&account, token)?;
    println!("stored the token for {account} in the keychain");
    println!("verify with: ujira check");
    Ok(())
}

/// Print the resolved config and prove the credentials work. The field count is a cheap authenticated
/// call that every account can make, so a failure here is genuinely auth and not permissions.
async fn check(jira: &JiraClient) -> Result<()> {
    let cfg = jira.config();
    println!("site:    {}", cfg.base);
    println!("user:    {}", cfg.email);
    println!("access:  {}", cfg.access);
    println!("token:   {}", cfg.token_source);
    println!("fields:  {} curated custom fields", cfg.custom_fields.len());
    let fields = jira.fields().await.context("calling JIRA (auth check)")?;
    println!("auth:    ok ({} fields visible)", fields.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn link_help_names_source_and_target_roles() {
        let mut command = Cli::command()
            .find_subcommand("link")
            .expect("link subcommand")
            .clone();
        let help = command.render_long_help().to_string();
        assert!(help.contains("[SOURCE] [TARGET]"), "{help}");
        assert!(help.contains("performs the outward relationship"), "{help}");
    }
}
