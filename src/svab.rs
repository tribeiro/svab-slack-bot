//! SVAB domain types and the decision logic.
//!
//! Nothing in here does I/O. [`decide`] looks at a ticket and what we already
//! posted about it, and returns the [`Action`]s to take; the actor carries them
//! out. Keeping the logic pure makes it easy to unit test every case.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Status of an SVAB ticket. See the workflow in the SVAB Confluence page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Status {
    Open,
    InProgress,
    Tested,
    InReview,
    Approved,
    Done,
    RolledBack,
    Cancelled,
    /// A status we don't know about (e.g. one added to the workflow later).
    Other(String),
}

impl Status {
    /// The status name as shown in Jira.
    pub fn name(&self) -> &str {
        match self {
            Self::Open => "Open",
            Self::InProgress => "In Progress",
            Self::Tested => "Tested",
            Self::InReview => "In Review",
            Self::Approved => "SVAB Approved",
            Self::Done => "Done",
            Self::RolledBack => "Rolled Back",
            Self::Cancelled => "Cancelled",
            Self::Other(name) => name,
        }
    }
}

/// Parse a Jira status name, ignoring case.
impl From<String> for Status {
    fn from(name: String) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "open" => Self::Open,
            "in progress" => Self::InProgress,
            "tested" => Self::Tested,
            "in review" => Self::InReview,
            "svab approved" => Self::Approved,
            "done" => Self::Done,
            "rolled back" => Self::RolledBack,
            "cancelled" => Self::Cancelled,
            _ => Self::Other(name),
        }
    }
}

impl From<Status> for String {
    fn from(status: Status) -> Self {
        status.name().to_string()
    }
}

/// Restriction level of a request, taken from the ticket's Jira priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Consultation,
    Critical,
}

impl Level {
    /// Map a Jira priority name ("SVAB Consultation", "SVAB Critical").
    pub fn from_priority(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "svab consultation" => Some(Self::Consultation),
            "svab critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// The parts of an SVAB ticket the bot cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub key: String,
    pub summary: String,
    pub status: Status,
    /// `None` if the priority isn't one of the SVAB levels.
    pub level: Option<Level>,
    /// Jira work type, e.g. "Software Update".
    pub work_type: String,
    /// Display name of the reporter.
    pub requester: String,
    pub labels: Vec<String>,
    /// "Planned deployment" date as written in Jira (YYYY-MM-DD).
    pub planned_deployment: Option<String>,
    /// "Change Contents" values, e.g. ["CSC software", "LOVE"].
    pub change_contents: Vec<String>,
    /// When the ticket last moved into In Review (from the Jira changelog);
    /// `None` if it never has.
    pub entered_review_at: Option<Timestamp>,
    /// Link to the ticket in the Jira web UI.
    pub url: String,
}

/// What the bot remembers about a ticket, stored on the ticket itself as a
/// Jira issue property (see `jira.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadState {
    /// Slack channel ID of the announcement.
    pub channel: String,
    /// Slack timestamp of the announcement; replies use it as `thread_ts`.
    pub thread_ts: String,
    /// Ticket status the last time we posted about it.
    pub last_status: Status,
}

/// Something the actor should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Post the review request to the channel, save the thread, then post
    /// the details in the thread.
    Announce,
}

/// Decide what to post, given the ticket and what we already posted.
pub fn decide(issue: &Issue, thread: Option<&ThreadState>) -> Vec<Action> {
    match (&issue.status, thread) {
        // First time the ticket is ready for review: announce it.
        (Status::InReview, None) => vec![Action::Announce],
        // Already announced, or not ready for review yet. Thread updates for
        // later statuses come in a later milestone.
        _ => vec![],
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// An example ticket, based on SVAB-15. Shared with other modules' tests.
    pub fn example_issue() -> Issue {
        Issue {
            key: "SVAB-15".into(),
            summary: "Update ATBuilding and ATPneumatics".into(),
            status: Status::InReview,
            level: Some(Level::Consultation),
            work_type: "Software Update".into(),
            requester: "amanda.ibsen".into(),
            labels: vec![],
            planned_deployment: Some("2026-10-27".into()),
            change_contents: vec!["CSC software".into()],
            entered_review_at: Some("2026-10-26T15:59:31.727Z".parse().unwrap()),
            url: "https://rubinobs.atlassian.net/browse/SVAB-15".into(),
        }
    }

    fn thread(last_status: Status) -> ThreadState {
        ThreadState {
            channel: "C123".into(),
            thread_ts: "1700000000.000100".into(),
            last_status,
        }
    }

    #[test]
    fn status_parses_case_insensitively_and_round_trips() {
        assert_eq!(Status::from("IN REVIEW".to_string()), Status::InReview);
        assert_eq!(Status::from("SVAB Approved".to_string()), Status::Approved);
        assert_eq!(
            Status::from("Blocked".to_string()),
            Status::Other("Blocked".into())
        );
        for status in [
            Status::InReview,
            Status::RolledBack,
            Status::Other("X".into()),
        ] {
            assert_eq!(Status::from(String::from(status.clone())), status);
        }
    }

    #[test]
    fn level_comes_from_priority() {
        assert_eq!(Level::from_priority("SVAB Critical"), Some(Level::Critical));
        assert_eq!(
            Level::from_priority("SVAB Consultation"),
            Some(Level::Consultation)
        );
        assert_eq!(Level::from_priority("Undefined"), None);
    }

    #[test]
    fn thread_state_serializes_status_by_name() {
        let json = serde_json::to_value(thread(Status::InReview)).unwrap();
        assert_eq!(json["last_status"], "In Review");
    }

    #[test]
    fn announces_when_in_review_without_thread() {
        assert_eq!(decide(&example_issue(), None), vec![Action::Announce]);
    }

    #[test]
    fn does_not_announce_twice() {
        let state = thread(Status::InReview);
        assert!(decide(&example_issue(), Some(&state)).is_empty());
    }

    #[test]
    fn ignores_tickets_not_in_review() {
        for status in [
            Status::Open,
            Status::InProgress,
            Status::Tested,
            Status::Done,
        ] {
            let issue = Issue {
                status,
                ..example_issue()
            };
            assert!(decide(&issue, None).is_empty());
        }
    }
}
