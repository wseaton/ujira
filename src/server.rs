//! The MCP tool surface: sixteen tools.
//!
//! Tool *descriptions* are themselves context the model pays for on every single turn, so they're
//! terse on purpose. The full-fat Atlassian MCP spends tens of thousands of tokens announcing tools
//! you never call; this one spends a few hundred.

use crate::client::JiraClient;
use crate::ops::{self, IssueFields};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

/// How much prose one description or comment may contribute before it's cut.
fn default_max_chars() -> usize {
    6000
}
fn default_limit() -> u32 {
    25
}
fn default_format() -> String {
    "text".into()
}
fn default_prose_format() -> String {
    "plain".into()
}
/// `serde_json::Value` generates an unconstrained schema (`{}`), so MCP clients don't know the
/// parameter is an object and may serialize it as a string. Pinning to `type: object` fixes that.
fn json_object_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), "object".into());
    m.into()
}

fn default_encoding() -> String {
    "utf8".into()
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    /// JQL, e.g. `project = PROJ AND labels = routing ORDER BY updated DESC`.
    pub jql: String,
    /// Max issues (clamped to [1,100]). Default 25.
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// `text` (default, compact) or `json` (raw rows).
    #[serde(default = "default_format")]
    pub format: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetIssueArgs {
    /// Issue key, e.g. `PROJ-142`.
    pub issue_key: String,
    /// Also fetch the comment thread. Default false (comments are usually the bulk of an issue).
    #[serde(default)]
    pub comments: bool,
    /// Cut each description/comment at this many chars. Default 6000.
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    /// `text` (default, ~10x smaller) or `json` (the full raw issue, every field).
    #[serde(default = "default_format")]
    pub format: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetCommentsArgs {
    /// Issue key.
    pub issue_key: String,
    /// Newest N comments (clamped to [1,100]), rendered oldest-first. Default 25.
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Cut each comment at this many chars. Default 6000.
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    /// `text` (default) or `json`.
    #[serde(default = "default_format")]
    pub format: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddCommentArgs {
    /// Issue key.
    pub issue_key: String,
    /// Comment body. Plain text / JIRA wiki markup by default; markdown when
    /// `description_format` says so.
    pub comment: String,
    /// `plain` (default) or `markdown` — when `markdown`, the body is converted to ADF and posted
    /// via api/v3 so rich formatting (headings, bold, code blocks, tables, links) survives.
    #[serde(default = "default_prose_format")]
    pub description_format: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateIssueArgs {
    /// Project key, e.g. `PROJ`.
    pub project: String,
    /// Issue type NAME, e.g. `Story`, `Bug`, `Epic`, `Feature`.
    pub issue_type: String,
    /// Summary line.
    pub summary: String,
    /// Description, plain text / JIRA wiki markup.
    #[serde(default)]
    pub description: Option<String>,
    /// Parent issue key (an epic, for a company-managed project).
    #[serde(default)]
    pub parent: Option<String>,
    /// Labels to set.
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    /// Escape hatch: extra raw `fields` merged in last, e.g.
    /// `{"customfield_10001": {"id": "…"}, "priority": {"name": "Major"}}`.
    /// Use `jira_fields` to find ids.
    #[serde(default)]
    #[schemars(schema_with = "json_object_schema")]
    pub fields: Option<Value>,
    /// `plain` (default) or `markdown` — when `markdown`, the description is converted to ADF
    /// (headings, bold, code blocks, tables, links) and sent via api/v3.
    #[serde(default = "default_prose_format")]
    pub description_format: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateIssueArgs {
    /// Issue key.
    pub issue_key: String,
    /// New summary.
    #[serde(default)]
    pub summary: Option<String>,
    /// New description (replaces the existing one).
    #[serde(default)]
    pub description: Option<String>,
    /// New label set (replaces the existing one).
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    /// Escape hatch: raw `fields` merged in last. Use `jira_fields` to find ids.
    #[serde(default)]
    #[schemars(schema_with = "json_object_schema")]
    pub fields: Option<Value>,
    /// `plain` (default) or `markdown` — when `markdown`, the description is converted to ADF
    /// (headings, bold, code blocks, tables, links) and sent via api/v3.
    #[serde(default = "default_prose_format")]
    pub description_format: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TransitionArgs {
    /// Issue key.
    pub issue_key: String,
    /// Target transition or status NAME (case-insensitive). Omit to list what's available.
    #[serde(default)]
    pub to: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LinkArgs {
    /// Link type name, e.g. `Blocks`, `Relates`. Omit to list the site's link types.
    #[serde(default)]
    pub link_type: Option<String>,
    /// Source issue that performs the outward relationship (e.g. the blocker for `Blocks`).
    #[serde(default, alias = "inward_key")]
    pub source_key: Option<String>,
    /// Target issue that receives the relationship (e.g. the blocked issue for `Blocks`).
    #[serde(default, alias = "outward_key")]
    pub target_key: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddLabelsArgs {
    /// Issue key.
    pub issue_key: String,
    /// Labels to add (does not remove existing ones).
    pub labels: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RemoveLabelsArgs {
    /// Issue key.
    pub issue_key: String,
    /// Labels to remove.
    pub labels: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddAttachmentArgs {
    /// Issue key.
    pub issue_key: String,
    /// Filename for the attachment (e.g. `report.md`).
    pub filename: String,
    /// File content as a UTF-8 string. For binary files, base64-encode the content and set
    /// `encoding` to `base64`.
    pub content: String,
    /// `utf8` (default) or `base64` — how `content` is encoded.
    #[serde(default = "default_encoding")]
    pub encoding: String,
    /// `text` (default) or `json` (raw attachment metadata from JIRA).
    #[serde(default = "default_format")]
    pub format: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteAttachmentArgs {
    /// Attachment id (from `jira_get_issue` format=json, or from `jira_add_attachment`).
    pub attachment_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MarkdownToAdfArgs {
    /// Markdown text to convert to ADF.
    pub markdown: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FieldsArgs {
    /// Case-insensitive substring of the field name, e.g. `team`. Empty lists every field.
    #[serde(default)]
    pub query: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UserSearchArgs {
    /// Email, username, or display name (substring match).
    pub query: String,
    /// Max users, 1 to 1000. Default 25.
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// `text` (default, `accountId  email  name` rows) or `json` (raw user objects).
    #[serde(default = "default_format")]
    pub format: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ComponentsArgs {
    /// Project key, e.g. `PROJ`.
    pub project: String,
    /// `text` (default, one sorted name per line) or `json` (raw component objects).
    #[serde(default = "default_format")]
    pub format: String,
}

/// The MCP server. One shared [`JiraClient`]; every tool is a thin call + render.
#[derive(Clone)]
pub struct JiraMcp {
    jira: Arc<JiraClient>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl JiraMcp {
    pub fn new(jira: Arc<JiraClient>) -> Self {
        Self {
            jira,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Search JIRA by JQL. Returns one compact line per issue: KEY [Type/Status] \
        Summary @assignee #labels. Use jira_get_issue for detail."
    )]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_search(&self, Parameters(a): Parameters<SearchArgs>) -> String {
        flatten(ops::search(&self.jira, &a.jql, a.limit, json_wanted(&a.format)).await)
    }

    #[tool(
        description = "Fetch one issue as compact text: header fields, links, subtasks, curated \
        custom fields (nulls dropped), then the description. Set comments=true for the thread, \
        format=json for every raw field."
    )]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_get_issue(&self, Parameters(a): Parameters<GetIssueArgs>) -> String {
        flatten(
            ops::issue(
                &self.jira,
                &a.issue_key,
                a.comments,
                a.max_chars,
                json_wanted(&a.format),
            )
            .await,
        )
    }

    #[tool(description = "Read an issue's comment thread (newest N, rendered oldest-first).")]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_get_comments(&self, Parameters(a): Parameters<GetCommentsArgs>) -> String {
        flatten(
            ops::comments(
                &self.jira,
                &a.issue_key,
                a.limit,
                a.max_chars,
                json_wanted(&a.format),
            )
            .await,
        )
    }

    #[tool(
        description = "Post a comment. Set description_format='markdown' for rich formatting \
        (headings, bold, code blocks, tables, links)."
    )]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_add_comment(&self, Parameters(a): Parameters<AddCommentArgs>) -> String {
        let prose = match a.description_format.parse() {
            Ok(p) => p,
            Err(e) => return format!("error: {e:#}"),
        };
        flatten(ops::add_comment(&self.jira, &a.issue_key, &a.comment, prose).await)
    }

    #[tool(description = "Create an issue. Returns the new key and url. Set \
        description_format='markdown' for rich description formatting.")]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_create_issue(&self, Parameters(a): Parameters<CreateIssueArgs>) -> String {
        let prose = match a.description_format.parse() {
            Ok(p) => p,
            Err(e) => return format!("error: {e:#}"),
        };
        flatten(
            ops::create_issue(
                &self.jira,
                &a.project,
                &a.issue_type,
                a.summary,
                a.parent,
                IssueFields {
                    description: a.description,
                    labels: a.labels,
                    extra: a.fields,
                    ..Default::default()
                },
                prose,
            )
            .await,
        )
    }

    #[tool(
        description = "Edit an issue's fields in place. Set description_format='markdown' for \
        rich description formatting."
    )]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_update_issue(&self, Parameters(a): Parameters<UpdateIssueArgs>) -> String {
        let prose = match a.description_format.parse() {
            Ok(p) => p,
            Err(e) => return format!("error: {e:#}"),
        };
        flatten(
            ops::update_issue(
                &self.jira,
                &a.issue_key,
                IssueFields {
                    summary: a.summary,
                    description: a.description,
                    labels: a.labels,
                    extra: a.fields,
                },
                prose,
            )
            .await,
        )
    }

    #[tool(
        description = "Move an issue to another status by transition/status name. Omit `to` to list \
        the transitions available from the current status."
    )]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_transition(&self, Parameters(a): Parameters<TransitionArgs>) -> String {
        flatten(ops::transition(&self.jira, &a.issue_key, a.to.as_deref()).await)
    }

    #[tool(
        description = "Link source_key to target_key using the link type's outward relationship. \
        For example, Depend with source A and target B means A depends on B. Omit `link_type` to \
        list the site's link types with their direction words."
    )]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_link_issues(&self, Parameters(a): Parameters<LinkArgs>) -> String {
        flatten(
            ops::link(
                &self.jira,
                a.link_type.as_deref(),
                a.source_key.as_deref(),
                a.target_key.as_deref(),
            )
            .await,
        )
    }

    #[tool(description = "Add labels to an issue without removing existing ones.")]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_add_labels(&self, Parameters(a): Parameters<AddLabelsArgs>) -> String {
        flatten(ops::add_labels(&self.jira, &a.issue_key, &a.labels).await)
    }

    #[tool(description = "Remove specific labels from an issue.")]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_remove_labels(&self, Parameters(a): Parameters<RemoveLabelsArgs>) -> String {
        flatten(ops::remove_labels(&self.jira, &a.issue_key, &a.labels).await)
    }

    #[tool(
        description = "Upload a file attachment to an issue. Pass content as UTF-8 text, or \
        base64-encode binary content and set encoding='base64'."
    )]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_add_attachment(&self, Parameters(a): Parameters<AddAttachmentArgs>) -> String {
        let data = if a.encoding.eq_ignore_ascii_case("base64") {
            use ::base64::Engine;
            match ::base64::engine::general_purpose::STANDARD.decode(&a.content) {
                Ok(d) => d,
                Err(e) => return format!("error: invalid base64: {e}"),
            }
        } else {
            a.content.into_bytes()
        };
        flatten(
            ops::add_attachment(
                &self.jira,
                &a.issue_key,
                &a.filename,
                data,
                json_wanted(&a.format),
            )
            .await,
        )
    }

    #[tool(description = "Delete an attachment by id.")]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_delete_attachment(
        &self,
        Parameters(a): Parameters<DeleteAttachmentArgs>,
    ) -> String {
        flatten(ops::delete_attachment(&self.jira, &a.attachment_id).await)
    }

    #[tool(
        description = "Convert markdown to Atlassian Document Format (ADF) JSON. Use when you need \
        to build ADF for the `fields` escape hatch (e.g. setting description via raw fields on \
        api/v3). For most cases, use description_format='markdown' on create/update/comment instead."
    )]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_markdown_to_adf(&self, Parameters(a): Parameters<MarkdownToAdfArgs>) -> String {
        let adf = crate::adf::markdown_to_adf(&a.markdown);
        serde_json::to_string(&adf).unwrap_or_else(|e| format!("error: {e}"))
    }

    #[tool(
        description = "Look up field ids by name (e.g. 'team' -> customfield_10001) for the `fields` \
        escape hatch on create/update."
    )]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_fields(&self, Parameters(a): Parameters<FieldsArgs>) -> String {
        flatten(ops::fields(&self.jira, &a.query).await)
    }

    #[tool(
        description = "Find users by email, username, or display name. Returns `accountId  email  \
        displayName` per match; use the accountId for reporter/assignee fields."
    )]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_user_search(&self, Parameters(a): Parameters<UserSearchArgs>) -> String {
        flatten(ops::user_search(&self.jira, &a.query, a.limit, json_wanted(&a.format)).await)
    }

    #[tool(description = "List a project's components, one name per line, sorted.")]
    #[tracing::instrument(level = "debug", skip_all)]
    async fn jira_components(&self, Parameters(a): Parameters<ComponentsArgs>) -> String {
        flatten(ops::components(&self.jira, &a.project, json_wanted(&a.format)).await)
    }
}

