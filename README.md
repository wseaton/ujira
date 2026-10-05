# ujira

A JIRA Cloud MCP server and CLI with sixteen tools and compact plain-text output, built for agents
that read many issues and pay per token. It is also a Rust library.

## Install

```bash
cargo install --git https://github.com/wseaton/ujira
ujira write-config   # writes ~/.config/ujira/config.toml
ujira set-token      # reads the API token from stdin into the OS keychain
ujira check          # verifies the site and reports where each setting came from
```

Prebuilt binaries for Linux and macOS (x86_64, arm64) are on the
[releases page](https://github.com/wseaton/ujira/releases). Tokens come from
<https://id.atlassian.com/manage-profile/security/api-tokens>.

Register as an MCP server (stdio):

```bash
claude mcp add --scope user jira -- ~/.cargo/bin/ujira mcp serve
```

```json
{ "mcpServers": { "jira": { "command": "/abs/path/to/ujira", "args": ["mcp", "serve"] } } }
```

## Configure

Env vars beat the config file, which beats the compiled-in defaults
([`presets/redhat.toml`](presets/redhat.toml)). The file is `$UJIRA_CONFIG`, else
`$XDG_CONFIG_HOME/ujira/config.toml` or `~/.config/ujira/config.toml`. Pre-rename `JIRA_MCP_*`
variables and `jira-mcp/config.toml` are still read.

| Key | Env | |
| --- | --- | --- |
| `url` | `JIRA_URL` | Site URL (falls back to a jira-cli config's `server:`) |
| `username` | `JIRA_USERNAME`, else `JIRA_EMAIL` | Account email (falls back to jira-cli's `login:`) |
| `access` | `UJIRA_ACCESS` | `read-only`, `read-comment`, or `read-write` (default) |
| `keychain` | `UJIRA_KEYCHAIN` | Use the OS keychain (default true) |
| `token_file` | `JIRA_API_TOKEN_FILE` | Default `~/.jiratoken` |
| `token` | `JIRA_API_TOKEN` | Inline token |
| `[custom_fields]` | | `friendly_name = "customfield_N"`; replaces the default map |

The token is the first of: `JIRA_API_TOKEN`, the keychain entry, `token_file`, `token`. Find custom
field ids with `ujira fields <name>`.

## Tools

Every CLI command is the same operation as its MCP tool and prints the same text. `search`,
`issue`, `comments`, `user-search`, `components`, and `attach` take `--json` (MCP:
`format: "json"`) for the raw payload.

| MCP tool | CLI | |
| --- | --- | --- |
| `jira_search` | `search` | JQL search, one line per issue |
| `jira_get_issue` | `issue` | One issue as compact text |
| `jira_get_comments` | `comments` | Newest N comments, oldest first |
| `jira_add_comment` | `comment` | Post a comment |
| `jira_create_issue` | `create` | Create an issue |
| `jira_update_issue` | `update` | Edit fields in place |
| `jira_transition` | `transition` | Change status, or list reachable ones |
| `jira_link_issues` | `link` | Link source to target, or list link types |
| `jira_add_labels` | `add-labels` | Add labels, keep the rest |
| `jira_remove_labels` | `remove-labels` | Remove specific labels |
| `jira_add_attachment` | `attach` | Upload an attachment |
| `jira_delete_attachment` | `delete-attachment` | Delete an attachment |
| `jira_markdown_to_adf` | `markdown-to-adf` | Convert markdown to ADF JSON (local) |
| `jira_fields` | `fields` | Field ids by name |
| `jira_user_search` | `user-search` | Users by email or name |
| `jira_components` | `components` | A project's components |

`ujira <command> --help` has the flags. `ujira link Blocks PROJ-7 PROJ-142` reads "PROJ-7 blocks
PROJ-142".

## Access levels

Checked inside the client before every mutating request, so no tool can widen it.

- `read-only`: reads only.
- `read-comment`: reads plus `comment`.
- `read-write`: everything.

## Library

`ujira = { git = "https://github.com/wseaton/ujira", default-features = false }` builds only the
client, typed models, and renderers, without the MCP server, CLI, or keychain dependencies (features
`mcp`, `cli`, `keychain`; `cli` is the default and implies the other two).

```rust
use ujira::{Access, Config, JiraClient, fields::field_text};

let Some(mut cfg) = Config::from_env() else { return Ok(()) }; // env only; Config::load layers files
cfg.access = Access::ReadOnly;
let jira = JiraClient::new(cfg);

let issues = jira.search_all("project = PROJ ORDER BY created", &["labels"], 500).await?;
for (name, value) in &jira.get_fields_by_name("PROJ-1", &["Embargo Status"]).await? {
    println!("{name}: {:?}", field_text(value)?);
}
let state = jira.get_issue_property("PROJ-1", "myapp.state").await?; // None when unset
```

`get_fields_by_name` fails with `fields::FieldResolutionError` when a name is unknown or ambiguous.
Every call opens a `debug` [`tracing`](https://docs.rs/tracing) span; the library installs no
subscriber. The CLI logs to stderr only when `UJIRA_LOG` or `RUST_LOG` is set.

## Development

```bash
just                                                   # fmt, clippy, test
just smoke jira_get_issue '{"issue_key":"PROJ-142"}'   # one tool over a real MCP handshake
```

The agent skill in [`skills/jira/SKILL.md`](skills/jira/SKILL.md) covers CLI usage for agents.

MIT.
