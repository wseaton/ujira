default: lint test

# Debug build.
build:
    cargo build

# Format, then the full clippy sweep (warnings are bugs in waiting).
lint:
    cargo fmt
    cargo clippy --all --benches --tests --examples --all-features
    cargo clippy --all-targets --no-default-features

test:
    cargo test
    cargo test --no-default-features

# Verify credentials resolve and the site answers. Run this FIRST when an install misbehaves.
check: build
    ./target/debug/ujira check

# Install to ~/.cargo/bin/ujira and seed ~/.config/ujira/config.toml if it's missing.
install:
    cargo install --path . --locked
    ujira write-config

# Drive one tool over a real stdio MCP handshake, e.g.
#   just smoke jira_get_issue '{"issue_key":"PROJ-142"}'
smoke tool args='{}': build
    ./scripts/smoke.sh {{tool}} '{{args}}'

# Show the token cost of a real issue both ways — the reason this server exists.
compare key: build
    @echo "text: $(./scripts/smoke.sh jira_get_issue '{"issue_key":"{{key}}"}' | wc -c) chars"
    @echo "json: $(./scripts/smoke.sh jira_get_issue '{"issue_key":"{{key}}","format":"json"}' | wc -c) chars"

# Register with Claude Code at user scope (available in every project).
claude-install: install
    claude mcp add --scope user jira -- "$HOME/.cargo/bin/ujira" mcp serve

# Drop the heavyweight Atlassian MCP from Claude Code.
claude-remove-atlassian:
    claude mcp remove --scope user atlassian || claude mcp remove atlassian
