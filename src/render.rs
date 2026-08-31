//! Token-frugal rendering: JIRA's JSON is mostly punctuation and self-links, and an LLM pays for all
//! of it. Every read tool renders to compact plain text by default (`format: "json"` opts back into
//! the raw payload when something downstream actually needs to parse it).
//!
//! The savings come from three places, in order of size:
//!   1. dropping fields nobody reads (avatar urls, `self` links, ids, icon urls, `1..n` empty fields),
//!   2. dropping JSON syntax itself (`{"status":{"name":"In Progress"}}` -> `In Progress`),
//!   3. de-noising the prose (CRLF, blank-line runs, `{color}` macros) that Jira's editor leaves behind.
//!
//! On a real PROJ issue that's roughly a 10x cut versus the raw `/rest/api/2/issue` body.

use crate::client::truncate;
use crate::config::Config;
use serde_json::Value;

/// One line per issue: `KEY [Type/Status] Summary @assignee #label`.
pub fn search_rows(issues: &[Value]) -> String {
    let mut out = String::new();
    out.push_str(&format!("{} issues\n", issues.len()));
    for issue in issues {
        let f = issue.get("fields");
        let key = s(issue.get("key"));
        let ty = ptr(f, "/issuetype/name");
        let status = ptr(f, "/status/name");
        let summary = s(f.and_then(|f| f.get("summary")));
        out.push_str(&format!("{key} [{ty}/{status}] {summary}"));
        let assignee = ptr(f, "/assignee/displayName");
        if !assignee.is_empty() {
            out.push_str(&format!(" @{assignee}"));
        }
        for label in f
            .and_then(|f| f.get("labels"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            out.push_str(&format!(" #{}", s(Some(label))));
        }
        out.push('\n');
    }
    out
}

/// The full single-issue view: a header block of `key: value` lines, then the description, then any
/// comments the caller asked for.
pub fn issue(issue: &Value, cfg: &Config, max_description_chars: usize) -> String {
    let f = issue.get("fields");
    let key = s(issue.get("key"));
    let mut out = String::new();
    out.push_str(&format!("{key} {}\n", s(f.and_then(|f| f.get("summary")))));
    out.push_str(&format!(
        "{}/{}",
        ptr(f, "/issuetype/name"),
        ptr(f, "/status/name")
    ));
    let priority = ptr(f, "/priority/name");
    if !priority.is_empty() {
        out.push_str(&format!("/{priority}"));
    }
    out.push('\n');

    line(&mut out, "assignee", &ptr(f, "/assignee/displayName"));
    line(&mut out, "reporter", &ptr(f, "/reporter/displayName"));
    line(&mut out, "parent", &parent(f));
    line(&mut out, "labels", &joined(f, "labels"));
    line(&mut out, "components", &named_list(f, "components"));
    line(&mut out, "fix_versions", &named_list(f, "fixVersions"));
    line(&mut out, "created", &date(f, "created"));
    line(&mut out, "updated", &date(f, "updated"));
    line(&mut out, "due", &date(f, "duedate"));
    for (name, id) in &cfg.custom_fields {
        line(&mut out, name, &scalar(f.and_then(|f| f.get(id))));
    }
    line(&mut out, "links", &links(f));
    line(&mut out, "subtasks", &subtasks(f));

    let desc = clean(&s(f.and_then(|f| f.get("description"))));
    if !desc.is_empty() {
        out.push_str("\n## description\n");
        out.push_str(&truncate(&desc, max_description_chars));
        out.push('\n');
    }

    if let Some(list) = f
        .and_then(|f| f.pointer("/comment/comments"))
        .and_then(Value::as_array)
        && !list.is_empty()
    {
        out.push('\n');
        out.push_str(&comments(list, max_description_chars));
    }
    out
}

/// `## comments (n)` then one `date author: body` block each, oldest first (the order they read in).
pub fn comments(list: &[Value], max_chars: usize) -> String {
    let mut out = format!("## comments ({})\n", list.len());
    for c in list {
        let author = ptr(Some(c), "/author/displayName");
        let when = day(&s(c.get("created")));
        let body = clean(&s(c.get("body")));
        out.push_str(&format!(
            "- {when} {author}: {}\n",
            truncate(&body, max_chars)
        ));
    }
    out
}

/// Semantic source/target descriptions per site link type, for `jira_link_issues`.
pub fn link_types(types: &[Value]) -> String {
    let mut out = String::new();
    for t in types {
        out.push_str(&format!(
            "{} (source -> target: {}; target -> source: {})\n",
            ptr(Some(t), "/name"),
            ptr(Some(t), "/outward"),
            ptr(Some(t), "/inward")
        ));
    }
    out
}

/// `customfield_NNNNN  Field Name` rows, filtered by a substring of the name.
pub fn fields(all: &[Value], query: &str) -> String {
    let q = query.to_ascii_lowercase();
    let mut out = String::new();
    let mut n = 0;
    for f in all {
        let name = ptr(Some(f), "/name");
        if !q.is_empty() && !name.to_ascii_lowercase().contains(&q) {
            continue;
        }
        out.push_str(&format!("{}  {name}\n", ptr(Some(f), "/id")));
        n += 1;
    }
    format!("{n} fields\n{out}")
}

/// `accountId  emailAddress  displayName` per user, for `jira_user_search`.
pub fn users(list: &[Value]) -> String {
    let mut out = String::new();
    for u in list {
        out.push_str(&format!(
            "{}  {}  {}\n",
            ptr(Some(u), "/accountId"),
            ptr(Some(u), "/emailAddress"),
            ptr(Some(u), "/displayName")
        ));
    }
    out
}

/// One component name per line, sorted, for `jira_components`.
pub fn components(list: &[Value]) -> String {
    let mut names: Vec<String> = list.iter().map(|c| ptr(Some(c), "/name")).collect();
    names.sort_unstable();
    let mut out = names.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Append `key: value` when the value is non-empty.
fn line(out: &mut String, key: &str, value: &str) {
    if !value.is_empty() {
        out.push_str(&format!("{key}: {value}\n"));
    }
}

/// `KEY (Status) Summary` for the parent epic/story.
fn parent(f: Option<&Value>) -> String {
    let Some(p) = f.and_then(|f| f.get("parent")) else {
        return String::new();
    };
    format!("{} {}", s(p.get("key")), s(p.pointer("/fields/summary")))
        .trim()
        .to_string()
}

/// `blocks PROJ-99 (Open); is blocked by PROJ-12 (Closed)` — direction words included, because
/// "linked to" without a direction is useless for planning.
fn links(f: Option<&Value>) -> String {
    let Some(list) = f
        .and_then(|f| f.get("issuelinks"))
        .and_then(Value::as_array)
    else {
        return String::new();
    };
    list.iter()
        .filter_map(|l| {
            let (dir, other) = if let Some(o) = l.get("outwardIssue") {
                (ptr(Some(l), "/type/outward"), o)
            } else {
                (ptr(Some(l), "/type/inward"), l.get("inwardIssue")?)
            };
            Some(format!(
                "{dir} {} ({})",
                s(other.get("key")),
                ptr(Some(other), "/fields/status/name")
            ))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn subtasks(f: Option<&Value>) -> String {
    let Some(list) = f.and_then(|f| f.get("subtasks")).and_then(Value::as_array) else {
        return String::new();
    };
    list.iter()
        .map(|t| {
            format!(
                "{} ({})",
                s(t.get("key")),
                ptr(Some(t), "/fields/status/name")
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `[{name: …}, …]` -> `a, b`.
fn named_list(f: Option<&Value>, key: &str) -> String {
    f.and_then(|f| f.get(key))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|e| ptr(Some(e), "/name"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn joined(f: Option<&Value>, key: &str) -> String {
    f.and_then(|f| f.get(key))
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|e| s(Some(e))).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

/// Timestamps trimmed to the day: `2026-07-01T09:12:33.000+0000` is 28 tokens of noise for one fact.
fn date(f: Option<&Value>, key: &str) -> String {
    day(&s(f.and_then(|f| f.get(key))))
}

fn day(ts: &str) -> String {
    ts.split('T').next().unwrap_or(ts).to_string()
}

/// Collapse a JIRA field value to its meaningful scalar: an option/user/version object to its
/// `value`/`name`/`displayName`, an array element-wise, a scalar as-is.
fn scalar(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|e| scalar(Some(e)))
            .filter(|e| !e.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Object(m)) => ["value", "name", "displayName", "text"]
            .iter()
            .find_map(|k| m.get(*k).map(|x| scalar(Some(x))))
            .unwrap_or_default(),
        // Jira hands back every number as a float; `42` reads better than `42.0` and costs less.
        Some(Value::Number(n)) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
            _ => n.to_string(),
        },
        Some(other) => other.to_string(),
    }
}

/// A JSON string without its quotes; anything else via its compact JSON form.
fn s(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn ptr(v: Option<&Value>, path: &str) -> String {
    s(v.and_then(|v| v.pointer(path)))
}

/// De-noise wiki prose: CRLF -> LF, `{color:#ff0000}…{color}` markup dropped (pure styling), runs of
/// blank lines collapsed to one, trailing spaces trimmed. Content is never reordered or reworded.
fn clean(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut out = String::with_capacity(text.len());
    let mut blanks = 0;
    for raw in text.lines() {
        let line = strip_color(raw).trim_end().to_string();
        if line.is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.trim().to_string()
}

/// Drop `{color…}` macros wherever they appear in a line.
fn strip_color(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("{color") {
        out.push_str(&rest[..start]);
        match rest[start..].find('}') {
            Some(end) => rest = &rest[start + end + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg() -> Config {
        Config {
            base: "https://x.atlassian.net".into(),
            email: "me@x".into(),
            token: "t".into(),
            token_source: crate::config::TokenSource::Inline,
            access: crate::config::Access::ReadWrite,
            custom_fields: vec![
                ("team".into(), "customfield_10001".into()),
                ("rice_score".into(), "customfield_10864".into()),
                ("blocked".into(), "customfield_10517".into()),
                ("absent".into(), "customfield_99999".into()),
            ],
        }
    }

    /// A real-shaped `/rest/api/2/issue` payload (the field shapes are exactly what Jira returns).
    fn rfe() -> Value {
        json!({
            "key": "PROJ-142",
            "fields": {
                "summary": "Cache-aware request routing",
                "issuetype": {"name": "Story"},
                "status": {"name": "In Progress"},
                "priority": null,
                "assignee": {"displayName": "Sam Rivera"},
                "labels": ["routing", "rfe"],
                "components": [{"name": "epp"}, {"name": "vllm"}],
                "fixVersions": [],
                "created": "2026-05-02T09:12:33.000+0000",
                "updated": "2026-07-01T11:00:00.000+0000",
                "description": "line one\r\n\r\n\r\n{color:#ff0000}red bit{color} tail   \r\nline two",
                "parent": {"key": "PROJ-7", "fields": {"summary": "Routing epic"}},
                "issuelinks": [
                    {"type": {"outward": "blocks", "inward": "is blocked by"},
                     "outwardIssue": {"key": "PROJ-99", "fields": {"status": {"name": "Open"}}}},
                    {"type": {"outward": "blocks", "inward": "is blocked by"},
                     "inwardIssue": {"key": "PROJ-12", "fields": {"status": {"name": "Closed"}}}}
                ],
                "customfield_10864": 42.0,
                "customfield_10001": {"name": "Platform Squad"},
                "customfield_10517": {"value": "No"},
                "customfield_10986": "opaque-forge-blob"
            }
        })
    }

    #[test]
    fn issue_renders_flat_and_omits_empties() {
        let out = issue(&rfe(), &cfg(), 4000);
        assert!(out.starts_with("PROJ-142 Cache-aware request routing\nStory/In Progress\n"));
        assert!(out.contains("assignee: Sam Rivera\n"));
        assert!(out.contains("parent: PROJ-7 Routing epic\n"));
        assert!(out.contains("labels: routing, rfe\n"));
        assert!(out.contains("components: epp, vllm\n"));
        assert!(
            out.contains("created: 2026-05-02\n"),
            "dates trim to the day"
        );
        assert!(out.contains("team: Platform Squad\n"), "object -> .name");
        assert!(out.contains("blocked: No\n"), "object -> .value");
        assert!(out.contains("rice_score: 42\n"), "scalar passthrough");
        assert!(
            out.contains("links: blocks PROJ-99 (Open); is blocked by PROJ-12 (Closed)\n"),
            "links carry direction: {out}"
        );
        // Empty / null / uncurated fields never appear.
        assert!(!out.contains("priority"), "null priority is dropped");
        assert!(!out.contains("fix_versions"), "empty array is dropped");
        assert!(!out.contains("absent"), "missing custom field is dropped");
        assert!(
            !out.contains("opaque-forge-blob"),
            "uncurated field is dropped"
        );
    }

    #[test]
    fn description_is_denoised() {
        let out = issue(&rfe(), &cfg(), 4000);
        let (_, desc) = out.split_once("## description\n").expect("a description");
        assert_eq!(desc.trim(), "line one\n\nred bit tail\nline two");
    }

    #[test]
    fn link_types_render_semantic_source_and_target_directions() {
        let types = vec![json!({
            "name": "Depend",
            "inward": "is depended on by",
            "outward": "depends on"
        })];
        assert_eq!(
            link_types(&types),
            "Depend (source -> target: depends on; target -> source: is depended on by)\n"
        );
    }

    #[test]
    fn description_truncates_at_the_cap() {
        let out = issue(&rfe(), &cfg(), 8);
        assert!(out.contains("line one… [truncated]"), "{out}");
    }

    /// The rendered issue must be dramatically smaller than the JSON it came from — that's the
    /// entire point of this module, so it gets a regression guard.
    #[test]
    fn rendering_beats_raw_json_by_a_lot() {
        let raw = rfe().to_string().len();
        let rendered = issue(&rfe(), &cfg(), 4000).len();
        assert!(
            rendered * 2 < raw,
            "rendered {rendered} bytes vs raw {raw}: not worth the module"
        );
    }

    #[test]
    fn search_rows_are_one_line_each() {
        let rows = search_rows(&[rfe(), json!({"key": "X-1", "fields": {"summary": "bare"}})]);
        let lines: Vec<_> = rows.lines().collect();
        assert_eq!(lines[0], "2 issues");
        assert_eq!(
            lines[1],
            "PROJ-142 [Story/In Progress] Cache-aware request routing @Sam Rivera #routing #rfe"
        );
        assert_eq!(
            lines[2], "X-1 [/] bare",
            "missing fields render empty, not absent"
        );
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn comments_render_oldest_first_with_dates() {
        let out = comments(
            &[json!({
                "author": {"displayName": "Jo Park"},
                "created": "2026-07-02T10:00:00.000+0000",
                "body": "looks good\r\n\r\n\r\nship it"
            })],
            4000,
        );
        assert_eq!(
            out,
            "## comments (1)\n- 2026-07-02 Jo Park: looks good\n\nship it\n"
        );
    }

    #[test]
    fn users_render_one_row_per_match_with_missing_fields_blank() {
        let out = users(&[
            json!({"accountId": "5b1", "emailAddress": "jo@x", "displayName": "Jo Park"}),
            json!({"accountId": "5b2", "displayName": "Hidden Email"}),
        ]);
        assert_eq!(out, "5b1  jo@x  Jo Park\n5b2    Hidden Email\n");
        assert_eq!(users(&[]), "");
    }

    #[test]
    fn components_render_sorted_names() {
        let out = components(&[
            json!({"id": "2", "name": "gateway"}),
            json!({"id": "1", "name": "Auth"}),
            json!({"id": "3", "name": "cache"}),
        ]);
        assert_eq!(out, "Auth\ncache\ngateway\n");
        assert_eq!(components(&[]), "");
    }
}
