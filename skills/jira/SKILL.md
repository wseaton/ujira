---
name: jira
description: Read and write JIRA Cloud issues from the shell with the ujira CLI — search by JQL, read issues and comment threads, comment, create, update, transition, link, manage labels, and attach files. Use when a task involves JIRA tickets, requirements written in an issue, sprint or backlog state, or when you need to leave a comment or file an issue. Output is compact plain text built for reading in context, and the same sixteen operations are available as MCP tools.
---

# Work JIRA from the shell

`ujira` is both an MCP server and a CLI over the same sixteen operations, printing
the same bytes either way. Reach for the CLI when you want to **script** JIRA:
loop over keys, pipe into grep, pull one field out of thirty issues. Reach for the
MCP tools when you want a single lookup mid-conversation.

The output is deliberately compact — a ticket costs roughly a fifth of what its raw
JSON does, so you can read ten of them without burning the context you needed for
the actual work.

## Prerequisites

- `ujira` on PATH (`cargo install --git https://github.com/wseaton/ujira`).
- Working credentials: `ujira check` prints the site, account, access level, and
  where the token came from. **Run it first** if anything behaves oddly; it
  distinguishes "not configured" from "wrong token" from "no permission".
- Access level matters. `ujira check` reports it:
  - `read-only` — the four read verbs
  - `read-comment` — reads plus `comment`
  - `read-write` — everything
  A refusal names the level, so don't retry a create against a `read-only` client.

## Reading

```bash
# One compact line per issue: KEY [Type/Status] Summary @assignee #labels
ujira search 'project = PROJ AND status = "In Progress" ORDER BY updated DESC' -l 20

# One issue: header fields, links, subtasks, curated custom fields, description
ujira issue PROJ-142

# ...with the comment thread, and a tighter cap on prose
ujira issue PROJ-142 --comments --max-chars 2000

# Just the thread (newest 10, printed oldest-first)
ujira comments PROJ-142 -l 10
```

Empty fields are dropped, so what you see is what the issue has. Timestamps are
trimmed to the day, and Jira's `{color}` markup is stripped.

### JQL that pays off

```bash
# Everything assigned to me that isn't done
ujira search 'assignee = currentUser() AND statusCategory != Done'

# Recently touched, most recent first — the usual "what's happening here"
ujira search 'project = PROJ AND updated >= -7d ORDER BY updated DESC'

# By label, by epic, by fix version
ujira search 'labels = platform AND fixVersion = "4.2"'
ujira search 'parent = PROJ-7'
```

`search` returns at most 100 (`-l`, default 25). Narrow the JQL rather than paging:
a tighter query is cheaper than a longer list you then have to read.

## Scripting

The one-line-per-issue format is built for this.

```bash
# Read every open bug in a project, in one pass
for key in $(ujira search 'project = PROJ AND type = Bug AND statusCategory != Done' -l 50 \
             | tail -n +2 | cut -d' ' -f1); do
  ujira issue "$key" --max-chars 800
  echo "---"
done

# Which of these tickets mention a term?
ujira search 'project = PROJ AND updated >= -30d' -l 100 | grep -i cache

# When you need to parse rather than read, ask for JSON
ujira issue PROJ-142 --json | jq -r '.fields.customfield_10001.name'
```

The first line of `search` output is the count (`12 issues`); `tail -n +2` skips it.

## Writing

```bash
# Comment. `-` reads the body from stdin, which is how you post multiple lines.
ujira comment PROJ-142 'Confirmed on 4.2; the fix is in #1234.'
ujira comment PROJ-142 - <<'EOF'
Reproduced with:
  ./run --config cache.yaml
EOF

# Create. --label repeats; --parent puts it under an epic.
ujira create -p PROJ -t Story -s 'Cache-aware routing' \
  -d 'Route by cache residency instead of round-robin.' \
  --parent PROJ-7 --label routing --label perf

# Update in place. Omitted fields are left alone; --label REPLACES the set.
ujira update PROJ-142 -s 'Better summary' -d - <<'EOF'
Rewritten description.
EOF

# Move status. Omit the target to see what's reachable from here.
ujira transition PROJ-142
ujira transition PROJ-142 'In Progress'

# Link SOURCE to TARGET using the type's outward relationship.
# Omit everything to list the site's link types and their direction words.
ujira link
ujira link Blocks PROJ-7 PROJ-142     # PROJ-7 blocks PROJ-142
ujira link Depend PROJ-7 PROJ-142     # PROJ-7 depends on PROJ-142

# Labels, incrementally — unlike `update --label`, these leave the rest alone.
ujira add-labels PROJ-142 -l needs-triage -l perf
ujira remove-labels PROJ-142 -l needs-triage

# Attach a file. --json prints the attachment metadata (including the id).
ujira attach PROJ-142 report.md
ujira delete-attachment 10042
```

Bodies and descriptions are **plain text / JIRA wiki markup** by default: `h2. Heading`,
`* bullet`, `{code}…{code}`. When you have real Markdown (headings, code blocks, tables,
links) and want the formatting to survive, pass `--description-format markdown` to
`comment`, `create`, or `update`:

```bash
ujira comment PROJ-142 --description-format markdown - <<'EOF'
## Findings

| Case | Result |
|------|--------|
| warm | 12ms   |
| cold | 340ms  |
EOF
```

### Custom fields

Typed flags cover the common fields; anything else goes through `--fields`, which is
merged last and can override them:

```bash
ujira fields team                      # -> customfield_10001  Team
ujira user-search jo@example.com       # -> accountId  email  displayName
ujira components PROJ                  # one component name per line
ujira create -p PROJ -t Story -s 'x' --fields '{"customfield_10001":{"id":"42"}}'
```

`ujira issue` already surfaces a curated set of custom fields by friendly name
(`team`, `rice_score`, `target_version`, …), configured in
`~/.config/ujira/config.toml`.

## Before you write

- **Transitions are workflow-specific.** Run `ujira transition KEY` with no target
  and pick from what it lists; guessing a status name fails.
- **`update --label` replaces the label set**, it doesn't append. Use `add-labels` /
  `remove-labels` to edit incrementally.
- **A create is hard to take back.** Confirm the project and issue type with the user
  when you're inferring them rather than being told.
- **Leave `UJIRA_LOG` / `RUST_LOG` unset.** Logging is off by default so the only bytes
  you read are the answer. Set `UJIRA_LOG=ujira=debug` only to debug a failing call, and
  expect the stderr noise in your context when you do.
