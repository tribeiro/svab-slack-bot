//! Slack message text, in Slack's "mrkdwn" format.
//!
//! Formatting quick reference: `<url|text>` is a link, `<!subteam^ID>` mentions
//! a user group, `*bold*`, and emoji are written as `:name:`. Characters `&`,
//! `<` and `>` in user-provided text must be escaped (see [`escape`]).

use crate::svab::{Issue, Level};

/// Slack user groups mentioned in the announcement.
#[derive(Debug, Clone)]
pub struct Mentions {
    pub approvers_group_id: String,
    pub main_group_id: String,
}

/// The main post announcing a request that is ready for review.
pub fn announcement(issue: &Issue, mentions: &Mentions) -> String {
    let (emoji, level) = match issue.level {
        Some(Level::Consultation) => (":large_blue_circle:", "CONSULTATION"),
        Some(Level::Critical) => (":red_circle:", "CRITICAL"),
        None => (":white_circle:", "UNKNOWN LEVEL"),
    };
    format!(
        "{emoji} *{level}*: <!subteam^{approvers}> (attn <!subteam^{main}>) SVAB request ready for review (:thread:).\n\n\
        <{url}|{key}>: {summary}",
        approvers = mentions.approvers_group_id,
        main = mentions.main_group_id,
        url = issue.url,
        key = issue.key,
        summary = escape(&issue.summary),
    )
}

/// First reply in the thread: the key facts reviewers look for.
pub fn details(issue: &Issue) -> String {
    let planned = issue.planned_deployment.as_deref().unwrap_or("not set");
    let contents = if issue.change_contents.is_empty() {
        "not set".to_string()
    } else {
        issue.change_contents.join(", ")
    };
    format!(
        "Details for {key}\n\
         - Type: {work_type}\n\
         - Requester: {requester}\n\
         - Planned deployment: {planned}\n\
         - Change contents: {contents}\n\
         - BTS testing: {bts}\n\n\
         All board members must approve. Discussion should happen in this thread. \
         Approvals must happen directly in the ticket.",
        work_type = escape(&issue.work_type),
        requester = escape(&issue.requester),
        key = issue.key,
        planned = escape(planned),
        contents = escape(&contents),
        bts = bts_status(&issue.labels),
    )
}

/// Describe BTS testing based on the `bts-pending` / `bts-exempt` labels.
fn bts_status(labels: &[String]) -> &'static str {
    if labels.iter().any(|l| l == "bts-pending") {
        ":warning: pending (`bts-pending`): must be tested on BTS before deployment"
    } else if labels.iter().any(|l| l == "bts-exempt") {
        ":warning: exemption requested (`bts-exempt`): see justification in the ticket"
    } else {
        "tested"
    }
}

/// Escape the characters Slack treats as control characters in mrkdwn.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svab::tests::example_issue;

    fn mentions() -> Mentions {
        Mentions {
            approvers_group_id: "SAPPROVERS".into(),
            main_group_id: "SMAIN".into(),
        }
    }

    #[test]
    fn announcement_matches_template() {
        let text = announcement(&example_issue(), &mentions());
        assert_eq!(
            text,
            ":large_blue_circle: *CONSULTATION*: <!subteam^SAPPROVERS> (attn <!subteam^SMAIN>) SVAB request ready for review (:thread:).\n\n\
             <https://rubinobs.atlassian.net/browse/SVAB-15|SVAB-15>: Update ATBuilding and ATPneumatics"
        );
    }

    #[test]
    fn announcement_shows_critical_level() {
        let issue = Issue {
            level: Some(Level::Critical),
            ..example_issue()
        };
        assert!(announcement(&issue, &mentions()).starts_with(":red_circle: *CRITICAL*"));
    }

    #[test]
    fn user_text_is_escaped() {
        let issue = Issue {
            summary: "Fix <!channel> & stuff".into(),
            ..example_issue()
        };
        let text = announcement(&issue, &mentions());
        assert!(text.contains("Fix &lt;!channel&gt; &amp; stuff"));
    }

    #[test]
    fn details_for_tested_request() {
        assert_eq!(
            details(&example_issue()),
            "Details for SVAB-15\n\
             - Type: Software Update\n\
             - Requester: amanda.ibsen\n\
             - Planned deployment: 2026-10-27\n\
             - Change contents: CSC software\n\
             - BTS testing: tested\n\n\
             All board members must approve. Discussion should happen in this thread. \
             Approvals must happen directly in the ticket."
        );
    }

    #[test]
    fn details_flag_bts_labels_and_missing_fields() {
        let issue = Issue {
            labels: vec!["bts-pending".into()],
            planned_deployment: None,
            change_contents: vec![],
            ..example_issue()
        };
        let text = details(&issue);
        assert!(text.contains("BTS testing: :warning: pending (`bts-pending`)"));
        assert!(text.contains("Planned deployment: not set"));
        assert!(text.contains("Change contents: not set"));
    }
}
