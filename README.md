# AgentPeek

Agent coordination daemon for macOS. Resource locks and shared notes for any MCP-compatible AI agent.

## The problem

Two Claude Code sessions running on the same repository can't coordinate their work. One session is adding authentication to the API while another is implementing notifications. Both sessions run `npm install` simultaneously, corrupting `node_modules`, and they independently generate migration filenames without checking if the other session already claimed that name. Without a shared coordination layer, agent work races, conflicts, and fails silently.

## How it works

AgentPeek is a native Swift daemon that runs as a macOS LaunchAgent. Any MCP-compatible agent (Claude Code, Cursor, Copilot) connects to it at `127.0.0.1:27183` to acquire locks and share state. Locks are TTL-based so they expire if an agent crashes without releasing them. Notes allow agents to leave breadcrumbs for each other — recording which migrations were created, which files were modified, what assumptions were made.

## Tools

| Tool | Parameters | Returns |
|------|-----------|---------|
| `lock_resource` | `name`, `agent_id`, `ttl_minutes` (default 15) | `{locked, expires_in_seconds}` or `{locked:false, held_by}` |
| `unlock_resource` | `name`, `agent_id` | `{released:true}` |
| `renew_lock` | `name`, `agent_id`, `ttl_minutes` | `{renewed:true, expires_in_seconds}` |
| `list_locks` | — | `[{name, agent_id, expires_in_seconds}]` |
| `set_note` | `key`, `value`, `author?`, `ttl_minutes?` | `{saved:true}` |
| `get_note` | `key` | `{key, value, author?, expires_in_seconds?}` |
| `list_notes` | — | `[{key, value, author?, expires_in_seconds?}]` |

### Naming conventions

- Files: `file:/abs/path/to/file.ts`
- Processes: `npm-install`, `db-migrations`, `tests:unit`
- Agent presence: `agent:claude-session-abc`

## Example: two sessions, no conflicts

Session A and Session B both need to run migrations but only one can modify the schema at a time. Here's how AgentPeek prevents corruption:

1. **Session A acquires the lock:** Call `lock_resource` with `name="db-migrations"`, `agent_id="session-A"`, `ttl_minutes=30`
   - Returns: `{locked: true, expires_in_seconds: 1800}`

2. **Session B tries and fails:** Call `lock_resource` with `name="db-migrations"`, `agent_id="session-B"`, `ttl_minutes=30`
   - Returns: `{locked: false, held_by: "session-A"}`
   - Session B knows to wait or skip this task

3. **Session A records its intent:** Call `set_note` with `key="migration:001-auth-tables"`, `value="Creating users and sessions tables"`, `author="session-A"`
   - Returns: `{saved: true}`
   - Session B can now query notes and see what session A is building

4. **Session B checks notes:** Call `list_notes` to see all recorded migrations
   - Returns: `[{key: "migration:001-auth-tables", value: "Creating users and sessions tables", author: "session-A"}]`
   - Session B avoids creating conflicting schema

5. **Session A completes and releases:** Call `unlock_resource` with `name="db-migrations"`, `agent_id="session-A"`
   - Returns: `{released: true}`

6. **Session B can now proceed:** Call `lock_resource` again, succeeds this time, and safely applies its own migrations

## Install

### 1. Build

```bash
git clone https://github.com/yourusername/agentpeek.git
cd agentpeek
swift build -c release
sudo cp .build/release/agentpeek /usr/local/bin/agentpeek
```

### 2. Install LaunchAgent

```bash
cp com.agentpeek.daemon.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.agentpeek.daemon.plist
```

### 3. Verify it's running

```bash
curl -s -X POST http://127.0.0.1:27183/ \
  -H "Content-Type: application/json" \
  -H "Host: 127.0.0.1:27183" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'
```

Expected: `{"result":{"serverInfo":{"name":"agentpeek",...},...}}`

## Configure Claude Code

Add to `~/.claude/mcp_servers.json`:

```json
{
  "agentpeek": {
    "transport": "http",
    "url": "http://127.0.0.1:27183"
  }
}
```

## License

MIT
