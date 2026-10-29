//! Minimal Jira Cloud REST client: check our access, read an issue, find
//! recently updated issues, and read/write the issue property where the bot
//! keeps its [`ThreadState`].
//!
//! Jira's JSON is mapped into private `Raw*` structs that mirror the API, then
//! converted into the domain [`Issue`]. Only this module knows Jira's shapes.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, bail};
use jiff::Timestamp;
use reqwest::{Response, StatusCode};
use serde::{Deserialize, Serialize};

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

/// Results per search page (Jira's maximum is 100 when few fields are asked).
const SEARCH_PAGE_SIZE: u32 = 100;
/// Stop paginating after this many pages, in case Jira keeps returning tokens.
const MAX_SEARCH_PAGES: usize = 20;

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

    /// Keys of the issues in `project` updated within the last
    /// `lookback_hours`, oldest update first.
    pub async fn search_recently_updated(
        &self,
        project: &str,
        lookback_hours: u32,
    ) -> anyhow::Result<Vec<String>> {
        // A relative date ("-72h") avoids any time zone ambiguity in JQL.
        let jql = format!(
            "project = \"{project}\" AND updated >= \"-{lookback_hours}h\" ORDER BY updated ASC"
        );
        let url = format!("{}/rest/api/3/search/jql", self.config.api_url);

        let mut keys = Vec::new();
        let mut next_page_token: Option<String> = None;
        for _ in 0..MAX_SEARCH_PAGES {
            let request = SearchRequest {
                jql: &jql,
                // Only the key is needed; ask for one cheap field.
                fields: &["status"],
                max_results: SEARCH_PAGE_SIZE,
                next_page_token: next_page_token.as_deref(),
            };
            let response = self
                .http
                .post(&url)
                .basic_auth(&self.config.email, Some(&self.config.api_token))
                .json(&request)
                .send()
                .await;
            let page: RawSearchPage = check_status(response)
                .await
                .context("searching recently updated issues")?
                .json()
                .await
                .context("parsing search results")?;

            keys.extend(page.issues.into_iter().map(|issue| issue.key));
            match page.next_page_token {
                Some(token) if !page.is_last => next_page_token = Some(token),
                _ => return Ok(keys),
            }
        }
        bail!("search returned more than {MAX_SEARCH_PAGES} pages; is the lookback too long?")
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
            "{}/rest/api/3/issue/{key}?fields={fields}&expand=changelog",
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchRequest<'a> {
    jql: &'a str,
    fields: &'a [&'a str],
    max_results: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_page_token: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSearchPage {
    issues: Vec<RawSearchHit>,
    next_page_token: Option<String>,
    #[serde(default)]
    is_last: bool,
}

#[derive(Deserialize)]
struct RawSearchHit {
    key: String,
}

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
    /// Present because we ask for `expand=changelog`. Jira includes the most
    /// recent 100 entries, newest first; SVAB tickets have far fewer.
    #[serde(default)]
    changelog: RawChangelog,
}

#[derive(Deserialize, Default)]
struct RawChangelog {
    histories: Vec<RawHistory>,
}

/// One edit of the ticket, possibly changing several fields at once.
#[derive(Deserialize)]
struct RawHistory {
    created: Timestamp,
    items: Vec<RawChangeItem>,
}

#[derive(Deserialize)]
struct RawChangeItem {
    #[serde(rename = "fieldId")]
    field_id: Option<String>,
    /// New value as displayed, e.g. the new status name.
    #[serde(rename = "toString")]
    to_name: Option<String>,
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
        let entered_review_at = self.changelog.last_entered(&Status::InReview);
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
            entered_review_at,
        }
    }
}

impl RawChangelog {
    /// When the ticket most recently moved into `status`, if ever.
    fn last_entered(&self, status: &Status) -> Option<Timestamp> {
        self.histories
            .iter()
            .filter(|history| {
                history.items.iter().any(|item| {
                    item.field_id.as_deref() == Some("status")
                        && item
                            .to_name
                            .as_ref()
                            .is_some_and(|name| Status::from(name.clone()) == *status)
                })
            })
            .map(|history| history.created)
            .max()
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{basic_auth, body_json, body_partial_json, method, path, query_param};
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
            },
            "changelog": {
                "startAt": 0, "maxResults": 2, "total": 2,
                "histories": [
                    {
                        "created": "2026-10-26T08:59:31.727-0700",
                        "items": [{"field": "status", "fieldId": "status",
                                   "fromString": "Tested", "toString": "In Review"}]
                    },
                    {
                        "created": "2026-10-26T08:58:29.564-0700",
                        "items": [{"field": "Change Contents", "fieldId": "customfield_12306",
                                   "toString": "CSC software"}]
                    }
                ]
            }
        })
    }

    /// Changelog with the given (time, new status) entries, newest first.
    fn changelog(entries: &[(&str, &str)]) -> RawChangelog {
        let histories = entries
            .iter()
            .map(|(created, status)| {
                json!({"created": created, "items": [{"fieldId": "status", "toString": status}]})
            })
            .collect::<Vec<_>>();
        serde_json::from_value(json!({ "histories": histories })).unwrap()
    }

    #[test]
    fn last_entered_review_uses_most_recent_entry() {
        // Reviewed, reworked, then resubmitted.
        let log = changelog(&[
            ("2026-10-25T10:00:00.000-0700", "In Review"),
            ("2026-10-24T10:00:00.000-0700", "In Progress"),
            ("2026-10-22T10:00:00.000-0700", "In Review"),
        ]);
        assert_eq!(
            log.last_entered(&Status::InReview),
            Some("2026-10-25T17:00:00Z".parse().unwrap())
        );
    }

    #[test]
    fn last_entered_review_is_none_if_never_reviewed() {
        let log = changelog(&[("2026-10-24T10:00:00.000-0700", "In Progress")]);
        assert_eq!(log.last_entered(&Status::InReview), None);
        assert_eq!(
            RawChangelog::default().last_entered(&Status::InReview),
            None
        );
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
    async fn search_follows_pagination() {
        let server = MockServer::start().await;
        let jql = "project = \"SVAB\" AND updated >= \"-72h\" ORDER BY updated ASC";
        // Mounted first so it wins for the second request, which carries the token.
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search/jql"))
            .and(body_partial_json(
                json!({"jql": jql, "nextPageToken": "page-2"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "issues": [{"id": "3", "key": "SVAB-3"}],
                "isLast": true
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search/jql"))
            .and(body_partial_json(json!({"jql": jql, "maxResults": 100})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "issues": [{"id": "1", "key": "SVAB-1"}, {"id": "2", "key": "SVAB-2"}],
                "nextPageToken": "page-2",
                "isLast": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let keys = client(&server)
            .await
            .search_recently_updated("SVAB", 72)
            .await
            .unwrap();
        assert_eq!(keys, ["SVAB-1", "SVAB-2", "SVAB-3"]);
    }

    #[tokio::test]
    async fn search_with_no_results() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"issues": [], "isLast": true})),
            )
            .mount(&server)
            .await;

        let keys = client(&server)
            .await
            .search_recently_updated("SVAB", 72)
            .await;
        assert!(keys.unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_issue_maps_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/SVAB-15"))
            .and(query_param("fields", "summary,status,priority,issuetype,reporter,labels,customfield_12307,customfield_12306"))
            .and(query_param("expand", "changelog"))
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
