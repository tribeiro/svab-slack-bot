//! Minimal Jira Cloud REST client: check our access, read an issue, and
//! read/write the issue property where the bot keeps its [`ThreadState`].
//!
//! Jira's JSON is mapped into private `Raw*` structs that mirror the API, then
//! converted into the domain [`Issue`]. Only this module knows Jira's shapes.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, bail};
use reqwest::{Response, StatusCode};
use serde::Deserialize;

use crate::config::JiraConfig;
use crate::svab::{Issue, Level, Status, ThreadState};

/// Issue property key under which the bot stores its [`ThreadState`].
const PROPERTY_KEY: &str = "svab-slack-bot";

/// Site-specific custom field IDs (found by inspecting SVAB tickets).
const FIELD_PLANNED_DEPLOYMENT: &str = "customfield_12307";
const FIELD_CHANGE_CONTENTS: &str = "customfield_12306";

/// Project permissions the bot needs: see issues, and set issue properties
/// (which Jira counts as editing the issue).
const REQUIRED_PERMISSIONS: [&str; 2] = ["BROWSE_PROJECTS", "EDIT_ISSUES"];

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

    /// Check that our credentials work and that the account has the
    /// permissions the bot needs in `project`. Returns the account's display
    /// name. Called at startup so misconfiguration fails fast and clearly.
    pub async fn verify_access(&self, project: &str) -> anyhow::Result<String> {
        let response = self
            .http
            .get(format!("{}/rest/api/3/myself", self.config.api_url))
            .basic_auth(&self.config.email, Some(&self.config.api_token))
            .send()
            .await;
        let me: RawUser = check_status(response)
            .await
            .context("GET /myself")?
            .json()
            .await
            .context("parsing /myself")?;

        let url = format!(
            "{}/rest/api/3/mypermissions?projectKey={project}&permissions={}",
            self.config.api_url,
            REQUIRED_PERMISSIONS.join(",")
        );
        let response = self
            .http
            .get(&url)
            .basic_auth(&self.config.email, Some(&self.config.api_token))
            .send()
            .await;
        let granted: RawPermissions = check_status(response)
            .await
            .context("GET /mypermissions")?
            .json()
            .await
            .context("parsing /mypermissions")?;

        let missing: Vec<&str> = REQUIRED_PERMISSIONS
            .into_iter()
            .filter(|name| {
                !granted
                    .permissions
                    .get(*name)
                    .is_some_and(|permission| permission.have_permission)
            })
            .collect();
        if !missing.is_empty() {
            bail!(
                "Jira account {} lacks permissions in project {project}: {}",
                me.display_name,
                missing.join(", ")
            );
        }
        Ok(me.display_name)
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

        let response = self
            .http
            .get(&url)
            .basic_auth(&self.config.email, Some(&self.config.api_token))
            .send()
            .await;
        let raw: RawIssue = check_status(response)
            .await
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

        let property: RawProperty = check_status(Ok(response))
            .await
            .with_context(|| format!("GET thread state of {key}"))?
            .json()
            .await
            .with_context(|| format!("parsing thread state of {key}"))?;
        Ok(Some(property.value))
    }

    /// Store (create or replace) the bot's thread state on an issue.
    pub async fn set_thread_state(&self, key: &str, state: &ThreadState) -> anyhow::Result<()> {
        let response = self
            .http
            .put(self.property_url(key))
            .basic_auth(&self.config.email, Some(&self.config.api_token))
            .json(state)
            .send()
            .await;
        check_status(response)
            .await
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

/// Turn a failed request or a non-2xx response into an error that includes
/// Jira's own explanation, e.g. "Issue does not exist or you do not have
/// permission to see it." (`reqwest`'s `error_for_status` drops the body.)
async fn check_status(response: reqwest::Result<Response>) -> anyhow::Result<Response> {
    let response = response?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<RawErrors>(&body)
        .map(|errors| errors.error_messages.join("; "))
        .ok()
        .filter(|messages| !messages.is_empty())
        .unwrap_or_else(|| body.chars().take(200).collect());

    if status == StatusCode::UNAUTHORIZED {
        bail!(
            "Jira rejected the credentials (401): {detail}. Check that SVAB_BOT_JIRA_EMAIL \
             is the Atlassian login of the account that owns SVAB_BOT_JIRA_API_TOKEN, and that \
             the token hasn't expired. Scoped (e.g. service account) tokens also need \
             SVAB_BOT_JIRA_API_URL=https://api.atlassian.com/ex/jira/<cloud-id>"
        );
    }
    bail!("Jira returned {status}: {detail}")
}

// --- Jira JSON shapes (only the fields we use) ---

/// Error body Jira sends with most 4xx responses.
#[derive(Deserialize)]
struct RawErrors {
    #[serde(rename = "errorMessages", default)]
    error_messages: Vec<String>,
}

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
struct RawPermissions {
    permissions: HashMap<String, RawPermission>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPermission {
    have_permission: bool,
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

    async fn mount_access(server: &MockServer, browse: bool, edit: bool) {
        Mock::given(path("/rest/api/3/myself"))
            .and(basic_auth("bot@example.com", "token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"displayName": "SVAB Bot"})),
            )
            .mount(server)
            .await;
        Mock::given(path("/rest/api/3/mypermissions"))
            .and(query_param("projectKey", "SVAB"))
            .and(query_param("permissions", "BROWSE_PROJECTS,EDIT_ISSUES"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "permissions": {
                    "BROWSE_PROJECTS": {"key": "BROWSE_PROJECTS", "havePermission": browse},
                    "EDIT_ISSUES": {"key": "EDIT_ISSUES", "havePermission": edit}
                }
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn verify_access_returns_account_name() {
        let server = MockServer::start().await;
        mount_access(&server, true, true).await;

        let name = client(&server).await.verify_access("SVAB").await.unwrap();
        assert_eq!(name, "SVAB Bot");
    }

    #[tokio::test]
    async fn verify_access_reports_missing_permissions() {
        let server = MockServer::start().await;
        mount_access(&server, true, false).await;

        let err = client(&server)
            .await
            .verify_access("SVAB")
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("lacks permissions in project SVAB: EDIT_ISSUES")
        );
    }

    #[tokio::test]
    async fn verify_access_reports_bad_credentials() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/api/3/myself"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = client(&server)
            .await
            .verify_access("SVAB")
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("Jira rejected the credentials"));
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
    async fn get_issue_errors_include_jira_explanation() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/api/3/issue/SVAB-15"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "errorMessages": ["Issue does not exist or you do not have permission to see it."],
                "errors": {}
            })))
            .mount(&server)
            .await;

        let err = client(&server)
            .await
            .get_issue("SVAB-15")
            .await
            .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("GET issue SVAB-15"), "{message}");
        assert!(message.contains("404"), "{message}");
        assert!(message.contains("do not have permission"), "{message}");
    }

    #[tokio::test]
    async fn unauthorized_errors_explain_what_to_check() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/api/3/issue/SVAB-15"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .mount(&server)
            .await;

        let err = client(&server)
            .await
            .get_issue("SVAB-15")
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("SVAB_BOT_JIRA_EMAIL"));
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
