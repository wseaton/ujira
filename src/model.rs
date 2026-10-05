//! Typed views of the Jira payloads [`crate::JiraClient`] parses for callers. Timestamps stay as
//! Jira's strings (e.g. `2026-10-04T12:00:00.000+0000`); unknown keys are ignored.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// A Jira account, as on comment and changelog authors and from `/myself`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub account_id: String,
    pub display_name: Option<String>,
    pub email_address: Option<String>,
    pub active: Option<bool>,
}

/// One changelog history: everything one author changed at one time.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChangelogEntry {
    pub id: String,
    /// `None` for changes Jira attributes to no account (some automation and imports).
    pub author: Option<Account>,
    pub created: String,
    pub items: Vec<ChangeItem>,
}

/// One field change within a [`ChangelogEntry`]. `from`/`to` are raw ids (option, user, status),
/// `from_string`/`to_string` the display text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeItem {
    pub field: String,
    pub field_id: Option<String>,
    #[serde(rename = "fieldtype")]
    pub field_type: Option<String>,
    pub from: Option<String>,
    pub from_string: Option<String>,
    pub to: Option<String>,
    pub to_string: Option<String>,
}

/// A comment read through api/3, so `body` is an ADF document.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Comment {
    pub id: String,
    pub author: Option<Account>,
    pub body: Value,
    pub created: String,
    pub updated: Option<String>,
}

/// Deserialize a list of raw items into `T`, naming the first item that does not fit.
pub(crate) fn parse_items<T: DeserializeOwned>(what: &str, items: Vec<Value>) -> Result<Vec<T>> {
    items
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            serde_json::from_value(item).with_context(|| format!("parsing jira {what} item {i}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::model::*;
    use serde_json::json;

    #[test]
    fn changelog_entry_reads_author_time_and_items() {
        let v = json!({
            "id": "100",
            "author": {"accountId": "557058:a", "displayName": "Ada", "active": true, "self": "x"},
            "created": "2026-10-04T12:00:00.000+0000",
            "items": [
                {"field": "status", "fieldtype": "jira", "fieldId": "status",
                 "from": "1", "fromString": "New", "to": "3", "toString": "In Progress"},
                {"field": "labels", "fieldtype": "jira", "from": null, "fromString": "",
                 "to": null, "toString": "triaged"},
            ],
        });
        let e: ChangelogEntry = serde_json::from_value(v).unwrap();
        assert_eq!(e.id, "100");
        assert_eq!(e.created, "2026-10-04T12:00:00.000+0000");
        let author = e.author.unwrap();
        assert_eq!(author.account_id, "557058:a");
        assert_eq!(author.display_name.as_deref(), Some("Ada"));
        assert_eq!(author.email_address, None);
        assert_eq!(
            e.items[0],
            ChangeItem {
                field: "status".into(),
                field_id: Some("status".into()),
                field_type: Some("jira".into()),
                from: Some("1".into()),
                from_string: Some("New".into()),
                to: Some("3".into()),
                to_string: Some("In Progress".into()),
            }
        );
        assert_eq!(e.items[1].from, None);
        assert_eq!(e.items[1].field_id, None);
        assert_eq!(e.items[1].to_string.as_deref(), Some("triaged"));
    }

    #[test]
    fn changelog_entry_allows_no_author() {
        let e: ChangelogEntry = serde_json::from_value(json!({
            "id": "1", "created": "2026-01-01T00:00:00.000+0000", "items": [],
        }))
        .unwrap();
        assert_eq!(e.author, None);
    }

    #[test]
    fn comment_keeps_the_adf_body() {
        let body = json!({"type": "doc", "version": 1, "content": []});
        let c: Comment = serde_json::from_value(json!({
            "id": "10001",
            "author": {"accountId": "557058:a"},
            "body": body,
            "created": "2026-10-04T12:00:00.000+0000",
            "updated": "2026-10-04T13:00:00.000+0000",
        }))
        .unwrap();
        assert_eq!(c.id, "10001");
        assert_eq!(c.author.unwrap().account_id, "557058:a");
        assert_eq!(c.body, body);
        assert_eq!(c.updated.as_deref(), Some("2026-10-04T13:00:00.000+0000"));
    }

    #[test]
    fn account_requires_an_account_id() {
        assert!(serde_json::from_value::<Account>(json!({"displayName": "Ada"})).is_err());
    }

    #[test]
    fn parse_items_names_the_bad_item() {
        let err = parse_items::<Account>(
            "user",
            vec![json!({"accountId": "a"}), json!({"accountId": 7})],
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "parsing jira user item 1");
    }
}
