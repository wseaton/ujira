//! Reading issue fields by display name: name -> id resolution over `/rest/api/3/field` metadata,
//! and picking the resolved values back out of an issue response.

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

/// Field display name -> every field id carrying that name, from `/rest/api/3/field`.
pub(crate) struct FieldIndex {
    ids_by_name: HashMap<String, Vec<String>>,
}

impl FieldIndex {
    pub(crate) fn from_metadata(metadata: &Value) -> Result<Self> {
        let Some(fields) = metadata.as_array() else {
            bail!("jira field metadata is not an array");
        };
        let mut ids_by_name: HashMap<String, Vec<String>> = HashMap::new();
        for (i, field) in fields.iter().enumerate() {
            let (Some(id), Some(name)) = (
                field.get("id").and_then(Value::as_str),
                field.get("name").and_then(Value::as_str),
            ) else {
                bail!("jira field metadata entry {i} lacks a string id and name");
            };
            ids_by_name
                .entry(name.to_string())
                .or_default()
                .push(id.to_string());
        }
        Ok(Self { ids_by_name })
    }

    /// `(name, id)` for each distinct name, in first-seen order.
    pub(crate) fn resolve(
        &self,
        names: &[&str],
    ) -> Result<Vec<(String, String)>, FieldResolutionError> {
        let mut resolved = Vec::new();
        let mut err = FieldResolutionError::default();
        let mut seen = HashSet::new();
        for &name in names {
            if !seen.insert(name) {
                continue;
            }
            match self.ids_by_name.get(name).map(Vec::as_slice) {
                Some([id]) => resolved.push((name.to_string(), id.clone())),
                Some(ids) if ids.len() > 1 => err.ambiguous.push(AmbiguousField {
                    name: name.to_string(),
                    ids: ids.to_vec(),
                }),
                _ => err.unresolved.push(name.to_string()),
            }
        }
        if err.unresolved.is_empty() && err.ambiguous.is_empty() {
            Ok(resolved)
        } else {
            Err(err)
        }
    }
}

/// Display names [`crate::JiraClient::get_fields_by_name`] could not map to exactly one field id.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FieldResolutionError {
    /// Names no field on the site carries.
    pub unresolved: Vec<String>,
    /// Names more than one field carries.
    pub ambiguous: Vec<AmbiguousField>,
}

/// A display name shared by several fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousField {
    pub name: String,
    pub ids: Vec<String>,
}

impl std::fmt::Display for FieldResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("jira field names did not resolve to exactly one field")?;
        if !self.unresolved.is_empty() {
            let names: Vec<String> = self.unresolved.iter().map(|n| format!("{n:?}")).collect();
            write!(f, "; no such field: {}", names.join(", "))?;
        }
        if !self.ambiguous.is_empty() {
            let names: Vec<String> = self
                .ambiguous
                .iter()
                .map(|a| format!("{:?} ({})", a.name, a.ids.join(", ")))
                .collect();
            write!(f, "; ambiguous: {}", names.join(", "))?;
        }
        Ok(())
    }
}

impl std::error::Error for FieldResolutionError {}

/// Pick each resolved field's value out of an issue response; an absent field reads as `null`.
pub(crate) fn values_by_name(
    issue: &Value,
    resolved: &[(String, String)],
) -> Result<Map<String, Value>> {
    let Some(fields) = issue.get("fields").and_then(Value::as_object) else {
        bail!("jira issue response has no fields object");
    };
    Ok(resolved
        .iter()
        .map(|(name, id)| (name.clone(), fields.get(id).cloned().unwrap_or(Value::Null)))
        .collect())
}

/// A Jira field value reduced to the text a person sees, from [`field_text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldText {
    /// The field is `null`.
    Empty,
    /// A scalar, or an object reduced to its label.
    One(String),
    /// An array, reduced element by element (possibly empty).
    Many(Vec<String>),
}

impl FieldText {
    /// The text of a single-valued field; `None` for [`FieldText::Empty`] and [`FieldText::Many`].
    pub fn as_one(&self) -> Option<&str> {
        match self {
            Self::One(s) => Some(s),
            Self::Empty | Self::Many(_) => None,
        }
    }
}

