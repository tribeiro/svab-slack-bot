# Changelog

All notable changes to this project are recorded here. Entries are generated
by [towncrier](https://towncrier.readthedocs.io) from the fragments in
`changelog.d/`; don't edit below the marker by hand except to fix mistakes.

<!-- towncrier release notes start -->

## [0.1.0] - 2026-11-03

First release. The bot replaces the Jira Slack plugin for SVAB notifications,
adding threads under each announcement.

### Features

- Added configuration from `SVAB_BOT_*` environment variables. Secrets are redacted from logs, and values are trimmed so stray whitespace or newlines don't matter.
- Added the SVAB actor: a single task that handles events one at a time from a tokio `mpsc` channel, so two events for the same ticket can never race. Each event carries a `oneshot` reply with the outcome.
- Added the HTTP server, with `GET /healthz` and `POST /jira/webhook`. The webhook checks a bearer secret in constant time before reading the body, accepts only `{"issue_key": "<project>-<number>"}` (4 KB limit), and returns 200, 400, 401, 500 or 503 so failures show in the Jira Automation audit log.
- Added Jira and Slack settings: Jira site and API URL (for scoped tokens), account email and API token, Slack bot token, channel ID, and the `@svab-approvers` / `@svab-main` user group IDs.
- Added the SVAB domain types (`Issue`, `Status`, `Level`, `ThreadState`, `Action`) and the pure `decide()` function that chooses what to post.
- Added a Jira REST client that reads issues (including priority level, planned deployment, change contents and labels) and stores the bot's thread state on each ticket as the `svab-slack-bot` issue property.
- Added a Slack Web API client for `chat.postMessage`, including thread replies.
- SVAB requests are announced when they move to In Review: the review request (level emoji, ticket link, group mentions) goes to the channel, and the details (type, requester, planned deployment, change contents, BTS testing status, approval rules) go in its thread. The thread is saved right after the announcement, so a retry never posts it twice.
- Added startup checks: the bot exits with an explanation if the Jira credentials are rejected, the Jira account lacks Browse/Edit permission on the project, or the Slack bot token is invalid.
- Added `SVAB_BOT_ANNOUNCE_SINCE`, a cutover guard: tickets that entered In Review before the cutover (according to the Jira changelog) were announced by the old plugin and are not announced again. Tickets that went through review after the cutover without being announced are logged as a warning instead of being announced late.
- Added a reconciler that catches up on events missed while the bot was down: at startup and every `SVAB_BOT_RECONCILE_INTERVAL_MINUTES` (default 15), it checks every SVAB ticket updated in the last `SVAB_BOT_RECONCILE_LOOKBACK_HOURS` (default 72).
- Status changes of announced tickets (approved, done, rolled back, cancelled, rework, resubmitted) are posted as replies in the ticket's thread, with an emoji per status. Replies go to the channel the thread is in, even if the configured channel has changed since.

### Documentation

- Added a README covering the architecture, where state lives, how to add a notification, configuration, Slack and Jira permissions, startup checks, Jira auth troubleshooting, the cutover procedure, running locally and the Jira Automation rule.
- Documented CI, the pinned toolchain, the changelog workflow and the release process.
- Added `CLAUDE.md` with the architecture rules, conventions and settled design decisions.

### Other changes

- Added a multi-stage Dockerfile producing a small Debian image that runs as a non-root user.
- Jira client errors now include Jira's own error messages, and a 401 explains the likely causes (wrong email, expired token, scoped token needing the API gateway URL).
- Pinned the Rust toolchain to 1.98 for local builds, CI and the Docker image.
- Added a GitHub Actions workflow that runs fmt, clippy and tests on every PR, and publishes the Docker image to `ghcr.io` from `main` and version tags.
- Added Dependabot for Cargo crates and GitHub Actions.
- Added changelog management with towncrier; CI fails PRs that don't add a news fragment.
