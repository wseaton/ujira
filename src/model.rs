//! Typed views of the Jira payloads [`crate::JiraClient`] parses for callers. Timestamps stay as
//! Jira's strings (e.g. `2026-10-04T12:00:00.000+0000`); unknown keys are ignored.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;

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
    /// Who may see the comment (`{"type", "value", "identifier"}`), or `None` when it is public.
    #[serde(default)]
    pub visibility: Option<Value>,
}

/// A transition available from an issue's current status, with its screen fields.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transition {
    pub id: String,
    pub name: String,
    /// The status the transition leads to.
    pub to: Option<TransitionTarget>,
    #[serde(default)]
    pub has_screen: bool,
    /// Field id -> field metadata, from `expand=transitions.fields`.
    #[serde(default)]
    pub fields: BTreeMap<String, FieldMeta>,
}

impl Transition {
    /// Ids of the fields this transition requires.
    pub fn required_fields(&self) -> impl Iterator<Item = &str> {
        self.fields
            .iter()
            .filter(|(_, f)| f.required)
            .map(|(id, _)| id.as_str())
    }
}

/// The status a [`Transition`] leads to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TransitionTarget {
    pub id: String,
    pub name: String,
}

/// A field's edit metadata, as on a transition screen or from editmeta.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldMeta {
    pub required: bool,
    pub name: String,
    pub key: Option<String>,
    #[serde(default)]
    pub schema: Value,
    /// Legal values for option-like fields (resolution, selects), raw.
    #[serde(default)]
    pub allowed_values: Vec<Value>,
    #[serde(default)]
    pub has_default_value: bool,
    #[serde(default)]
    pub operations: Vec<String>,
}

