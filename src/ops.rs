//! The shared operations, once.
//!
//! Both front ends are thin over this module: `crate::server` wraps each one in an MCP tool, and
//! the CLI wraps each in a subcommand. Neither owns any logic — an agent scripting `ujira search`
//! in a shell and an agent calling `jira_search` over MCP must get byte-identical output, or the
//! skill that teaches one is lying about the other.
//!
//! Every op returns `Result<String>`: the MCP side flattens an error into a readable line (a tool
//! error is data the model acts on), the CLI side exits non-zero (a failed command is a failed
//! command).

use crate::adf;
use crate::client::JiraClient;
use crate::render;
use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

/// Whether description/comment prose is plain text (api/v2) or markdown that should be converted to
/// ADF (api/v3). The default is plain text, preserving the existing behavior.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum ProseFormat {
    /// Send as-is via api/v2 (plain text / wiki markup).
    #[default]
    Plain,
    /// Convert markdown to ADF and send via api/v3 (rich formatting survives).
    Markdown,
}

// Strict on purpose: a lenient parse would turn a typo like "markdwn" into a silent plain-text
// post, and the model would never learn its formatting got dropped.
impl std::str::FromStr for ProseFormat {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "plain" => Ok(Self::Plain),
            "markdown" | "md" => Ok(Self::Markdown),
            other => bail!("unknown description_format {other:?} (use \"plain\" or \"markdown\")"),
        }
    }
}

/// Fields a create/update sets, assembled from typed args plus a raw escape hatch.
#[derive(Debug, Default)]
pub struct IssueFields {
    pub summary: Option<String>,
    pub description: Option<String>,
    pub labels: Option<Vec<String>>,
    /// Raw `{"customfield_N": …}` merged in LAST, so it can override anything above.
    pub extra: Option<Value>,
}

impl IssueFields {
    fn into_map(self) -> Result<Map<String, Value>> {
        let mut m = Map::new();
        put(&mut m, "summary", self.summary);
        put(&mut m, "description", self.description);
        put(&mut m, "labels", self.labels);
        merge_extra(&mut m, self.extra)?;
        Ok(m)
    }
}

/// JQL -> one compact line per issue (or the raw rows).
#[tracing::instrument(level = "debug", skip(jira))]
pub async fn search(jira: &JiraClient, jql: &str, limit: u32, json_out: bool) -> Result<String> {
    let rows = jira.search(jql, limit).await?;
    Ok(if json_out {
        dump(&Value::Array(rows))
    } else {
        render::search_rows(&rows)
    })
}

/// One issue, rendered compact (or raw).
#[tracing::instrument(level = "debug", skip(jira))]
pub async fn issue(
    jira: &JiraClient,
    key: &str,
    comments: bool,
    max_chars: usize,
    json_out: bool,
) -> Result<String> {
    let v = jira.get_issue(key, comments).await?;
    Ok(if json_out {
        dump(&v)
    } else {
        render::issue(&v, jira.config(), max_chars)
    })
}

/// The comment thread, newest `limit`, rendered oldest-first.
#[tracing::instrument(level = "debug", skip(jira))]
pub async fn comments(
    jira: &JiraClient,
    key: &str,
    limit: u32,
    max_chars: usize,
    json_out: bool,
) -> Result<String> {
    let mut list = jira.get_comments(key, limit).await?;
    // Fetched newest-first (so a limit keeps the RECENT ones); read oldest-first.
    list.reverse();
    Ok(if json_out {
        dump(&Value::Array(list))
    } else {
        render::comments(&list, max_chars)
    })
}

#[tracing::instrument(level = "debug", skip(jira, body))]
pub async fn add_comment(
    jira: &JiraClient,
    key: &str,
    body: &str,
    prose_format: ProseFormat,
) -> Result<String> {
    let id = match prose_format {
        ProseFormat::Plain => jira.add_comment(key, body).await?,
        ProseFormat::Markdown => {
            let body_adf = adf::markdown_to_adf(body);
            jira.add_comment_adf(key, body_adf).await?
        }
    };
    Ok(format!("commented (id {id}) {}", jira.browse_url(key)))
}