fn json_wanted(format: &str) -> bool {
    format.eq_ignore_ascii_case("json")
}

/// An op's error becomes a plain line the model can read (and act on) rather than a protocol fault.
/// The CLI does the opposite with the same `Result`: it exits non-zero.
fn flatten(r: anyhow::Result<String>) -> String {
    r.unwrap_or_else(|e| format!("error: {e:#}"))
}

// Dispatch through the INSTANCE router, not rmcp's `Self::tool_router()` default: the router is
// built once in `new` and callers may adjust it before serving.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for JiraMcp {
    /// Advertise the `tools` capability during `initialize`. Without it a spec-compliant client
    /// (Claude Code) connects, sees no tools capability, and never calls `tools/list` — the server
    /// looks connected but exposes nothing.
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "JIRA Cloud. Read tools return compact text; pass format=\"json\" for raw payloads.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn server() -> JiraMcp {
        JiraMcp::new(Arc::new(JiraClient::new(Config {
            base: "https://x.atlassian.net".into(),
            email: "me@x".into(),
            token: "t".into(),
            token_source: crate::config::TokenSource::Inline,
            access: crate::config::Access::ReadWrite,
            custom_fields: Vec::new(),
        })))
    }

    /// The handshake must declare tools, else clients never list them (a bug that presents as
    /// "connected, zero tools").
    #[test]
    fn get_info_advertises_tools() {
        assert!(server().get_info().capabilities.tools.is_some());
    }

    /// The whole point of the rewrite: a small, fixed tool surface. If this count grows, the context
    /// cost grows on every turn — make it a deliberate decision, not a drift.
    #[test]
    fn tool_surface_stays_small() {
        let names: Vec<_> = JiraMcp::tool_router()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(names.len(), 16, "tools: {names:?}");
        assert!(names.contains(&"jira_get_issue".to_string()), "{names:?}");
        assert!(names.contains(&"jira_add_labels".to_string()), "{names:?}");
        assert!(
            names.contains(&"jira_add_attachment".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(&"jira_markdown_to_adf".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"jira_user_search".to_string()), "{names:?}");
        assert!(names.contains(&"jira_components".to_string()), "{names:?}");
    }

    #[test]
    fn link_args_accept_semantic_and_legacy_field_names() {
        let semantic: LinkArgs = serde_json::from_value(serde_json::json!({
            "link_type": "Depend",
            "source_key": "PROJ-1",
            "target_key": "PROJ-2"
        }))
        .expect("semantic link arguments");
        assert_eq!(semantic.source_key.as_deref(), Some("PROJ-1"));
        assert_eq!(semantic.target_key.as_deref(), Some("PROJ-2"));

        let legacy: LinkArgs = serde_json::from_value(serde_json::json!({
            "link_type": "Depend",
            "inward_key": "PROJ-1",
            "outward_key": "PROJ-2"
        }))
        .expect("legacy link arguments");
        assert_eq!(legacy.source_key.as_deref(), Some("PROJ-1"));
        assert_eq!(legacy.target_key.as_deref(), Some("PROJ-2"));
    }
}
