# CLAUDE.md

Slack bot for the SVAB process: Jira Automation calls a webhook, the bot posts
the review request to Slack and follow-ups in its thread. Read `README.md`
first: it has the architecture diagram, module roles, configuration and
operations. This file only records the rules and decisions behind the code.

## Commands

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
uvx towncrier create <TICKET>.<feature|bugfix|doc|misc>.md -c "Past-tense sentence."
```

CI runs all of the above and fails a PR without a news fragment in
`changelog.d/`. Every PR needs one (`+name.misc.md` if there's no ticket).

## Architecture rules

- **One actor does all the work.** Event sources (`server.rs`, `reconciler.rs`)
  only send a `Command` over the mpsc channel and await the `oneshot` reply.
  No `Mutex`, no shared state; new kinds of work are new `Command` variants.
- **`svab::decide()` is pure.** No I/O, no async. Every rule about *what* to
  post lives there and gets a unit test; the actor only carries out the
  returned `Action`s.
- **Presentation lives in `messages.rs`.** Text, emoji and mrkdwn formatting
  go there, not on the domain types in `svab.rs`.
- **The bot process is stateless.** Per-ticket state (channel, thread_ts,
  last_status) is the Jira issue property `svab-slack-bot`. Never add a local
  database or in-memory state that must survive restarts.
- **Never trust the webhook payload.** It only names a ticket; always re-read
  the issue and the property from Jira.
- **Order of writes is deliberate.** Announcement: post, then save the
  property immediately (a duplicate main post is the worst failure). Thread
  update: post, then save `last_status` (a duplicate reply beats a lost one).
- **Thread replies use the stored `thread.channel`**, never the configured
  channel, so changing `SVAB_BOT_SLACK_CHANNEL_ID` doesn't break old threads.

## Conventions

- Env vars are prefixed `SVAB_BOT_` and spelled out in full (greppable).
  Secrets get redacted in hand-written `Debug` impls.
- Escape all Jira-provided text with `messages::escape` before putting it in
  Slack mrkdwn.
- Match on `Status`/`Level` exhaustively; avoid `_ =>` so new variants are
  flagged by the compiler.
- Tests: pure logic as unit tests; anything touching Jira or Slack as
  wiremock tests (see `actor.rs`). Pick fixture values that would expose the
  bug you're guarding against.
- Error context strings are lowercase, without a trailing period (anyhow
  chains them as `outer: inner`).
- Commits: one module or feature per commit, each building and passing tests.
- Rust is pinned in `rust-toolchain.toml`; bump it together with the `rust:`
  tag in the `Dockerfile`.

## Decided, don't revisit without asking

- Event source is a Jira Automation web request with a shared secret in the
  `X-Svab-Bot-Secret` header (not polling, not Jira's native webhooks). The
  bot keeps checking it even behind an authenticating ingress (Gafaelfawr),
  which owns the `Authorization` header.
- Cutover from the Jira plugin uses the changelog time the ticket entered In
  Review vs `SVAB_BOT_ANNOUNCE_SINCE`. Tickets that went through review
  unannounced are logged, never announced late.
- Status updates are plain thread replies, never `reply_broadcast`: the
  channel shouldn't get follow-up noise.
- One replica only: the single actor is what prevents duplicate posts.
