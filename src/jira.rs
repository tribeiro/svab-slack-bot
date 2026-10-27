//! Minimal Jira Cloud REST client: read an issue, and read/write the issue
//! property where the bot keeps its [`ThreadState`].
//!
//! Jira's JSON is mapped into private `Raw*` structs that mirror the API, then
//! converted into the domain [`Issue`]. Only this module knows Jira's shapes.

use std::time::Duration;

use anyhow::Context;
use reqwest::StatusCode;
use serde::Deserialize;

use crate::config::JiraConfig;
use crate::svab::{Issue, Level, Status, ThreadState};

/// Issue property key under which the bot stores its [`ThreadState`].
const PROPERTY_KEY: &str = "svab-slack-bot";

/// Site-specific custom field IDs (found by inspecting SVAB tickets).
const FIELD_PLANNED_DEPLOYMENT: &str = "customfield_12307";
const FIELD_CHANGE_CONTENTS: &str = "customfield_12306";

/// Don't let a slow Jira block the actor forever.
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct JiraClient {
    http: reqwest::Client,
    config: JiraConfig,
}

impl JiraClient {
    pub fn new(config: JiraConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder().timeout(TIMEOUT).build()?;
        Ok(Self { http, config })
    }

    /// Fetch an issue and convert it to the domain type.
    pub async fn get_issue(&self, key: &str) -> anyhow::Result<Issue> {
        let fields = [
            "summary",
            "status",
            "priority",
            "issuetype",
            "reporter",
            "labels",
            FIELD_PLANNED_DEPLOYMENT,
            FIELD_CHANGE_CONTENTS,
        ]
        .join(",");
        let url = format!(
            "{}/rest/api/3/issue/{key}?fields={fields}",
            self.config.api_url
        );

        let raw: RawIssue = self
            .http
            .get(&url)
            .basic_auth(&self.config.email, Some(&self.config.api_token))
            .send()
            .await
            .with_context(|| format!("GET issue {key}"))?
            .error_for_status()
            .with_context(|| format!("GET issue {key}"))?
            .json()
            .await
            .with_context(|| format!("parsing issue {key}"))?;

        Ok(raw.into_issue(&self.config.site_url))
    }

    /// Read the bot's thread state for an issue; `None` if never stored.
    pub async fn get_thread_state(&self, key: &str) -> anyhow::Result<Option<ThreadState>> {
        let response = self
            .http
            .get(self.property_url(key))
            .basic_auth(&self.config.email, Some(&self.config.api_token))
            .send()
            .await
            .with_context(|| format!("GET thread state of {key}"))?;

        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }

        let property: RawProperty = response
            .error_for_status()
            .with_context(|| format!("GET thread state of {key}"))?
            .json()
            .await
            .with_context(|| format!("parsing thread state of {key}"))?;
        Ok(Some(property.value))
    }

    /// Store (create or replace) the bot's thread state on an issue.
    pub async fn set_thread_state(&self, key: &str, state: &ThreadState) -> anyhow::Result<()> {
        self.http
            .put(self.property_url(key))
            .basic_auth(&self.config.email, Some(&self.config.api_token))
            .json(state)
            .send()
            .await
            .with_context(|| format!("PUT thread state of {key}"))?
            .error_for_status()
            .with_context(|| format!("PUT thread state of {key}"))?;
        Ok(())
    }

    fn property_url(&self, key: &str) -> String {
        format!(
            "{}/rest/api/3/issue/{key}/properties/{PROPERTY_KEY}",
            self.config.api_url
        )
    }
}

// --- Jira JSON shapes (only the fields we use) ---

#[derive(Deserialize)]
struct RawIssue {
    key: String,
    fields: RawFields,
}

#[derive(Deserialize)]
struct RawFields {
    summary: String,
    status: RawNamed,
    priority: Option<RawNamed>,
    issuetype: RawNamed,
    reporter: Option<RawUser>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(rename = "customfield_12307")]
    planned_deployment: Option<String>,
    #[serde(rename = "customfield_12306")]
    change_contents: Option<Vec<RawOption>>,
}

