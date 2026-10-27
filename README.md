# svab-slack-bot

Posts [SVAB](https://rubinobs.atlassian.net/wiki/x/HQBqSg) ticket updates to
Slack (`#recap-software`). Unlike the Jira Slack plugin, it can put details and
follow-up updates in a thread under the original announcement.

> Status: milestone 1 (skeleton). The webhook is authenticated and reaches the
> actor, which only logs the issue key for now.

## Architecture

```
Jira Automation ──POST /jira/webhook──▶ server.rs   (auth, validate, forward)
                                             │  Command::IssueChanged { key, reply }
                                             ▼  tokio mpsc channel
                                        actor.rs    (one command at a time)
```

- **`server.rs`**: the HTTP layer. It checks the bearer secret, validates the
  issue key, sends a `Command` to the actor, and waits for the reply on a
  `oneshot` channel. The reply becomes the HTTP status, so failures show up in
  the Jira Automation audit log.
- **`actor.rs`**: a single task that does all the work. Commands are processed in
  order, one at a time, so events for the same ticket can't race and no locks
  are needed. New kinds of work become new `Command` variants.
- **`config.rs`**: configuration from environment variables.
- **`error.rs`**: HTTP-facing errors and their status codes.

Planned: the actor will fetch the ticket from Jira, decide what to post with a
pure `decide()` function, post to Slack, and store the thread reference on the
Jira ticket as an issue property. A reconciler will catch up on events missed
while the bot was down.

| Response | Meaning |
|---|---|
| 200 | Event handled |
| 400 | Body isn't `{"issue_key": "SVAB-<n>"}` |
| 401 | Missing or wrong bearer secret |
| 500 | Handling failed (message in body) or actor not running |
| 503 | Queue full; retry later |

## Configuration

| Variable | Required | Default | Description |
|---|---|---|---|
| `SVAB_BOT_WEBHOOK_SECRET` | yes | | Bearer token Jira Automation must send (≥ 32 chars; `openssl rand -hex 32`) |
| `SVAB_BOT_BIND_ADDR` | no | `0.0.0.0:8080` | HTTP listen address |
| `SVAB_BOT_JIRA_PROJECT` | no | `SVAB` | Only issue keys from this project are accepted |
| `RUST_LOG` | no | `info` | Log level/filter |

## Running locally

```sh
export SVAB_BOT_WEBHOOK_SECRET=$(openssl rand -hex 32)
cargo run

# in another terminal
curl -i localhost:8080/healthz
curl -i -X POST localhost:8080/jira/webhook \
  -H "Authorization: Bearer $SVAB_BOT_WEBHOOK_SECRET" \
  -d '{"issue_key": "SVAB-1"}'
```

## Jira Automation rule (sketch)

- Trigger: *Work item transitioned* (project SVAB).
- Action: *Send web request*.
  - URL: `https://<host>/jira/webhook`
  - Method: `POST`
  - Headers: `Authorization: Bearer <SVAB_BOT_WEBHOOK_SECRET>` (mark it as hidden), plus `Content-Type: application/json`
  - Body (custom data): `{"issue_key": "{{issue.key}}"}`

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
docker build -t svab-slack-bot .
```
