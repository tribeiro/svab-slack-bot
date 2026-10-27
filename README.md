# svab-slack-bot

Posts [SVAB](https://rubinobs.atlassian.net/wiki/x/HQBqSg) ticket updates to
Slack (`#recap-software`). Unlike the Jira Slack plugin, it can put details and
follow-up updates in a thread under the original announcement.

> Status: milestone 2. When a ticket moves to **In Review**, the bot posts the
> review request to the channel and the key details in its thread. Updates for
> later statuses and catch-up after downtime are coming next.

## Architecture

```
Jira Automation ──POST /jira/webhook──▶ server.rs   (auth, validate, forward)
                                             │  Command::IssueChanged { key, reply }
                                             ▼  tokio mpsc channel
                                        actor.rs    (one command at a time)
                                             │
                    ┌────────────────────────┼─────────────────────────┐
                    ▼                        ▼                         ▼
                jira.rs              svab::decide()               slack.rs
       issue + thread state     (pure: what should we post?)   chat.postMessage
                                             │
                                        messages.rs (text)
```

- **`server.rs`**: the HTTP layer. It checks the bearer secret, validates the
  issue key, sends a `Command` to the actor, and waits for the reply on a
  `oneshot` channel. The reply becomes the HTTP status, so failures show up in
  the Jira Automation audit log.
- **`actor.rs`**: a single task that does all the work. Commands are processed in
  order, one at a time, so events for the same ticket can't race and no locks
  are needed. For each ticket it fetches the facts from Jira, asks
  `svab::decide` what to do, and carries out the resulting actions.
- **`svab.rs`**: domain types (`Issue`, `Status`, `Level`, `ThreadState`,
  `Action`) and `decide()`. There's no I/O here, so every case is a simple unit test.
- **`messages.rs`**: the text of each Slack message (Slack mrkdwn).
- **`jira.rs`** / **`slack.rs`**: thin HTTP clients for the few API calls we use.
- **`config.rs`**: configuration from environment variables.
- **`error.rs`**: HTTP-facing errors and their status codes.

### Where state lives

The bot process keeps no state. For each announced ticket it stores the Slack
channel, the thread timestamp and the last status it posted about in a Jira
**issue property** (`svab-slack-bot`) on the ticket itself. Restarts lose
nothing, and handling is idempotent: an event for a ticket that is already
announced posts nothing.

The webhook body only says *which* ticket changed. The bot always reads the
real state from the Jira API, so a forged or replayed request can't make it
post anything that isn't true.

| Response | Meaning |
|---|---|
| 200 | Event handled |
| 400 | Body isn't `{"issue_key": "SVAB-<n>"}` |
| 401 | Missing or wrong bearer secret |
| 500 | Handling failed (message in body) or actor not running |
| 503 | Queue full; retry later |

## Adding a new notification

1. Add a variant to `Action` in `svab.rs` and return it from `decide()` for the
   right status/state combination. Add a unit test there.
2. Add a function that builds the text in `messages.rs`.
3. Handle the new action in `SvabActor::handle_issue` (`actor.rs`).

## Configuration

| Variable | Required | Default | Description |
|---|---|---|---|
| `SVAB_BOT_WEBHOOK_SECRET` | yes | | Bearer token Jira Automation must send (≥ 32 chars; `openssl rand -hex 32`) |
| `SVAB_BOT_JIRA_SITE_URL` | yes | | e.g. `https://rubinobs.atlassian.net` (used for ticket links) |
| `SVAB_BOT_JIRA_API_URL` | no | site URL | REST base URL; set to `https://api.atlassian.com/ex/jira/<cloud-id>` for scoped/service-account tokens |
| `SVAB_BOT_JIRA_EMAIL` | yes | | Jira account the bot uses |
| `SVAB_BOT_JIRA_API_TOKEN` | yes | | API token of that account |
| `SVAB_BOT_SLACK_BOT_TOKEN` | yes | | Slack app bot token (`xoxb-…`) |
| `SVAB_BOT_SLACK_CHANNEL_ID` | yes | | Channel to post in (`C…`) |
| `SVAB_BOT_SLACK_APPROVERS_GROUP_ID` | yes | | User group ID of `@svab-approvers` (`S…`) |
| `SVAB_BOT_SLACK_MAIN_GROUP_ID` | yes | | User group ID of `@svab-main` (`S…`) |
| `SVAB_BOT_BIND_ADDR` | no | `0.0.0.0:8080` | HTTP listen address |
| `SVAB_BOT_JIRA_PROJECT` | no | `SVAB` | Only issue keys from this project are accepted |
| `RUST_LOG` | no | `info` | Log level/filter, e.g. `svab_slack_bot=debug` |

### Accounts and permissions

- **Slack app**: needs the `chat:write` bot scope, and the bot must be a member
  of the channel (`/invite @<bot>`). To find IDs, open the channel details and
  look at the bottom for the channel ID. For a user group, use the group's
  profile or `usergroups.list`.
- **Jira account**: must be able to browse SVAB issues and edit them, because
  setting an issue property requires edit permission.

## Running locally

Point `SVAB_BOT_SLACK_CHANNEL_ID` at a **test channel**.

```sh
export SVAB_BOT_WEBHOOK_SECRET=$(openssl rand -hex 32)
export SVAB_BOT_JIRA_SITE_URL=https://rubinobs.atlassian.net
export SVAB_BOT_JIRA_EMAIL=... SVAB_BOT_JIRA_API_TOKEN=...
export SVAB_BOT_SLACK_BOT_TOKEN=xoxb-... SVAB_BOT_SLACK_CHANNEL_ID=C...
export SVAB_BOT_SLACK_APPROVERS_GROUP_ID=S... SVAB_BOT_SLACK_MAIN_GROUP_ID=S...
RUST_LOG=svab_slack_bot=debug cargo run

# in another terminal
curl -i localhost:8080/healthz
curl -i -X POST localhost:8080/jira/webhook \
  -H "Authorization: Bearer $SVAB_BOT_WEBHOOK_SECRET" \
  -d '{"issue_key": "SVAB-15"}'
```

The bot only announces tickets that are **In Review** and have no stored thread
yet. To announce the same ticket again, delete the property:

```sh
curl -u "$SVAB_BOT_JIRA_EMAIL:$SVAB_BOT_JIRA_API_TOKEN" -X DELETE \
  "$SVAB_BOT_JIRA_SITE_URL/rest/api/3/issue/SVAB-15/properties/svab-slack-bot"
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

Tests for `jira.rs`, `slack.rs` and `actor.rs` run against
[wiremock](https://docs.rs/wiremock) fake servers, so they need no network
access or credentials.
