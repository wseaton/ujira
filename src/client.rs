//! A thin JIRA Cloud REST client.
//!
//! Endpoint choices that matter:
//! - search is the bounded `POST /rest/api/3/search/jql` (the old `/rest/api/3/search` was removed,
//!   Atlassian CHANGE-2046),
//! - everything that carries prose (get/create/update issue, comments) uses `/rest/api/2/…` so
//!   descriptions and comment bodies are PLAIN TEXT instead of ADF document JSON. ADF round-trips are
//!   a tax on both ends: the model can't read it cheaply and can't write it correctly.
//! - the `_v3`/`_adf` variants are the deliberate exception: when the caller opts into markdown,
//!   [`crate::adf`] builds the ADF and `/rest/api/3/…` carries it. Reads stay on api/2.

use crate::config::{Access, Config};
use crate::fields::{FieldIndex, values_by_name};
use crate::model::{ChangelogEntry, Comment, parse_items};
use crate::paging::{OffsetPages, SearchPages};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::sync::OnceLock;

/// A connected JIRA Cloud client. Cheap to share behind an `Arc`.
pub struct JiraClient {
    cfg: Config,
    http: reqwest::Client,
    field_index: OnceLock<FieldIndex>,
}

/// The page size asked of offset-paged endpoints (Jira caps changelog and comments at 100).
const OFFSET_PAGE_SIZE: u32 = 100;

/// The compact fields a search row carries (enough to triage; `get_issue` for detail).
const SEARCH_FIELDS: &[&str] = &["summary", "status", "issuetype", "labels", "assignee"];

impl JiraClient {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            http: reqwest::Client::new(),
            field_index: OnceLock::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// `https://site/browse/KEY`, the link a human clicks.
    pub fn browse_url(&self, key: &str) -> String {
        format!("{}/browse/{key}", self.cfg.base)
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.authed(
            self.http
                .request(method, format!("{}{path}", self.cfg.base)),
        )
    }