/// Reduce a raw field value (as [`crate::JiraClient::get_fields_by_name`] returns it) to text:
///
/// | value                                         | result                       |
/// |-----------------------------------------------|------------------------------|
/// | `null`                                        | `Empty`                      |
/// | string, number, bool                          | `One` (numbers/bools as JSON) |
/// | object with a string `value` (option, select) | `One(value)`                 |
/// | else object with a string `name` (security, status, priority, version, component) | `One(name)` |
/// | else object with a string `displayName` (user) | `One(displayName)`          |
/// | array                                         | `Many`, each element as above |
///
/// A cascading select yields its parent `value` only. Any other shape is an error: an object with
/// none of those labels, or an array element that is `null` or an array.
pub fn field_text(value: &Value) -> Result<FieldText> {
    match value {
        Value::Null => Ok(FieldText::Empty),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(i, item)| match item {
                Value::Null | Value::Array(_) => {
                    bail!("jira field array element {i} has no text: {item}")
                }
                other => scalar_text(other),
            })
            .collect::<Result<_>>()
            .map(FieldText::Many),
        other => scalar_text(other).map(FieldText::One),
    }
}

fn scalar_text(value: &Value) -> Result<String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Object(o) => ["value", "name", "displayName"]
            .iter()
            .find_map(|k| o.get(*k).and_then(Value::as_str))
            .map(str::to_string)
            .with_context(|| {
                format!(
                    "jira field object has no value, name or displayName: {}",
                    crate::client::truncate(&value.to_string(), 200)
                )
            }),
        Value::Null | Value::Array(_) => bail!("jira field value has no text: {value}"),
    }
}

#[cfg(test)]
mod tests {
    use crate::fields::*;
    use serde_json::json;

    fn metadata() -> Value {
        json!([
            {"id": "summary", "name": "Summary", "custom": false},
            {"id": "security", "name": "Security Level", "custom": false},
            {"id": "customfield_10860", "name": "Embargo Status", "custom": true},
            {"id": "customfield_10859", "name": "CVSS Score", "custom": true},
            {"id": "customfield_1", "name": "Team", "custom": true},
            {"id": "customfield_2", "name": "Team", "custom": true},
        ])
    }

    #[test]
    fn field_index_resolves_unique_names() {
        let index = FieldIndex::from_metadata(&metadata()).unwrap();
        assert_eq!(
            index
                .resolve(&["Embargo Status", "Security Level", "Embargo Status"])
                .unwrap(),
            [
                (
                    "Embargo Status".to_string(),
                    "customfield_10860".to_string()
                ),
                ("Security Level".to_string(), "security".to_string()),
            ]
        );
        assert!(index.resolve(&[]).unwrap().is_empty());
    }

    #[test]
    fn field_index_names_every_unresolved_and_ambiguous_name() {
        let index = FieldIndex::from_metadata(&metadata()).unwrap();
        let err = index
            .resolve(&["Embargo Status", "Embargo status", "Team", "Nope", "Team"])
            .unwrap_err();
        assert_eq!(
            err,
            FieldResolutionError {
                unresolved: vec!["Embargo status".into(), "Nope".into()],
                ambiguous: vec![AmbiguousField {
                    name: "Team".into(),
                    ids: vec!["customfield_1".into(), "customfield_2".into()],
                }],
            }
        );
        assert_eq!(
            err.to_string(),
            "jira field names did not resolve to exactly one field; \
             no such field: \"Embargo status\", \"Nope\"; \
             ambiguous: \"Team\" (customfield_1, customfield_2)"
        );
    }

    #[test]
    fn field_resolution_error_survives_anyhow_for_callers_to_inspect() {
        let index = FieldIndex::from_metadata(&metadata()).unwrap();
        let err: anyhow::Error = index.resolve(&["Missing"]).unwrap_err().into();
        let typed = err.downcast_ref::<FieldResolutionError>().unwrap();
        assert_eq!(typed.unresolved, ["Missing"]);
        assert!(typed.ambiguous.is_empty());
    }

