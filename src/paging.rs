//! Paging state machines for Jira's list endpoints, kept free of HTTP so they are unit-testable.

use anyhow::{Result, bail};
use serde_json::Value;
use std::collections::HashSet;

/// The largest page [`crate::JiraClient::search_all`] asks for.
const SEARCH_PAGE_SIZE: u32 = 50;

/// The next `search/jql` request [`SearchPages`] wants made.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PageRequest {
    pub(crate) size: u32,
    pub(crate) token: Option<String>,
}

/// Token-paging state for `search/jql`: which page to ask for next, and what came back so far.
pub(crate) struct SearchPages {
    max: usize,
    issues: Vec<Value>,
    token: Option<String>,
    seen: HashSet<String>,
    done: bool,
}

impl SearchPages {
    pub(crate) fn new(max_results: u32) -> Self {
        Self {
            max: max_results as usize,
            issues: Vec::new(),
            token: None,
            seen: HashSet::new(),
            done: false,
        }
    }

    /// The next page to fetch, or `None` once the last page or `max_results` is reached.
    pub(crate) fn next_request(&self) -> Option<PageRequest> {
        let remaining = self.max.saturating_sub(self.issues.len());
        if self.done || remaining == 0 {
            return None;
        }
        let size = u32::try_from(remaining).map_or(SEARCH_PAGE_SIZE, |r| r.min(SEARCH_PAGE_SIZE));
        Some(PageRequest {
            size,
            token: self.token.clone(),
        })
    }

    /// Take one `search/jql` response body.
    pub(crate) fn accept(&mut self, page: Value) -> Result<()> {
        let Some(issues) = page.get("issues").and_then(Value::as_array) else {
            bail!("jira search returned no issues array");
        };
        if let Some(i) = issues.iter().position(|issue| !issue.is_object()) {
            bail!("jira search returned a non-object issue at index {i}");
        }
        let room = self.max.saturating_sub(self.issues.len());
        self.issues.extend(issues.iter().take(room).cloned());
        if page.get("isLast").and_then(Value::as_bool) == Some(true)
            || self.issues.len() >= self.max
        {
            self.done = true;
            return Ok(());
        }
        let token = match page.get("nextPageToken").and_then(Value::as_str) {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => bail!("jira search omitted nextPageToken on a page that is not the last"),
        };
        if !self.seen.insert(token.clone()) {
            bail!("jira search repeated nextPageToken {token}");
        }
        self.token = Some(token);
        Ok(())
    }

    pub(crate) fn into_issues(self) -> Vec<Value> {
        self.issues
    }
}

#[cfg(test)]
mod tests {
    use crate::paging::*;
    use serde_json::json;

    fn issues(range: std::ops::Range<u32>) -> Vec<Value> {
        range.map(|n| json!({"key": format!("P-{n}")})).collect()
    }

    fn keys(issues: &[Value]) -> Vec<&str> {
        issues
            .iter()
            .map(|i| {
                i.pointer("/key")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            })
            .collect()
    }

    #[test]
    fn search_pages_walks_tokens_until_is_last() {
        let mut p = SearchPages::new(500);
        assert_eq!(
            p.next_request(),
            Some(PageRequest {
                size: 50,
                token: None
            })
        );
        p.accept(json!({"issues": issues(0..50), "nextPageToken": "t1", "isLast": false}))
            .unwrap();
        assert_eq!(
            p.next_request(),
            Some(PageRequest {
                size: 50,
                token: Some("t1".into())
            })
        );
        p.accept(json!({"issues": issues(50..60), "isLast": true}))
            .unwrap();
        assert_eq!(p.next_request(), None);
        let all = p.into_issues();
        assert_eq!(all.len(), 60);
        assert_eq!(keys(&all)[59], "P-59");
    }

    #[test]
    fn search_pages_shrinks_the_last_page_to_the_remaining_budget() {
        let mut p = SearchPages::new(70);
        assert_eq!(p.next_request().map(|r| r.size), Some(50));
        p.accept(json!({"issues": issues(0..50), "nextPageToken": "t1"}))
            .unwrap();
        assert_eq!(p.next_request().map(|r| r.size), Some(20));
        p.accept(json!({"issues": issues(50..70), "nextPageToken": "t2"}))
            .unwrap();
        assert_eq!(p.next_request(), None);
        assert_eq!(p.into_issues().len(), 70);
    }

    #[test]
    fn search_pages_stops_at_max_without_needing_a_token() {
        let mut p = SearchPages::new(3);
        assert_eq!(p.next_request().map(|r| r.size), Some(3));
        p.accept(json!({"issues": issues(0..3), "isLast": false}))
            .unwrap();
        assert_eq!(p.next_request(), None);
        assert_eq!(keys(&p.into_issues()), ["P-0", "P-1", "P-2"]);
    }

    #[test]
    fn search_pages_never_returns_more_than_max() {
        let mut p = SearchPages::new(2);
        p.accept(json!({"issues": issues(0..5), "nextPageToken": "t1"}))
            .unwrap();
        assert_eq!(p.next_request(), None);
        assert_eq!(keys(&p.into_issues()), ["P-0", "P-1"]);
    }

    #[test]
    fn search_pages_with_zero_max_asks_for_nothing() {
        let p = SearchPages::new(0);
        assert_eq!(p.next_request(), None);
        assert!(p.into_issues().is_empty());
    }

    #[test]
    fn search_pages_accepts_an_empty_last_page() {
        let mut p = SearchPages::new(10);
        p.accept(json!({"issues": [], "isLast": true})).unwrap();
        assert_eq!(p.next_request(), None);
        assert!(p.into_issues().is_empty());
    }

    #[test]
    fn search_pages_fails_when_a_non_last_page_omits_its_token() {
        for page in [
            json!({"issues": issues(0..50), "isLast": false}),
            json!({"issues": issues(0..50)}),
            json!({"issues": issues(0..50), "nextPageToken": ""}),
            json!({"issues": issues(0..50), "nextPageToken": null}),
            json!({"issues": issues(0..50), "nextPageToken": 7}),
            json!({"issues": issues(0..50), "isLast": "true", "nextPageToken": null}),
        ] {
            let mut p = SearchPages::new(500);
            let err = p.accept(page.clone()).unwrap_err().to_string();
            assert!(err.contains("omitted nextPageToken"), "{page}: {err}");
        }
    }

    #[test]
    fn search_pages_fails_when_a_token_repeats() {
        let mut p = SearchPages::new(500);
        p.accept(json!({"issues": issues(0..50), "nextPageToken": "t1"}))
            .unwrap();
        p.accept(json!({"issues": issues(50..100), "nextPageToken": "t2"}))
            .unwrap();
        let err = p
            .accept(json!({"issues": issues(100..150), "nextPageToken": "t1"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("repeated nextPageToken t1"), "{err}");
    }

    #[test]
    fn search_pages_fails_when_issues_is_not_an_array() {
        for page in [
            json!({"isLast": true}),
            json!({"issues": null, "isLast": true}),
            json!({"issues": {"key": "P-1"}, "isLast": true}),
            json!([]),
            Value::Null,
        ] {
            let mut p = SearchPages::new(10);
            let err = p.accept(page.clone()).unwrap_err().to_string();
            assert!(err.contains("no issues array"), "{page}: {err}");
        }
    }

    #[test]
    fn search_pages_fails_on_a_non_object_issue() {
        let mut p = SearchPages::new(10);
        let err = p
            .accept(json!({"issues": [{"key": "P-1"}, "P-2"], "isLast": true}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-object issue at index 1"), "{err}");
    }
}
