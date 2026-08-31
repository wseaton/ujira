# ujira

µJIRA: MCP server and CLI for JIRA Cloud. Fourteen operations, compact plain-text output.

Built for agents that read many issues and pay per token. Measured on one feature request with a long
description, a dozen labels, and two links: 3422 chars rendered against 18151 chars of raw JSON. The
tool schemas total 9.8 KB. Clients that announce every tool up front carry that in context on every
turn; Claude Code defers MCP tool schemas and loads them on demand via tool search, so there the
schema cost is per-use and the compact rendering is where the savings are.

```
ujira <COMMAND>
```

`ujira mcp serve` serves MCP over stdio. Every other command is the same operation an MCP tool
exposes, over the same code, printing the same bytes.

## Install

Requires a Rust toolchain. TLS is rustls; there is no OpenSSL dependency.

```bash
cargo install --git https://github.com/wseaton/ujira
ujira write-config     # -> ~/.config/ujira/config.toml
ujira set-token        # reads the token from stdin, stores it in the OS keychain
ujira check            # verifies, and reports where each setting came from
```

Prebuilt binaries for linux and macos on x86_64 and arm64 are attached to each
[release](https://github.com/wseaton/ujira/releases).

Register with Claude Code:

```bash
claude mcp add --scope user jira -- ~/.cargo/bin/ujira mcp serve
```

Any other client, as stdio:

```json
{ "mcpServers": { "jira": { "command": "/abs/path/to/ujira", "args": ["mcp", "serve"] } } }
```

## Configuration

Precedence: environment variable, then the config file, then the compiled-in defaults
([`presets/redhat.toml`](presets/redhat.toml)).

| Key | Environment | Meaning |
| --- | --- | --- |
| `url` | `JIRA_URL` | Site base url. Falls back to `server:` in a jira-cli config |
| `username` | `JIRA_USERNAME` | Account email. Falls back to `login:` in a jira-cli config |
| `keychain` | `UJIRA_KEYCHAIN` | Consult the OS credential store. Default true |
| `token_file` | `JIRA_API_TOKEN_FILE` | Token file. Default `~/.jiratoken` |
| `token` | `JIRA_API_TOKEN` | Token inline |
| `access` | `UJIRA_ACCESS` | `read-only`, `read-comment`, or `read-write`. Default `read-write` |
| `[custom_fields]` | — | `friendly_name = "customfield_N"` |

Config file lookup: `$UJIRA_CONFIG`, `$XDG_CONFIG_HOME/ujira/config.toml`,
`~/.config/ujira/config.toml`, then the pre-rename `jira-mcp/config.toml` when only it exists.
The pre-rename `JIRA_MCP_*` env vars are still honored. `JIRA_CLI_CONFIG` relocates the
[jira-cli](https://github.com/ankitpokhrel/jira-cli) file.

Unknown keys are rejected. A malformed config file is fatal; a malformed `UJIRA_ACCESS` falls back
to `read-only`.

### Token

Resolved in order, first hit wins. `check` reports which answered.

1. `JIRA_API_TOKEN`
2. OS keychain, filed under the account email — macOS Keychain, Windows Credential Manager, Secret
   Service on Linux. `set-token` writes it; `delete-token` removes it
3. `token_file`
4. `token` in the config file

`set-token` reads stdin so the token stays out of shell history and `ps` output. Get one from
<https://id.atlassian.com/manage-profile/security/api-tokens>.

### Access

Enforced in the client, beneath every operation, so a level cannot be widened by a tool.

| Level | Permits |
| --- | --- |
| `read-only` | `search`, `issue`, `comments`, `fields`, `user-search`, `components` |
| `read-comment` | the above plus `comment` |
| `read-write` | everything |

## Commands

| Command | Operation |
| --- | --- |
| `search <JQL> [-l N] [--json]` | one compact line per issue |
| `issue <KEY> [-c] [--max-chars N] [--json]` | one issue; `-c` includes the comment thread |
| `comments <KEY> [-l N] [--max-chars N] [--json]` | newest N comments, printed oldest-first |
| `comment <KEY> <BODY\|->` | post a comment; `-` reads stdin |
| `create -p KEY -t TYPE -s TEXT [-d TEXT\|-] [--parent KEY] [-l LABEL]… [--fields JSON]` | create |
| `update <KEY> [-s TEXT] [-d TEXT\|-] [-l LABEL]… [--fields JSON]` | edit in place |
| `transition <KEY> [TO]` | move status; omit `TO` to list what is reachable |
| `link [TYPE] [SOURCE] [TARGET]` | link source to target using the type's outward relationship; omit all to list link types |
| `add-labels <KEY> -l LABEL…` | append labels, leaving the rest alone |
| `remove-labels <KEY> -l LABEL…` | remove specific labels |
| `attach <KEY> <FILE> [--filename NAME] [--json]` | upload a file attachment |
| `delete-attachment <ID>` | delete an attachment by id |
| `markdown-to-adf [TEXT\|-]` | print the ADF JSON for markdown text |
| `fields [QUERY]` | field ids matching a name substring |
| `user-search <QUERY> [-l N]` | `accountId  email  displayName` per matching user |
| `components <PROJECT>` | component names, sorted |
| `check` | verify credentials, report resolved settings |
| `write-config` | install the config template; never overwrites |
| `set-token` / `delete-token` | manage the keychain entry |
| `mcp serve` | serve MCP over stdio |

`--fields` is merged last and overrides typed flags. `-l/--label` on `update` replaces the label set
rather than appending; `add-labels`/`remove-labels` edit incrementally. Descriptions and comment
bodies are plain text or JIRA wiki markup by default; `--description-format markdown` on `comment`,
`create`, and `update` converts markdown to ADF on the way in, so headings, code blocks, tables, and
links survive.

Errors exit non-zero. Over MCP the same errors return as a readable line, since a tool error is data
the model acts on.

For directional links, `SOURCE` is the issue performing the relationship and `TARGET` is the issue
receiving it:

```bash
ujira link Blocks PROJ-7 PROJ-142  # PROJ-7 blocks PROJ-142
ujira link Depend PROJ-7 PROJ-142  # PROJ-7 depends on PROJ-142
```

The `inwardIssue` and `outwardIssue` names used by Jira's REST API are intentionally not exposed as
semantic roles because their direction is counterintuitive.

### MCP tools

`jira_search`, `jira_get_issue`, `jira_get_comments`, `jira_add_comment`, `jira_create_issue`,
`jira_update_issue`, `jira_transition`, `jira_link_issues`, `jira_add_labels`, `jira_remove_labels`,
`jira_add_attachment`, `jira_delete_attachment`, `jira_markdown_to_adf`, `jira_fields`,
`jira_user_search`, `jira_components`.

Arguments match the commands. Read tools take `format: "json"` and `max_chars`.

## Output

```
PROJ-142 Cache-aware request routing
Feature Request/Approved/Critical
assignee: Sam Rivera
reporter: Jo Park
labels: routing, tech-reviewed
components: gateway
created: 2026-07-23
updated: 2026-07-24
target_version: 4.2
links: is cloned by PROJ-98 (New)

## description
…
```

Search rows are `KEY [Type/Status] Summary @assignee #labels`, one per issue, preceded by a count.

Empty and null fields are omitted. Timestamps are trimmed to the day. Integral numbers drop their
`.0`. Wiki `{color}` macros, CRLF, and runs of blank lines are stripped from prose. Content is never
reordered or reworded. `--json` returns the raw payload unmodified.

## Custom fields

`issue` surfaces custom fields by friendly name, dropping empty ones. The map is data, in
`[custom_fields]`, compiled in from `presets/redhat.toml` and installed by `write-config`.

The shipped ids are Red Hat's; every site numbers its own differently. Find yours with `fields`, then
replace the table — a user table replaces the default wholesale, without merging.

## Library

The crate is a library as well as a binary. An embedding host can reuse the client and renderers
without inheriting this tool surface:

```rust
use ujira::{Access, Config, JiraClient, render};

let Some(mut cfg) = Config::from_env() else { /* JIRA off: report `disabled` */ };
cfg.access = Access::ReadComment;
let jira = JiraClient::new(cfg);
let issue = jira.get_issue("PROJ-1", false).await?;
println!("{}", render::issue(&issue, jira.config(), 6000));
```

`Config::from_env` is strict and reads no files, for a host that injects credentials itself.
`Config::load` is the layered resolution the binary uses. To expose the full tool surface, serve
`ujira::JiraMcp`.

The library is instrumented with [`tracing`](https://docs.rs/tracing): a `debug` span per client
call, op, and MCP tool (issue keys, JQL, limits; never token or prose), a `debug` event per HTTP
request and response, `warn` on a non-2xx or an access-level refusal, and the token source at
config load. It never installs a subscriber; the host's does the filtering.

## Logging

Off by default. The CLI installs a subscriber only when `UJIRA_LOG` (or `RUST_LOG`) is set, and
writes it to stderr, so a plain invocation emits nothing but its answer:

```bash
UJIRA_LOG=ujira=debug ujira issue PROJ-142    # spans, requests, and the token source on stderr
```

## Agent skill

[`skills/jira/SKILL.md`](skills/jira/SKILL.md) documents the CLI for an agent: verbs, JQL patterns,
scripting idioms, and the sharp edges.

```bash
ln -s "$PWD/skills/jira" ~/.claude/skills/jira
```

## Development

```bash
just                                          # fmt, clippy, test
just check                                    # credentials against the live site
just smoke jira_get_issue '{"issue_key":"PROJ-142"}'   # one tool over a real MCP handshake
just compare PROJ-142                         # rendered size against raw JSON
```

Every operation is verified against a live JIRA Cloud site, writes included.

Constraints worth keeping:

- Prose endpoints default to `/rest/api/2`, so descriptions and comments are plain text rather than
  ADF documents. api/3 is reached only when a caller opts into `description_format=markdown`, and
  reads never move there.
- Search uses `POST /rest/api/3/search/jql`. The old `/rest/api/3/search` was removed (CHANGE-2046).
- Operations live in `ops.rs` alone. The MCP tool and the CLI subcommand are thin wrappers, so they
  cannot drift apart.
- Each tool costs context: every turn in clients that load schemas up front, per lookup in clients
  that defer them behind tool search. `tool_surface_stays_small` fails when the count changes, to
  keep growth a decision rather than a drift.

MIT.