/// The `fields` map of an editmeta response: field id -> metadata.
pub(crate) fn parse_edit_meta(body: Value) -> Result<BTreeMap<String, FieldMeta>> {
    let Value::Object(mut o) = body else {
        bail!("jira editmeta response is not an object");
    };
    let Some(fields) = o.remove("fields") else {
        bail!("jira editmeta response has no fields");
    };
    serde_json::from_value(fields).context("parsing jira editmeta fields")
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

/// The `transitions` array of a transitions response.
pub(crate) fn parse_transitions(body: Value) -> Result<Vec<Transition>> {
    let Value::Object(mut o) = body else {
        bail!("jira transitions response is not an object");
    };
    let Some(Value::Array(items)) = o.remove("transitions") else {
        bail!("jira transitions response has no transitions array");
    };
    parse_items("transition", items)
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
        assert_eq!(c.visibility, None);
    }

    #[test]
    fn comment_keeps_its_visibility_restriction_whole() {
        let visibility = json!({
            "type": "group",
            "value": "Red Hat Employee",
            "identifier": "13bd0387-d75c-4c18-9d37-5439e8bf984c",
        });
        let c: Comment = serde_json::from_value(json!({
            "id": "10002",
            "body": {"type": "doc", "version": 1, "content": []},
            "created": "2026-10-04T12:00:00.000+0000",
            "visibility": visibility,
        }))
        .unwrap();
        assert_eq!(c.visibility, Some(visibility));
        let public: Comment = serde_json::from_value(json!({
            "id": "10003",
            "body": {"type": "doc", "version": 1, "content": []},
            "created": "2026-10-04T12:00:00.000+0000",
            "visibility": null,
        }))
        .unwrap();
        assert_eq!(public.visibility, None);
    }

    #[test]
    fn account_reads_a_myself_payload() {
        let a: Account = serde_json::from_value(json!({
            "self": "https://site.atlassian.net/rest/api/3/user?accountId=557058:a",
            "accountId": "557058:a",
            "accountType": "atlassian",
            "emailAddress": "ada@example.com",
            "avatarUrls": {"48x48": "x"},
            "displayName": "Ada Lovelace",
            "active": true,
            "timeZone": "UTC",
            "locale": "en_US",
            "groups": {"size": 3, "items": []},
        }))
        .unwrap();
        assert_eq!(
            a,
            Account {
                account_id: "557058:a".into(),
                display_name: Some("Ada Lovelace".into()),
                email_address: Some("ada@example.com".into()),
                active: Some(true),
            }
        );
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

    #[test]
    fn transitions_parse_with_screen_fields() {
        let ts = parse_transitions(json!({
            "expand": "transitions",
            "transitions": [
                {
                    "id": "61", "name": "Close", "hasScreen": true,
                    "to": {"id": "6", "name": "Closed", "self": "x"},
                    "fields": {
                        "resolution": {
                            "required": true, "name": "Resolution", "key": "resolution",
                            "schema": {"type": "resolution", "system": "resolution"},
                            "operations": ["set"], "hasDefaultValue": false,
                            "allowedValues": [{"id": "10000", "name": "Done"}, {"id": "10001", "name": "Won't Do"}],
                        },
                        "customfield_10500": {
                            "required": false, "name": "Affects Testing",
                            "schema": {"type": "option", "custom": "x", "customId": 10500},
                            "operations": ["set"],
                        },
                    },
                },
                {"id": "11", "name": "Start", "to": {"id": "3", "name": "In Progress"}},
            ],
        }))
        .unwrap();
        assert_eq!(ts.len(), 2);
        let close = &ts[0];
        assert_eq!((close.id.as_str(), close.name.as_str()), ("61", "Close"));
        assert!(close.has_screen);
        assert_eq!(
            close.to,
            Some(TransitionTarget {
                id: "6".into(),
                name: "Closed".into()
            })
        );
        assert_eq!(close.required_fields().collect::<Vec<_>>(), ["resolution"]);
        let resolution = &close.fields["resolution"];
        assert_eq!(resolution.allowed_values.len(), 2);
        assert_eq!(resolution.operations, ["set"]);
        assert!(close.fields["customfield_10500"].allowed_values.is_empty());
        assert!(!ts[1].has_screen);
        assert!(ts[1].fields.is_empty());
        assert_eq!(ts[1].required_fields().count(), 0);
    }

    #[test]
    fn transitions_reject_a_malformed_response() {
        for (v, want) in [
            (json!([]), "not an object"),
            (json!({}), "no transitions array"),
            (json!({"transitions": {}}), "no transitions array"),
            (json!({"transitions": [{"name": "x"}]}), "transition item 0"),
        ] {
            let err = parse_transitions(v.clone()).unwrap_err().to_string();
            assert!(err.contains(want), "{v}: {err}");
        }
    }

    #[test]
    fn edit_meta_parses_each_field() {
        let meta = parse_edit_meta(json!({
            "fields": {
                "labels": {
                    "required": false, "name": "Labels", "key": "labels",
                    "schema": {"type": "array", "items": "string", "system": "labels"},
                    "operations": ["add", "set", "remove"],
                    "autoCompleteUrl": "https://x/rest/api/1.0/labels/suggest?query=",
                },
                "customfield_10860": {
                    "required": true, "name": "Embargo Status", "key": "customfield_10860",
                    "schema": {"type": "option", "custom": "select", "customId": 10860},
                    "operations": ["set"],
                    "allowedValues": [{"value": "True", "id": "1"}, {"value": "False", "id": "2"}],
                    "hasDefaultValue": true,
                },
            },
        }))
        .unwrap();
        assert_eq!(meta.len(), 2);
        let labels = &meta["labels"];
        assert_eq!(labels.name, "Labels");
        assert!(!labels.required);
        assert_eq!(labels.operations, ["add", "set", "remove"]);
        assert_eq!(labels.schema["type"], "array");
        assert!(labels.allowed_values.is_empty());
        let embargo = &meta["customfield_10860"];
        assert!(embargo.required && embargo.has_default_value);
        assert_eq!(embargo.key.as_deref(), Some("customfield_10860"));
        assert_eq!(embargo.allowed_values[1]["value"], "False");
        assert!(parse_edit_meta(json!({"fields": {}})).unwrap().is_empty());
    }

    #[test]
    fn edit_meta_rejects_a_malformed_response() {
        for (v, want) in [
            (json!([]), "not an object"),
            (json!({}), "has no fields"),
            (json!({"fields": []}), "parsing jira editmeta fields"),
            (
                json!({"fields": {"x": {"name": "X"}}}),
                "parsing jira editmeta fields",
            ),
        ] {
            let err = parse_edit_meta(v.clone()).unwrap_err().to_string();
            assert!(err.contains(want), "{v}: {err}");
        }
    }
}
