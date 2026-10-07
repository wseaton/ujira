use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn ujira(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ujira"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("JIRA_URL", "http://127.0.0.1:9")
        .env("JIRA_EMAIL", "agent@example.com")
        .env("JIRA_API_TOKEN", "not-a-real-token")
        .env("UJIRA_ACCESS", "read-only")
        .env("UJIRA_KEYCHAIN", "false");
    cmd
}

fn refused(output: Output) -> String {
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    String::from_utf8(output.stderr).unwrap()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn post(port: u16, token: Option<&str>, body: &str) -> (u16, String) {
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        sock,
        "POST /mcp HTTP/1.1\r\nHost: host.openshell.internal:{port}\r\n\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         {auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    sock.read_to_string(&mut out).unwrap();
    let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, out)
}

fn tool_names(result: &Value) -> Vec<String> {
    let mut names: Vec<String> = result["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

const LIST: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;

#[test]
fn an_unknown_tool_name_fails_startup() {
    let home = tempfile::tempdir().unwrap();
    let flag = ujira(home.path())
        .args(["mcp", "serve", "--tools", "jira_search,jira_nope"])
        .output()
        .unwrap();
    assert!(refused(flag).contains("unknown tool \"jira_nope\""));
    let env = ujira(home.path())
        .args(["mcp", "serve"])
        .env("MCP_TOOLS", "jira_get_issue,jira_bogus")
        .output()
        .unwrap();
    assert!(refused(env).contains("unknown tool \"jira_bogus\""));
}

#[test]
fn http_mode_refuses_to_start_without_a_tokens_file() {
    let home = tempfile::tempdir().unwrap();
    let output = ujira(home.path())
        .args(["mcp", "serve"])
        .env("MCP_BIND", format!("127.0.0.1:{}", free_port()))
        .output()
        .unwrap();
    assert!(refused(output).contains("--tokens-file"));
}

#[test]
fn a_crucible_spawned_server_serves_its_selection_behind_the_tokens_file() {
    let home = tempfile::tempdir().unwrap();
    let tokens = home.path().join("tokens");
    std::fs::write(&tokens, "tok-a sb-a /sandbox/workspace\n").unwrap();
    let port = free_port();
    let _server = Server(
        ujira(home.path())
            .args(["mcp", "serve"])
            .env("MCP_NAME", "ujira")
            .env("MCP_BIND", format!("127.0.0.1:{port}"))
            .env("MCP_TOKENS_FILE", &tokens)
            .env("MCP_TOOLS", "jira_search,jira_get_issue,jira_get_comments")
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "no listener on {port} within 5s");
        std::thread::sleep(Duration::from_millis(50));
    }

    assert_eq!(post(port, None, LIST).0, 401);
    assert_eq!(post(port, Some("tok-b"), LIST).0, 401);
    let (status, reply) = post(port, Some("tok-a"), LIST);
    assert_eq!(status, 200, "{reply}");
    let (_, body) = reply.split_once("\r\n\r\n").unwrap();
    let body: Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        tool_names(&body["result"]),
        ["jira_get_comments", "jira_get_issue", "jira_search"]
    );
}

#[test]
fn stdio_stays_the_default_and_honors_the_tool_selection() {
    let home = tempfile::tempdir().unwrap();
    let mut child = ujira(home.path())
        .args(["mcp", "serve", "--tools", "jira_markdown_to_adf"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let _server = Server(child);
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "test", "version": "0"}}});
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    writeln!(stdin, "{init}\n{initialized}\n{LIST}").unwrap();

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let listed = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = rx
            .recv_timeout(left)
            .expect("no tools/list reply over stdio");
        let msg: Value = serde_json::from_str(&line).unwrap();
        if msg["id"] == 2 {
            break msg;
        }
    };
    assert_eq!(tool_names(&listed["result"]), ["jira_markdown_to_adf"]);
}