    #[test]
    fn field_index_rejects_malformed_metadata() {
        let err = FieldIndex::from_metadata(&json!({"fields": []}))
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("not an array"), "{err}");
        for bad in [
            json!([{"id": "summary"}]),
            json!([{"name": "Summary"}]),
            json!([{"id": 7, "name": "Summary"}]),
            json!(["summary"]),
        ] {
            let err = FieldIndex::from_metadata(&bad).err().unwrap().to_string();
            assert!(err.contains("entry 0 lacks"), "{bad}: {err}");
        }
    }

    #[test]
    fn values_by_name_maps_ids_back_to_names() {
        let resolved = vec![
            (
                "Embargo Status".to_string(),
                "customfield_10860".to_string(),
            ),
            ("Security Level".to_string(), "security".to_string()),
            ("CVSS Score".to_string(), "customfield_10859".to_string()),
        ];
        let issue = json!({
            "key": "P-1",
            "fields": {
                "customfield_10860": {"value": "False", "id": "1"},
                "security": null,
            },
        });
        let got = values_by_name(&issue, &resolved).unwrap();
        assert_eq!(
            Value::Object(got),
            json!({
                "Embargo Status": {"value": "False", "id": "1"},
                "Security Level": null,
                "CVSS Score": null,
            })
        );
    }

    #[test]
    fn values_by_name_rejects_an_issue_without_fields() {
        let resolved = vec![("Summary".to_string(), "summary".to_string())];
        for issue in [json!({"key": "P-1"}), json!({"fields": []}), Value::Null] {
            let err = values_by_name(&issue, &resolved).unwrap_err().to_string();
            assert!(err.contains("no fields object"), "{issue}: {err}");
        }
    }

    #[test]
    fn field_text_reads_an_embargo_status_option() {
        let v = json!({"self": "https://x/rest/api/3/customFieldOption/1", "value": "False", "id": "25812"});
        assert_eq!(field_text(&v).unwrap(), FieldText::One("False".into()));
        assert_eq!(field_text(&v).unwrap().as_one(), Some("False"));
    }

    #[test]
    fn field_text_reads_a_security_level() {
        let v = json!({
            "self": "https://x/rest/api/3/securitylevel/1",
            "id": "11697",
            "description": "",
            "name": "Embargoed Security Issue",
        });
        assert_eq!(
            field_text(&v).unwrap(),
            FieldText::One("Embargoed Security Issue".into())
        );
    }

    #[test]
    fn field_text_reads_named_objects() {
        let status = json!({"name": "In Progress", "id": "3", "statusCategory": {"key": "indeterminate", "name": "In Progress"}});
        assert_eq!(
            field_text(&status).unwrap(),
            FieldText::One("In Progress".into())
        );
        let priority = json!({"name": "Major", "id": "3"});
        assert_eq!(
            field_text(&priority).unwrap(),
            FieldText::One("Major".into())
        );
    }

    #[test]
    fn field_text_prefers_value_over_name() {
        let v = json!({"value": "picked", "name": "other"});
        assert_eq!(field_text(&v).unwrap(), FieldText::One("picked".into()));
    }

    #[test]
    fn field_text_reads_a_user_by_display_name() {
        let v = json!({"accountId": "557058:abc", "displayName": "Ada Lovelace", "active": true});
        assert_eq!(
            field_text(&v).unwrap(),
            FieldText::One("Ada Lovelace".into())
        );
    }

    #[test]
    fn field_text_reads_a_cascading_select_as_its_parent() {
        let v = json!({"value": "Parent", "id": "1", "child": {"value": "Child", "id": "2"}});
        assert_eq!(field_text(&v).unwrap(), FieldText::One("Parent".into()));
    }

    #[test]
    fn field_text_passes_scalars_through() {
        assert_eq!(
            field_text(&json!("2026-10-04T12:00:00.000+0000")).unwrap(),
            FieldText::One("2026-10-04T12:00:00.000+0000".into())
        );
        assert_eq!(
            field_text(&json!(9.8)).unwrap(),
            FieldText::One("9.8".into())
        );
        assert_eq!(field_text(&json!(7)).unwrap(), FieldText::One("7".into()));
        assert_eq!(
            field_text(&json!(true)).unwrap(),
            FieldText::One("true".into())
        );
        assert_eq!(
            field_text(&json!("")).unwrap(),
            FieldText::One(String::new())
        );
    }

    #[test]
    fn field_text_reads_null_as_empty() {
        let t = field_text(&Value::Null).unwrap();
        assert_eq!(t, FieldText::Empty);
        assert_eq!(t.as_one(), None);
    }

    #[test]
    fn field_text_maps_arrays_element_wise() {
        let v = json!([
            {"value": "A", "id": "1"},
            {"name": "Component X", "id": "2"},
            {"displayName": "Ada"},
            "label",
            3,
        ]);
        let t = field_text(&v).unwrap();
        assert_eq!(
            t,
            FieldText::Many(vec![
                "A".into(),
                "Component X".into(),
                "Ada".into(),
                "label".into(),
                "3".into()
            ])
        );
        assert_eq!(t.as_one(), None);
        assert_eq!(field_text(&json!([])).unwrap(), FieldText::Many(vec![]));
    }

    #[test]
    fn field_text_rejects_shapes_without_text() {
        for (v, want) in [
            (json!({"id": "1"}), "no value, name or displayName"),
            (
                json!({"value": 1, "id": "1"}),
                "no value, name or displayName",
            ),
            (
                json!({"value": null, "name": null}),
                "no value, name or displayName",
            ),
            (json!({}), "no value, name or displayName"),
            (json!(["a", null]), "array element 1 has no text"),
            (json!([["a"]]), "array element 0 has no text"),
            (json!([{"id": "1"}]), "no value, name or displayName"),
        ] {
            let err = field_text(&v).unwrap_err().to_string();
            assert!(err.contains(want), "{v}: {err}");
        }
    }
}