    /// Like [`Self::req`], but each segment is percent-encoded, for paths that carry caller strings.
    fn req_segments(
        &self,
        method: reqwest::Method,
        segments: &[&str],
    ) -> Result<reqwest::RequestBuilder> {
        let url = segment_url(&self.cfg.base, segments)?;
        Ok(self.authed(self.http.request(method, url)))
    }

    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.basic_auth(&self.cfg.email, Some(&self.cfg.token))
            .header("Accept", "application/json")
    }

    /// The single choke point for authority: every mutating call goes through here first, so an
    /// embedding host that configures `read-comment` cannot be talked into a create by any tool,
    /// present or future.
    fn require(&self, need: Access) -> Result<()> {
        if !self.cfg.access.allows(need) {
            tracing::warn!(configured = %self.cfg.access, required = %need, "refusing a call the access level does not allow");
            bail!(
                "this ujira client is configured {} — {need} is required for that call",
                self.cfg.access
            );
        }
        Ok(())
    }

    /// JQL search. Returns the raw `issues` array (rendering is the caller's job).
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn search(&self, jql: &str, limit: u32) -> Result<Vec<Value>> {
        let body = json!({"jql": jql, "maxResults": limit.clamp(1, 100), "fields": SEARCH_FIELDS});
        let v = self
            .send(
                self.req(reqwest::Method::POST, "/rest/api/3/search/jql")
                    .json(&body),
                "search",
            )
            .await?;
        Ok(v.get("issues")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// JQL search over every page, with the caller's `fields`. Returns up to `max_results` raw
    /// issue objects. Fails when Jira breaks the paging contract (a missing or repeated
    /// `nextPageToken`, or an `issues` value that is not an array of objects).
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn search_all(
        &self,
        jql: &str,
        fields: &[&str],
        max_results: u32,
    ) -> Result<Vec<Value>> {
        let mut pages = SearchPages::new(max_results);
        while let Some(page) = pages.next_request() {
            let mut body = json!({"jql": jql, "fields": fields, "maxResults": page.size});
            if let Some(token) = page.token {
                body["nextPageToken"] = Value::String(token);
            }
            let v = self
                .send(
                    self.req(reqwest::Method::POST, "/rest/api/3/search/jql")
                        .json(&body),
                    "search_all",
                )
                .await?;
            pages.accept(v)?;
        }
        Ok(pages.into_issues())
    }

    /// One issue, plus its links and (optionally) its comments. `/rest/api/2` for plain-text prose.
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn get_issue(&self, key: &str, with_comments: bool) -> Result<Value> {
        // Comments cost a page of prose each, so they're opt-in: `*all,-comment` is api/2's
        // "everything except" selector.
        let path = if with_comments {
            format!("/rest/api/2/issue/{key}")
        } else {
            format!("/rest/api/2/issue/{key}?fields=*all,-comment")
        };
        self.send(self.req(reqwest::Method::GET, &path), "get_issue")
            .await
    }

    /// Every changelog history of an issue, oldest first, across all pages.
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn changelog(&self, key: &str) -> Result<Vec<ChangelogEntry>> {
        let items = self
            .offset_paged(
                &["rest", "api", "3", "issue", key, "changelog"],
                &[],
                "values",
                "changelog",
            )
            .await?;
        parse_items("changelog", items)
    }

    /// The newest `limit` comments on an issue.
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn get_comments(&self, key: &str, limit: u32) -> Result<Vec<Value>> {
        let path = format!(
            "/rest/api/2/issue/{key}/comment?orderBy=-created&maxResults={}",
            limit.clamp(1, 100)
        );
        let v = self
            .send(self.req(reqwest::Method::GET, &path), "get_comments")
            .await?;
        Ok(v.get("comments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Every comment on an issue, oldest first, across all pages. Read through api/3, so bodies
    /// are ADF; [`Self::get_comments`] is the plain-text, newest-N read.
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn get_all_comments(&self, key: &str) -> Result<Vec<Comment>> {
        let items = self
            .offset_paged(
                &["rest", "api", "3", "issue", key, "comment"],
                &[("orderBy", "created")],
                "comments",
                "get_all_comments",
            )
            .await?;
        parse_items("comment", items)
    }

    /// Replace an existing comment's body with an ADF document (api/v3).
    #[tracing::instrument(level = "debug", skip(self, body_adf), err)]
    pub async fn update_comment_adf(
        &self,
        key: &str,
        comment_id: &str,
        body_adf: Value,
    ) -> Result<()> {
        self.require(Access::ReadWrite)?;
        self.send(
            self.req_segments(
                reqwest::Method::PUT,
                &["rest", "api", "3", "issue", key, "comment", comment_id],
            )?
            .json(&json!({ "body": body_adf })),
            "update_comment_adf",
        )
        .await?;
        Ok(())
    }

    /// Post a comment with an ADF body (api/v3). Returns the new comment id.
    #[tracing::instrument(level = "debug", skip(self, body_adf), err)]
    pub async fn add_comment_adf(&self, key: &str, body_adf: Value) -> Result<String> {
        self.require(Access::ReadComment)?;
        let v = self
            .send(
                self.req(
                    reqwest::Method::POST,
                    &format!("/rest/api/3/issue/{key}/comment"),
                )
                .json(&json!({ "body": body_adf })),
                "add_comment_adf",
            )
            .await?;
        Ok(id_of(&v))
    }

    /// Post a plain-text comment. Returns the new comment id.
    #[tracing::instrument(level = "debug", skip(self, body), err)]
    pub async fn add_comment(&self, key: &str, body: &str) -> Result<String> {
        self.require(Access::ReadComment)?;
        let v = self
            .send(
                self.req(
                    reqwest::Method::POST,
                    &format!("/rest/api/2/issue/{key}/comment"),
                )
                .json(&json!({ "body": body })),
                "add_comment",
            )
            .await?;
        Ok(id_of(&v))
    }

    /// Create an issue from an already-assembled `fields` object. Returns the new key.
    #[tracing::instrument(level = "debug", skip(self, fields), err)]
    pub async fn create_issue(&self, fields: Map<String, Value>) -> Result<String> {
        self.require(Access::ReadWrite)?;
        let v = self
            .send(
                self.req(reqwest::Method::POST, "/rest/api/2/issue")
                    .json(&json!({ "fields": Value::Object(fields) })),
                "create_issue",
            )
            .await?;
        Ok(v.get("key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Create an issue via api/v3 (accepts ADF in description). Returns the new key.
    #[tracing::instrument(level = "debug", skip(self, fields), err)]
    pub async fn create_issue_v3(&self, fields: Map<String, Value>) -> Result<String> {
        self.require(Access::ReadWrite)?;
        let v = self
            .send(
                self.req(reqwest::Method::POST, "/rest/api/3/issue")
                    .json(&json!({ "fields": Value::Object(fields) })),
                "create_issue_v3",
            )
            .await?;
        Ok(v.get("key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Edit an issue's fields in place. A 204 carries no body, so there's nothing to return.
    #[tracing::instrument(level = "debug", skip(self, fields), err)]
    pub async fn update_issue(&self, key: &str, fields: Map<String, Value>) -> Result<()> {
        self.require(Access::ReadWrite)?;
        self.send(
            self.req(reqwest::Method::PUT, &format!("/rest/api/2/issue/{key}"))
                .json(&json!({ "fields": Value::Object(fields) })),
            "update_issue",
        )
        .await?;
        Ok(())
    }

    /// Edit an issue's fields via api/v3 (accepts ADF values for description/comment bodies).
    #[tracing::instrument(level = "debug", skip(self, fields), err)]
    pub async fn update_issue_v3(&self, key: &str, fields: Map<String, Value>) -> Result<()> {
        self.require(Access::ReadWrite)?;
        self.send(
            self.req(reqwest::Method::PUT, &format!("/rest/api/3/issue/{key}"))
                .json(&json!({ "fields": Value::Object(fields) })),
            "update_issue_v3",
        )
        .await?;
        Ok(())
    }

    /// The transitions available from the issue's current status, as `(id, name)`.
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn transitions(&self, key: &str) -> Result<Vec<(String, String)>> {
        let v = self
            .send(
                self.req(
                    reqwest::Method::GET,
                    &format!("/rest/api/2/issue/{key}/transitions"),
                ),
                "transitions",
            )
            .await?;
        Ok(v.get("transitions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|t| (id_of(t), str_at(t, "/name").to_string()))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Drive a transition by id (resolve the name first with [`Self::transitions`]).
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn transition(&self, key: &str, id: &str) -> Result<()> {
        self.require(Access::ReadWrite)?;
        self.send(
            self.req(
                reqwest::Method::POST,
                &format!("/rest/api/2/issue/{key}/transitions"),
            )
            .json(&json!({"transition": {"id": id}})),
            "transition",
        )
        .await?;
        Ok(())
    }

    /// Link `source` to `target` using the link type's outward description.
    ///
    /// Jira's wire-format names are counterintuitive: the source belongs in `inwardIssue`, while
    /// the target belongs in `outwardIssue`. For example, `Depend, A, B` means `A depends on B`.
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn link(&self, link_type: &str, source: &str, target: &str) -> Result<()> {
        self.require(Access::ReadWrite)?;
        self.send(
            self.req(reqwest::Method::POST, "/rest/api/2/issueLink")
                .json(&issue_link_body(link_type, source, target)),
            "link",
        )
        .await?;
        Ok(())
    }

    /// The site's link type names (the only legal values for `jira_link_issues`).
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn link_types(&self) -> Result<Vec<Value>> {
        let v = self
            .send(
                self.req(reqwest::Method::GET, "/rest/api/2/issueLinkType"),
                "link_types",
            )
            .await?;
        Ok(v.get("issueLinkTypes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Incremental label add: appends without replacing the existing set.
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn add_labels(&self, key: &str, labels: &[String]) -> Result<()> {
        self.require(Access::ReadWrite)?;
        let ops: Vec<Value> = labels.iter().map(|l| json!({"add": l})).collect();
        self.send(
            self.req(reqwest::Method::PUT, &format!("/rest/api/2/issue/{key}"))
                .json(&json!({"update": {"labels": ops}})),
            "add_labels",
        )
        .await?;
        Ok(())
    }

    /// Incremental label remove: drops specific labels without touching the rest.
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn remove_labels(&self, key: &str, labels: &[String]) -> Result<()> {
        self.require(Access::ReadWrite)?;
        let ops: Vec<Value> = labels.iter().map(|l| json!({"remove": l})).collect();
        self.send(
            self.req(reqwest::Method::PUT, &format!("/rest/api/2/issue/{key}"))
                .json(&json!({"update": {"labels": ops}})),
            "remove_labels",
        )
        .await?;
        Ok(())
    }

    /// Upload a file attachment. Returns the attachment JSON array from JIRA.
    #[tracing::instrument(level = "debug", skip(self, data), err)]
    pub async fn add_attachment(&self, key: &str, filename: &str, data: Vec<u8>) -> Result<Value> {
        self.require(Access::ReadWrite)?;
        let part = reqwest::multipart::Part::bytes(data)
            .file_name(filename.to_string())
            .mime_str(&mime_from_filename(filename))
            .context("setting attachment MIME type")?;
        self.send(
            self.req(
                reqwest::Method::POST,
                &format!("/rest/api/2/issue/{key}/attachments"),
            )
            // Without this header JIRA rejects the multipart POST as potential XSRF.
            .header("X-Atlassian-Token", "no-check")
            .multipart(reqwest::multipart::Form::new().part("file", part)),
            "add_attachment",
        )
        .await
    }

    /// Delete an attachment by id.
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn delete_attachment(&self, id: &str) -> Result<()> {
        self.require(Access::ReadWrite)?;
        self.send(
            self.req(
                reqwest::Method::DELETE,
                &format!("/rest/api/2/attachment/{id}"),
            ),
            "delete_attachment",
        )
        .await?;
        Ok(())
    }

    /// Every field on the site (id + name + custom flag) — the lookup behind `jira_fields`, which is
    /// how you find the `customfield_NNNNN` to pass to create/update.
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn fields(&self) -> Result<Vec<Value>> {
        let v = self
            .send(
                self.req(reqwest::Method::GET, "/rest/api/2/field"),
                "fields",
            )
            .await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// An issue entity property's value, or `None` when Jira answers 404 (no such property, or no
    /// issue the account can see).
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn get_issue_property(&self, key: &str, property_key: &str) -> Result<Option<Value>> {
        let req = self.req_segments(
            reqwest::Method::GET,
            &["rest", "api", "3", "issue", key, "properties", property_key],
        )?;
        self.send_or_missing(req, "get_issue_property")
            .await?
            .map(property_value)
            .transpose()
    }

    /// Create or replace an issue entity property. `value` is stored verbatim as the property.
    #[tracing::instrument(level = "debug", skip(self, value), err)]
    pub async fn set_issue_property(
        &self,
        key: &str,
        property_key: &str,
        value: &Value,
    ) -> Result<()> {
        self.require(Access::ReadWrite)?;
        let req = self.req_segments(
            reqwest::Method::PUT,
            &["rest", "api", "3", "issue", key, "properties", property_key],
        )?;
        self.send(req.json(value), "set_issue_property").await?;
        Ok(())
    }

    /// Read fields of one issue by display name (e.g. `"Embargo Status"`), as name -> raw field
    /// value (`null` when the issue has no value). Names resolve against `/rest/api/3/field`,
    /// fetched once per client. A name that matches no field, or more than one, fails the whole
    /// call with a [`crate::fields::FieldResolutionError`] listing every such name.
    /// [`crate::fields::field_text`] reduces each value to its display text.
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn get_fields_by_name(
        &self,
        key: &str,
        names: &[&str],
    ) -> Result<Map<String, Value>> {
        if names.is_empty() {
            return Ok(Map::new());
        }
        let resolved = self.field_index().await?.resolve(names)?;
        let ids: Vec<&str> = resolved.iter().map(|(_, id)| id.as_str()).collect();
        let issue = self
            .send(
                self.req_segments(reqwest::Method::GET, &["rest", "api", "3", "issue", key])?
                    .query(&[("fields", ids.join(","))]),
                "get_fields_by_name",
            )
            .await?;
        values_by_name(&issue, &resolved)
    }

    async fn field_index(&self) -> Result<&FieldIndex> {
        if let Some(index) = self.field_index.get() {
            return Ok(index);
        }
        let v = self
            .send(
                self.req(reqwest::Method::GET, "/rest/api/3/field"),
                "field_index",
            )
            .await?;
        let index = FieldIndex::from_metadata(&v)?;
        Ok(self.field_index.get_or_init(|| index))
    }

    /// Users matching an email, username, or display name. Raw user objects.
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn user_search(&self, query: &str, limit: u32) -> Result<Vec<Value>> {
        let v = self
            .send(
                self.req(reqwest::Method::GET, "/rest/api/2/user/search")
                    .query(&[
                        ("query", query),
                        ("maxResults", &limit.clamp(1, 1000).to_string()),
                    ]),
                "user_search",
            )
            .await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// A project's components. Raw component objects.
    #[tracing::instrument(level = "debug", skip(self), err)]
    pub async fn components(&self, project: &str) -> Result<Vec<Value>> {
        let v = self
            .send(
                self.req(
                    reqwest::Method::GET,
                    &format!("/rest/api/2/project/{project}/components"),
                ),
                "components",
            )
            .await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// GET every page of a `startAt`/`maxResults` endpoint and return the raw items.
    async fn offset_paged(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
        items_key: &'static str,
        what: &str,
    ) -> Result<Vec<Value>> {
        let mut pages = OffsetPages::new(items_key);
        while let Some(start) = pages.next_start() {
            let page = self
                .send(
                    self.req_segments(reqwest::Method::GET, segments)?
                        .query(query)
                        .query(&[
                            ("startAt", start.to_string()),
                            ("maxResults", OFFSET_PAGE_SIZE.to_string()),
                        ]),
                    what,
                )
                .await?;
            pages.accept(page)?;
        }
        Ok(pages.into_items())
    }

    /// Send, then map a non-2xx to an error carrying the (truncated) body. A 204/empty body becomes
    /// `null` rather than a parse error, which is what the write endpoints return.
    async fn send(&self, req: reqwest::RequestBuilder, what: &str) -> Result<Value> {
        let (status, text) = self.exchange(req, what).await?;
        decode(what, status, &text)
    }

    /// [`Self::send`], except a 404 is `None` instead of an error.
    async fn send_or_missing(
        &self,
        req: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<Option<Value>> {
        let (status, text) = self.exchange(req, what).await?;
        missing_as_none(what, status, &text)
    }

    async fn exchange(
        &self,
        req: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<(reqwest::StatusCode, String)> {
        let req = req
            .build()
            .with_context(|| format!("building the jira {what} request"))?;
        tracing::debug!(what, method = %req.method(), url = %req.url(), "jira request");
        let resp = self
            .http
            .execute(req)
            .await
            .with_context(|| format!("jira {what} request"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Ok((status, text))
    }
}

/// A response's status and body as a JSON value: non-2xx is an error, an empty body is `null`.
fn decode(what: &str, status: reqwest::StatusCode, text: &str) -> Result<Value> {
    if status.is_success() {
        tracing::debug!(what, %status, bytes = text.len(), "jira response");
    } else {
        tracing::warn!(what, %status, body = %truncate(text, 400), "jira request failed");
        bail!("jira {what} failed ({status}): {}", truncate(text, 400));
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(text)
        .with_context(|| format!("parsing jira {what} response: {}", truncate(text, 200)))
}

/// [`decode`], with a 404 read as absence.
fn missing_as_none(what: &str, status: reqwest::StatusCode, text: &str) -> Result<Option<Value>> {
    if status == reqwest::StatusCode::NOT_FOUND {
        tracing::debug!(what, %status, "jira resource absent");
        return Ok(None);
    }
    decode(what, status, text).map(Some)
}

/// `base` with `segments` appended, each percent-encoded as a single path segment.
fn segment_url(base: &str, segments: &[&str]) -> Result<reqwest::Url> {
    let mut url =
        reqwest::Url::parse(base).with_context(|| format!("parsing the jira base url {base}"))?;
    url.path_segments_mut()
        .map_err(|()| anyhow!("the jira base url {base} cannot carry a path"))?
        .pop_if_empty()
        .extend(segments);
    Ok(url)
}

/// The `value` of an issue property response (`{"key": .., "value": ..}`).
fn property_value(body: Value) -> Result<Value> {
    match body {
        Value::Object(mut o) => o
            .remove("value")
            .context("jira issue property response has no value"),
        other => bail!(
            "jira issue property response is not an object: {}",
            truncate(&other.to_string(), 200)
        ),
    }
}

fn id_of(v: &Value) -> String {
    v.get("id")
        .map(|i| match i {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

fn str_at<'a>(v: &'a Value, ptr: &str) -> &'a str {
    v.pointer(ptr).and_then(Value::as_str).unwrap_or_default()
}

fn issue_link_body(link_type: &str, source: &str, target: &str) -> Value {
    json!({
        "type": {"name": link_type},
        "inwardIssue": {"key": source},
        "outwardIssue": {"key": target},
    })
}

/// Best-effort MIME type from a filename extension. Falls back to application/octet-stream.
fn mime_from_filename(name: &str) -> String {
    match name
        .rsplit('.')
        .next()
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("md" | "markdown") => "text/markdown".into(),
        Some("txt") => "text/plain".into(),
        Some("json") => "application/json".into(),
        Some("pdf") => "application/pdf".into(),
        Some("png") => "image/png".into(),
        Some("jpg" | "jpeg") => "image/jpeg".into(),
        Some("gif") => "image/gif".into(),
        Some("svg") => "image/svg+xml".into(),
        Some("html" | "htm") => "text/html".into(),
        Some("xml") => "application/xml".into(),
        Some("csv") => "text/csv".into(),
        Some("zip") => "application/zip".into(),
        Some("gz" | "tgz") => "application/gzip".into(),
        Some("yaml" | "yml") => "text/yaml".into(),
        _ => "application/octet-stream".into(),
    }
}

/// Char-safe truncation (byte slicing would panic mid-UTF-8 on a Jira description).
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}… [truncated]")
}

#[cfg(test)]
mod tests {
    use crate::client::*;

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("héllo wörld", 5), "héllo… [truncated]");
    }

    #[test]
    fn id_of_handles_string_and_number() {
        assert_eq!(id_of(&json!({"id": "10001"})), "10001");
        assert_eq!(id_of(&json!({"id": 10001})), "10001");
        assert_eq!(id_of(&json!({})), "");
    }

    #[test]
    fn link_source_maps_to_jiras_inward_issue_field() {
        let body = issue_link_body("Depend", "PROJ-1", "PROJ-2");
        assert_eq!(body["inwardIssue"]["key"], "PROJ-1");
        assert_eq!(body["outwardIssue"]["key"], "PROJ-2");
    }

    #[test]
    fn decode_maps_status_and_body() {
        use reqwest::StatusCode;
        assert_eq!(
            decode("x", StatusCode::OK, r#"{"a":1}"#).unwrap(),
            json!({"a": 1})
        );
        assert_eq!(
            decode("x", StatusCode::NO_CONTENT, "").unwrap(),
            Value::Null
        );
        assert_eq!(
            decode("x", StatusCode::CREATED, "  \n").unwrap(),
            Value::Null
        );
        let err = decode("x", StatusCode::BAD_REQUEST, "nope")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("jira x failed (400 Bad Request): nope"),
            "{err}"
        );
        let err = decode("x", StatusCode::NOT_FOUND, "gone")
            .unwrap_err()
            .to_string();
        assert!(err.contains("404"), "{err}");
        assert!(decode("x", StatusCode::OK, "{not json").is_err());
    }

    #[test]
    fn missing_as_none_reads_only_404_as_absent() {
        use reqwest::StatusCode;
        assert_eq!(
            missing_as_none("x", StatusCode::NOT_FOUND, r#"{"errorMessages":["no"]}"#).unwrap(),
            None
        );
        assert_eq!(
            missing_as_none("x", StatusCode::OK, r#"{"value":{}}"#).unwrap(),
            Some(json!({"value": {}}))
        );
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::GONE,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            assert!(missing_as_none("x", status, "").is_err(), "{status}");
        }
    }

    #[test]
    fn property_value_returns_the_stored_value() {
        assert_eq!(
            property_value(json!({"key": "verdict", "value": {"ok": true}})).unwrap(),
            json!({"ok": true})
        );
        assert_eq!(
            property_value(json!({"key": "n", "value": 3})).unwrap(),
            json!(3)
        );
        assert_eq!(
            property_value(json!({"key": "n", "value": null})).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn property_value_rejects_a_malformed_body() {
        let err = property_value(json!({"key": "verdict"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("has no value"), "{err}");
        let err = property_value(Value::Null).unwrap_err().to_string();
        assert!(err.contains("not an object"), "{err}");
    }

    #[test]
    fn segment_url_encodes_each_segment() {
        let url = segment_url(
            "https://site.atlassian.net",
            &[
                "rest",
                "api",
                "3",
                "issue",
                "P-1",
                "properties",
                "a/b c?d#e",
            ],
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://site.atlassian.net/rest/api/3/issue/P-1/properties/a%2Fb%20c%3Fd%23e"
        );
    }

    #[test]
    fn segment_url_keeps_a_base_path() {
        let url = segment_url("https://host/jira", &["rest", "api", "3", "field"]).unwrap();
        assert_eq!(url.as_str(), "https://host/jira/rest/api/3/field");
    }

    #[test]
    fn segment_url_rejects_a_bad_base() {
        assert!(segment_url("not a url", &["rest"]).is_err());
        assert!(segment_url("mailto:me@example.com", &["rest"]).is_err());
    }
}
