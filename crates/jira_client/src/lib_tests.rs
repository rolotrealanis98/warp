use serde_json::json;

use super::*;

#[test]
fn site_base_url_adds_https_to_a_bare_host() {
    let url = site_base_url("example.atlassian.net/").unwrap();

    assert_eq!(url.as_str(), "https://example.atlassian.net/");
}

#[test]
fn site_base_url_rejects_http_and_empty_input() {
    assert!(matches!(
        site_base_url("http://example.atlassian.net"),
        Err(JiraError::InvalidSiteUrl)
    ));
    assert!(matches!(
        site_base_url("  "),
        Err(JiraError::InvalidSiteUrl)
    ));
}

#[test]
fn browse_url_appends_the_key() {
    assert_eq!(
        browse_url("https://example.atlassian.net/", "EXAMPLE-123").unwrap(),
        "https://example.atlassian.net/browse/EXAMPLE-123"
    );
}

#[test]
fn api_urls_keep_a_site_path_and_encode_segments() {
    let client =
        JiraClient::new("https://example.atlassian.net/jira", "me@example.com", "t").unwrap();

    assert_eq!(
        client.url(&["issue", "EXAMPLE 1", "comment"]).as_str(),
        "https://example.atlassian.net/jira/rest/api/3/issue/EXAMPLE%201/comment"
    );
}

#[test]
fn parse_search_reads_list_fields() {
    let response = json!({
        "issues": [{
            "key": "EXAMPLE-123",
            "fields": {
                "summary": "Fix login redirect",
                "status": { "name": "In Progress" },
                "issuetype": { "name": "Bug" },
                "assignee": { "displayName": "Example User" },
                "labels": ["frontend"]
            }
        }, {
            "key": "EXAMPLE-124",
            "fields": { "summary": "Unassigned task", "assignee": null }
        }],
        "isLast": true
    });

    let issues = parse_search(&response);

    assert_eq!(
        issues,
        vec![
            Issue {
                key: "EXAMPLE-123".into(),
                summary: "Fix login redirect".into(),
                status: "In Progress".into(),
                issue_type: "Bug".into(),
                assignee: Some("Example User".into()),
                labels: vec!["frontend".into()],
                description: None,
                sprint: None,
            },
            Issue {
                key: "EXAMPLE-124".into(),
                summary: "Unassigned task".into(),
                ..Issue::default()
            },
        ]
    );
}

#[test]
fn parse_issue_converts_description_and_finds_the_active_sprint() {
    let response = json!({
        "key": "EXAMPLE-123",
        "names": { "customfield_10020": "Sprint", "summary": "Summary" },
        "fields": {
            "summary": "Fix login redirect",
            "description": {
                "type": "doc", "version": 1,
                "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": "Steps" }] }]
            },
            "customfield_10020": [
                { "name": "Sprint 1", "state": "closed" },
                { "name": "Sprint 2", "state": "active" },
                { "name": "Sprint 3", "state": "future" }
            ]
        }
    });

    let issue = parse_issue(&response).unwrap();

    assert_eq!(issue.description.as_deref(), Some("Steps"));
    assert_eq!(issue.sprint.as_deref(), Some("Sprint 2"));
}

#[test]
fn parse_issue_without_key_is_none() {
    assert_eq!(parse_issue(&json!({ "fields": {} })), None);
}

#[test]
fn parse_transitions_reads_target_status() {
    let response = json!({
        "transitions": [{ "id": "21", "name": "Start progress", "to": { "name": "In Progress" } }]
    });

    assert_eq!(
        parse_transitions(&response),
        vec![Transition {
            id: "21".into(),
            name: "Start progress".into(),
            to_status: "In Progress".into(),
        }]
    );
}

#[test]
fn error_message_joins_messages_and_field_errors() {
    let body = r#"{"errorMessages":["Error in the JQL Query"],"errors":{"jql":"bad clause"}}"#;

    assert_eq!(
        error_message(body),
        "Error in the JQL Query; jql: bad clause"
    );
}

#[test]
fn error_message_falls_back_for_non_json_bodies() {
    assert_eq!(error_message("<html>"), "Jira rejected the request");
}