#[tracing::instrument(level = "debug", skip(jira, fields))]
pub async fn create_issue(
    jira: &JiraClient,
    project: &str,
    issue_type: &str,
    summary: String,
    parent: Option<String>,
    fields: IssueFields,
    prose_format: ProseFormat,
) -> Result<String> {
    let mut m = IssueFields {
        summary: Some(summary),
        ..fields
    }
    .into_map()?;
    if prose_format == ProseFormat::Markdown
        && let Some(desc) = m.get("description").and_then(Value::as_str)
    {
        let adf_val = adf::markdown_to_adf(desc);
        m.insert("description".to_string(), adf_val);
    }
    // Identity fields go in after, but never override an explicit `extra` — a caller who spells out
    // `issuetype` with an id means it.
    m.entry("project").or_insert(json!({ "key": project }));
    m.entry("issuetype")
        .or_insert(json!({ "name": issue_type }));
    if let Some(p) = parent {
        m.entry("parent").or_insert(json!({ "key": p }));
    }
    // api/v3 is required when the description is ADF.
    let key = if prose_format == ProseFormat::Markdown {
        jira.create_issue_v3(m).await?
    } else {
        jira.create_issue(m).await?
    };
    Ok(format!("created {key} {}", jira.browse_url(&key)))
}

#[tracing::instrument(level = "debug", skip(jira, fields))]
pub async fn update_issue(
    jira: &JiraClient,
    key: &str,
    fields: IssueFields,
    prose_format: ProseFormat,
) -> Result<String> {
    let mut m = fields.into_map()?;
    if m.is_empty() {
        bail!("nothing to update (pass summary, description, labels, or fields)");
    }
    if prose_format == ProseFormat::Markdown {
        if let Some(desc) = m.get("description").and_then(Value::as_str) {
            let adf_val = adf::markdown_to_adf(desc);
            m.insert("description".to_string(), adf_val);
        }
        jira.update_issue_v3(key, m).await?;
    } else {
        jira.update_issue(key, m).await?;
    }
    Ok(format!("updated {}", jira.browse_url(key)))
}