#[derive(Deserialize)]
struct RawNamed {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUser {
    display_name: String,
}

#[derive(Deserialize)]
struct RawOption {
    value: String,
}

#[derive(Deserialize)]
struct RawProperty {
    value: ThreadState,
}

impl RawIssue {
    fn into_issue(self, site_url: &str) -> Issue {
        let fields = self.fields;
        Issue {
            url: format!("{site_url}/browse/{}", self.key),
            key: self.key,
            summary: fields.summary,
            status: Status::from(fields.status.name),
            level: fields
                .priority
                .and_then(|priority| Level::from_priority(&priority.name)),
            work_type: fields.issuetype.name,
            requester: fields
                .reporter
                .map_or_else(|| "unknown".to_string(), |user| user.display_name),
            labels: fields.labels,
            planned_deployment: fields.planned_deployment,
            change_contents: fields
                .change_contents
                .unwrap_or_default()
                .into_iter()
                .map(|option| option.value)
                .collect(),
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{basic_auth, body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::svab::tests::example_issue;

    /// Trimmed-down copy of the real SVAB-15 response.
    pub fn svab_15_json() -> serde_json::Value {
        json!({
            "key": "SVAB-15",
            "fields": {
                "summary": "Update ATBuilding and ATPneumatics",
                "status": {"name": "In Review", "id": "10014"},
                "priority": {"name": "SVAB Consultation", "id": "11093"},
                "issuetype": {"name": "Software Update", "id": "12036"},
                "reporter": {"displayName": "amanda.ibsen", "accountId": "x"},
                "labels": [],
                "customfield_12307": "2026-10-27",
                "customfield_12306": [{"value": "CSC software", "id": "13166"}]
            }
        })
    }

    async fn client(server: &MockServer) -> JiraClient {
        JiraClient::new(JiraConfig {
            site_url: "https://rubinobs.atlassian.net".into(),
            api_url: server.uri(),
            email: "bot@example.com".into(),
            api_token: "token".into(),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn get_issue_maps_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/SVAB-15"))
            .and(query_param("fields", "summary,status,priority,issuetype,reporter,labels,customfield_12307,customfield_12306"))
            .and(basic_auth("bot@example.com", "token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(svab_15_json()))
            .mount(&server)
            .await;

        let issue = client(&server).await.get_issue("SVAB-15").await.unwrap();
        assert_eq!(issue, example_issue());
    }

    #[tokio::test]
    async fn get_issue_tolerates_missing_optional_fields() {
        let server = MockServer::start().await;
        let mut body = svab_15_json();
        body["fields"]["priority"] = json!(null);
        body["fields"]["reporter"] = json!(null);
        body["fields"]["customfield_12307"] = json!(null);
        body["fields"]["customfield_12306"] = json!(null);
        Mock::given(path("/rest/api/3/issue/SVAB-15"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let issue = client(&server).await.get_issue("SVAB-15").await.unwrap();
        assert_eq!(issue.level, None);
        assert_eq!(issue.requester, "unknown");
        assert_eq!(issue.planned_deployment, None);
        assert!(issue.change_contents.is_empty());
    }

    #[tokio::test]
    async fn get_issue_reports_http_errors() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/api/3/issue/SVAB-15"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        assert!(client(&server).await.get_issue("SVAB-15").await.is_err());
    }

    #[tokio::test]
    async fn thread_state_missing_is_none() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/api/3/issue/SVAB-15/properties/svab-slack-bot"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let state = client(&server).await.get_thread_state("SVAB-15").await;
        assert_eq!(state.unwrap(), None);
    }

    #[tokio::test]
    async fn thread_state_round_trip() {
        let server = MockServer::start().await;
        let state = ThreadState {
            channel: "C123".into(),
            thread_ts: "1700000000.000100".into(),
            last_status: Status::InReview,
        };
        let stored = json!({"channel": "C123", "thread_ts": "1700000000.000100", "last_status": "In Review"});

        Mock::given(method("PUT"))
            .and(path("/rest/api/3/issue/SVAB-15/properties/svab-slack-bot"))
            .and(body_json(&stored))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/SVAB-15/properties/svab-slack-bot"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"key": "svab-slack-bot", "value": stored})),
            )
            .mount(&server)
            .await;

        let jira = client(&server).await;
        jira.set_thread_state("SVAB-15", &state).await.unwrap();
        assert_eq!(jira.get_thread_state("SVAB-15").await.unwrap(), Some(state));
    }
}
