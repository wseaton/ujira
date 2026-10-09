//! The MCP tool surface over streamable HTTP, for a host that spawns `ujira` as a network server and
//! hands each caller a bearer token.
//!
//! Every request must carry `Authorization: Bearer <token>` matching a line of the tokens file, which
//! holds `<token> <sandbox> [<workdir>]` lines and is re-read on every request. The server is
//! stateless: each POST to [`MCP_PATH`] is answered with one JSON body and no session id.

use crate::server::JiraMcp;
use anyhow::{Context, Result, bail};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use std::path::PathBuf;
use std::sync::Arc;

pub const MCP_PATH: &str = "/mcp";

const SANDBOX_HOSTS: [&str; 2] = ["host.containers.internal", "host.openshell.internal"];

/// The file of `<token> <sandbox> [<workdir>]` lines a request's bearer is checked against.
#[derive(Debug, Clone)]
pub struct TokensFile(PathBuf);

impl TokensFile {
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }

    async fn admit(&self, headers: &HeaderMap) -> Result<String, StatusCode> {
        let token = bearer(headers).ok_or(StatusCode::UNAUTHORIZED)?;
        let lines = tokio::fs::read_to_string(&self.0)
            .await
            .context("reading the tokens file")
            .and_then(|text| parse_tokens(&text))
            .map_err(|e| {
                tracing::error!("{}: {e:#}", self.0.display());
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        lines
            .into_iter()
            .fold(None, |found, line| {
                if constant_time_eq(line.token.as_bytes(), token.as_bytes()) {
                    Some(line.sandbox)
                } else {
                    found
                }
            })
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct TokenLine {
    token: String,
    sandbox: String,
}

fn parse_tokens(text: &str) -> Result<Vec<TokenLine>> {
    let mut lines = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let (token, sandbox) = match raw.split_whitespace().collect::<Vec<_>>()[..] {
            [] => continue,
            [token, sandbox] => (token, sandbox),
            [token, sandbox, workdir] if workdir.starts_with('/') => (token, sandbox),
            _ => bail!(
                "tokens file line {} is not `<token> <sandbox> [<workdir>]`",
                i + 1
            ),
        };
        lines.push(TokenLine {
            token: token.to_string(),
            sandbox: sandbox.to_string(),
        });
    }
    Ok(lines)
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|t| !t.is_empty())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The `/mcp` endpoint behind the bearer check. `Host` must be loopback, a sandbox gateway name, or
/// one of `extra_hosts`.
pub fn router(server: JiraMcp, tokens: TokensFile, extra_hosts: &[String]) -> axum::Router {
    let mut hosts = StreamableHttpServerConfig::default().allowed_hosts;
    hosts.extend(SANDBOX_HOSTS.map(String::from));
    hosts.extend(extra_hosts.iter().map(|h| h.trim().to_string()));
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_sse_retry(None)
        .with_allowed_hosts(hosts);
    let service: StreamableHttpService<JiraMcp, NeverSessionManager> = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    axum::Router::new()
        .nest_service(MCP_PATH, service)
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(tokens),
            authenticate,
        ))
}

async fn authenticate(State(tokens): State<Arc<TokensFile>>, req: Request, next: Next) -> Response {
    match tokens.admit(req.headers()).await {
        Ok(sandbox) => {
            tracing::debug!(%sandbox, "authenticated MCP request");
            next.run(req).await
        }
        Err(status) => status.into_response(),
    }
}

/// Serve [`router`] on `bind` (`host:port`) until the process exits.
pub async fn serve(
    server: JiraMcp,
    bind: &str,
    tokens: TokensFile,
    extra_hosts: &[String],
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!("serving MCP on http://{bind}{MCP_PATH}");
    axum::serve(listener, router(server, tokens, extra_hosts))
        .await
        .context("serving MCP over HTTP")
}

#[cfg(test)]
mod tests {
    use crate::client::JiraClient;
    use crate::config::{Access, Config, TokenSource};
    use crate::http::{MCP_PATH, TokenLine, TokensFile, parse_tokens, router};
    use crate::server::{JiraMcp, ToolSelection};
    use serde_json::{Value, json};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn jira() -> JiraMcp {
        JiraMcp::new(Arc::new(JiraClient::new(Config {
            base: "http://127.0.0.1:9".into(),
            email: "me@x".into(),
            token: "t".into(),
            token_source: TokenSource::Inline,
            access: Access::ReadOnly,
            custom_fields: Vec::new(),
        })))
    }

    fn tokens(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("tokens");
        std::fs::write(&path, body).unwrap();
        path
    }

    async fn serve(server: JiraMcp, file: &Path) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let app = router(
            server,
            TokensFile::new(file.to_path_buf()),
            &["jira.svc".to_string()],
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }

    async fn post(addr: &str, host: &str, token: Option<&str>, body: &Value) -> (u16, String) {
        let body = body.to_string();
        let auth = token
            .map(|t| format!("Authorization: Bearer {t}\r\n"))
            .unwrap_or_default();
        let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "POST {MCP_PATH} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\n{auth}Content-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        sock.write_all(req.as_bytes()).await.unwrap();
        let mut out = String::new();
        sock.read_to_string(&mut out).await.unwrap();
        let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, out)
    }

    fn json_body(reply: &str) -> Value {
        let (_, body) = reply.split_once("\r\n\r\n").unwrap();
        serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {reply}"))
    }

    fn list() -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})
    }

    fn call(name: &str, arguments: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": name, "arguments": arguments}})
    }

    fn listed_names(reply: &str) -> Vec<String> {
        let mut names: Vec<String> = json_body(reply)["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn token_lines_take_an_optional_absolute_workdir() {
        let lines = parse_tokens("tok-a sb-a /sandbox/task-a\n\n  tok-b  sb-b \n").unwrap();
        assert_eq!(
            lines,
            [
                TokenLine {
                    token: "tok-a".to_string(),
                    sandbox: "sb-a".to_string(),
                },
                TokenLine {
                    token: "tok-b".to_string(),
                    sandbox: "sb-b".to_string(),
                },
            ]
        );
        assert_eq!(parse_tokens("").unwrap(), []);
        for (bad, line) in [
            ("tok-a\n", 1),
            ("tok-a sb-a /w extra\n", 1),
            ("tok-a sb-a relative\n", 1),
            ("tok-a sb-a\ntok-b\n", 2),
        ] {
            let err = parse_tokens(bad).unwrap_err().to_string();
            assert!(err.contains(&format!("line {line} ")), "{bad:?}: {err}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn requests_need_a_bearer_listed_in_the_tokens_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = tokens(dir.path(), "tok-a sb-a /sandbox/a\ntok-b sb-b\n");
        let addr = serve(jira(), &file).await;
        let (status, reply) = post(&addr, "127.0.0.1", Some("tok-a"), &list()).await;
        assert_eq!(status, 200, "{reply}");
        let lower = reply.to_ascii_lowercase();
        assert!(lower.contains("content-type: application/json"), "{reply}");
        assert!(!lower.contains("mcp-session-id"), "{reply}");
        assert_eq!(listed_names(&reply).len(), 16, "{reply}");
        assert_eq!(
            post(&addr, "127.0.0.1", Some("tok-b"), &list()).await.0,
            200
        );
        assert_eq!(post(&addr, "127.0.0.1", None, &list()).await.0, 401);
        for wrong in ["nope", "tok-", "tok-a2", "sb-a", "/sandbox/a"] {
            assert_eq!(
                post(&addr, "127.0.0.1", Some(wrong), &list()).await.0,
                401,
                "{wrong}"
            );
        }
        for host in [
            "host.openshell.internal:8849",
            "host.containers.internal",
            "jira.svc",
        ] {
            assert_eq!(
                post(&addr, host, Some("tok-a"), &list()).await.0,
                200,
                "{host}"
            );
        }
        assert_eq!(
            post(&addr, "evil.example", Some("tok-a"), &list()).await.0,
            403
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_replaced_tokens_file_is_honored_on_the_next_request() {
        let dir = tempfile::tempdir().unwrap();
        let file = tokens(dir.path(), "tok-a sb-a /sandbox/a\n");
        let addr = serve(jira(), &file).await;
        assert_eq!(
            post(&addr, "127.0.0.1", Some("tok-a"), &list()).await.0,
            200
        );
        assert_eq!(
            post(&addr, "127.0.0.1", Some("tok-c"), &list()).await.0,
            401
        );
        let staged = dir.path().join("tokens.next");
        std::fs::write(&staged, "tok-c sb-c /sandbox/c\n").unwrap();
        std::fs::rename(&staged, &file).unwrap();
        assert_eq!(
            post(&addr, "127.0.0.1", Some("tok-a"), &list()).await.0,
            401
        );
        assert_eq!(
            post(&addr, "127.0.0.1", Some("tok-c"), &list()).await.0,
            200
        );
        std::fs::write(&file, "tok-c\n").unwrap();
        assert_eq!(
            post(&addr, "127.0.0.1", Some("tok-c"), &list()).await.0,
            500,
            "a malformed tokens file admits no one"
        );
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            post(&addr, "127.0.0.1", Some("tok-c"), &list()).await.0,
            500
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_selected_tools_are_listed_or_callable() {
        let dir = tempfile::tempdir().unwrap();
        let file = tokens(dir.path(), "tok-a sb-a\n");
        let picked: ToolSelection = "jira_search,jira_markdown_to_adf".parse().unwrap();
        let addr = serve(jira().select(&picked), &file).await;
        let (status, reply) = post(&addr, "127.0.0.1", Some("tok-a"), &list()).await;
        assert_eq!(status, 200, "{reply}");
        assert_eq!(
            listed_names(&reply),
            ["jira_markdown_to_adf", "jira_search"]
        );

        let md = call("jira_markdown_to_adf", json!({"markdown": "**hi**"}));
        let (status, reply) = post(&addr, "127.0.0.1", Some("tok-a"), &md).await;
        assert_eq!(status, 200, "{reply}");
        let body = json_body(&reply);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"strong\""), "{reply}");

        let refused = call("jira_get_issue", json!({"issue_key": "PROJ-1"}));
        let (_, reply) = post(&addr, "127.0.0.1", Some("tok-a"), &refused).await;
        let body = json_body(&reply);
        assert!(body.get("result").is_none(), "{reply}");
        assert_eq!(body["error"]["message"], "tool not found", "{reply}");
    }
}