/// Move an issue by transition/status NAME. `None` lists what's reachable from here.
#[tracing::instrument(level = "debug", skip(jira))]
pub async fn transition(jira: &JiraClient, key: &str, to: Option<&str>) -> Result<String> {
    let available = jira.transitions(key).await?;
    let names = || {
        available
            .iter()
            .map(|(_, n)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let Some(want) = to else {
        return Ok(format!("transitions: {}", names()));
    };
    let Some((id, name)) = available
        .iter()
        .find(|(_, n)| n.eq_ignore_ascii_case(want.trim()))
    else {
        bail!("no transition named {want:?}; available: {}", names());
    };
    jira.transition(key, id).await?;
    Ok(format!("{key} -> {name}"))
}

/// Link two issues, or list the site's link types when `link_type` is `None`.
#[tracing::instrument(level = "debug", skip(jira))]
pub async fn link(
    jira: &JiraClient,
    link_type: Option<&str>,
    source: Option<&str>,
    target: Option<&str>,
) -> Result<String> {
    let Some(link_type) = link_type else {
        return Ok(render::link_types(&jira.link_types().await?));
    };
    let (Some(source), Some(target)) = (source, target) else {
        bail!("a link type needs both a source and a target issue key");
    };
    jira.link(link_type, source, target).await?;
    Ok(link_confirmation(link_type, source, target))
}

fn link_confirmation(link_type: &str, source: &str, target: &str) -> String {
    format!("linked source {source} to target {target} ({link_type})")
}

/// Add labels without replacing the existing set.
#[tracing::instrument(level = "debug", skip(jira))]
pub async fn add_labels(jira: &JiraClient, key: &str, labels: &[String]) -> Result<String> {
    if labels.is_empty() {
        bail!("at least one label is required");
    }
    jira.add_labels(key, labels).await?;
    Ok(format!(
        "added {} to {}",
        labels
            .iter()
            .map(|l| format!("#{l}"))
            .collect::<Vec<_>>()
            .join(" "),
        jira.browse_url(key),
    ))
}

/// Remove specific labels.
#[tracing::instrument(level = "debug", skip(jira))]
pub async fn remove_labels(jira: &JiraClient, key: &str, labels: &[String]) -> Result<String> {
    if labels.is_empty() {
        bail!("at least one label is required");
    }
    jira.remove_labels(key, labels).await?;
    Ok(format!(
        "removed {} from {}",
        labels
            .iter()
            .map(|l| format!("#{l}"))
            .collect::<Vec<_>>()
            .join(" "),
        jira.browse_url(key),
    ))
}

/// Upload a file as an attachment. Returns a one-line summary (or JSON).
#[tracing::instrument(level = "debug", skip(jira, data))]
pub async fn add_attachment(
    jira: &JiraClient,
    key: &str,
    filename: &str,
    data: Vec<u8>,
    json_out: bool,
) -> Result<String> {
    let v = jira.add_attachment(key, filename, data).await?;
    if json_out {
        return Ok(dump(&v));
    }
    // JIRA returns an array of attachment objects; we usually upload one file.
    let id = v
        .as_array()
        .and_then(|a| a.first())
        .and_then(|a| a.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    Ok(format!(
        "attached {filename} (id {id}) to {}",
        jira.browse_url(key)
    ))
}

/// Delete an attachment by id.
#[tracing::instrument(level = "debug", skip(jira))]
pub async fn delete_attachment(jira: &JiraClient, id: &str) -> Result<String> {
    jira.delete_attachment(id).await?;
    Ok(format!("deleted attachment {id}"))
}

/// Field ids by name substring — how you find the `customfield_NNNNN` for the escape hatch.
#[tracing::instrument(level = "debug", skip(jira))]
pub async fn fields(jira: &JiraClient, query: &str) -> Result<String> {
    Ok(render::fields(&jira.fields().await?, query))
}

/// Users by email, username, or display name -> `accountId  email  name` rows (or raw).
#[tracing::instrument(level = "debug", skip(jira))]
pub async fn user_search(
    jira: &JiraClient,
    query: &str,
    limit: u32,
    json_out: bool,
) -> Result<String> {
    let list = jira.user_search(query, limit).await?;
    Ok(if json_out {
        dump(&Value::Array(list))
    } else {
        render::users(&list)
    })
}

/// A project's component names, sorted (or raw).
#[tracing::instrument(level = "debug", skip(jira))]
pub async fn components(jira: &JiraClient, project: &str, json_out: bool) -> Result<String> {
    let list = jira.components(project).await?;
    Ok(if json_out {
        dump(&Value::Array(list))
    } else {
        render::components(&list)
    })
}

/// Set a field only when the caller supplied one — an absent arg must not clear the value in JIRA.
fn put<T: Into<Value>>(fields: &mut Map<String, Value>, key: &str, value: Option<T>) {
    if let Some(v) = value {
        fields.insert(key.to_string(), v.into());
    }
}

/// Merge the raw `fields` escape hatch last, so it can override anything the typed args set.
fn merge_extra(fields: &mut Map<String, Value>, extra: Option<Value>) -> Result<()> {
    match extra {
        None | Some(Value::Null) => Ok(()),
        Some(Value::Object(m)) => {
            fields.extend(m);
            Ok(())
        }
        Some(other) => bail!("`fields` must be a JSON object of field id -> value, got {other}"),
    }
}

/// `--json` output. Compact, not pretty-printed: the reader is a model or a `jq`, and indentation is
/// pure cost to one and invisible to the other.
fn dump(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| format!("error: serializing response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_fields_override_typed_args() {
        let m = IssueFields {
            summary: Some("typed".into()),
            extra: Some(json!({"summary": "raw"})),
            ..Default::default()
        }
        .into_map()
        .expect("object merges");
        assert_eq!(m["summary"], "raw");
    }

    #[test]
    fn a_non_object_escape_hatch_is_an_error() {
        let err = IssueFields {
            extra: Some(json!("nope")),
            ..Default::default()
        }
        .into_map()
        .expect_err("must reject");
        assert!(err.to_string().contains("must be a JSON object"), "{err}");
    }

    /// An empty update would be a no-op PUT against JIRA; say so instead of pretending it worked.
    #[test]
    fn empty_update_is_rejected_before_the_call() {
        let m = IssueFields::default()
            .into_map()
            .expect("empty is fine here");
        assert!(m.is_empty(), "the guard lives in update_issue, on this map");
    }

    #[test]
    fn link_confirmation_names_the_semantic_roles() {
        assert_eq!(
            link_confirmation("Depend", "PROJ-1", "PROJ-2"),
            "linked source PROJ-1 to target PROJ-2 (Depend)"
        );
    }
}
