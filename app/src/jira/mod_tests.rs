use std::collections::HashMap;
use std::path::PathBuf;

use super::*;
use crate::terminal::CLIAgent;

fn draft() -> TaskAgentRequest {
    TaskAgentRequest::with_cli(PathBuf::from("/work/octo/repo"), CLIAgent::Claude)
}

fn mapping() -> HashMap<String, String> {
    HashMap::from([
        ("Bug".to_string(), "fix".to_string()),
        ("Story".to_string(), "feat".to_string()),
    ])
}

#[test]
fn plain_query_is_fuzzy() {
    assert_eq!(parse_query(" login "), PickerQuery::Fuzzy("login"));
}

#[test]
fn jql_prefix_switches_to_a_server_query_in_any_case() {
    assert_eq!(
        parse_query("JQL: project = EXAMPLE ORDER BY created"),
        PickerQuery::Jql("project = EXAMPLE ORDER BY created")
    );
    assert_eq!(parse_query("jql:"), PickerQuery::Jql(""));
}

#[test]
fn jql_text_without_the_colon_stays_fuzzy() {
    assert_eq!(
        parse_query("jql project"),
        PickerQuery::Fuzzy("jql project")
    );
}

#[test]
fn fuzzy_filter_ranks_matches_and_drops_misses() {
    let labels = [
        "EXAMPLE-1 Update docs",
        "EXAMPLE-2 Fix login redirect",
        "EXAMPLE-3 Login page copy",
    ];

    let matches = fuzzy_filter(labels.into_iter(), "login redirect");

    assert_eq!(matches, vec![1]);
}

#[test]
fn fuzzy_filter_keeps_order_for_an_empty_query() {
    let labels = ["EXAMPLE-2 b", "EXAMPLE-1 a"];

    assert_eq!(fuzzy_filter(labels.into_iter(), ""), vec![0, 1]);
}

#[test]
fn issue_keys_are_recognized() {
    assert!(looks_like_issue_key("EXAMPLE-123"));
    assert!(looks_like_issue_key("AB2_X-7"));
    assert!(!looks_like_issue_key("123-4"));
    assert!(!looks_like_issue_key("EXAMPLE-"));
    assert!(!looks_like_issue_key("fix login"));
}

#[test]
fn scoped_jql_inserts_projects_before_order_by() {
    let jql = scoped_jql(
        "assignee = currentUser() ORDER BY updated DESC",
        &["EXAMPLE".to_string(), " OTHER ".to_string()],
    );

    assert_eq!(
        jql,
        "project in (EXAMPLE, OTHER) AND (assignee = currentUser()) ORDER BY updated DESC"
    );
}

#[test]
fn scoped_jql_without_projects_is_unchanged() {
    assert_eq!(scoped_jql(" status = Open ", &[]), "status = Open");
}

#[test]
fn branch_type_maps_issue_types_case_insensitively() {
    assert_eq!(branch_type("Bug", &mapping()), "fix");
    assert_eq!(branch_type("bug", &mapping()), "fix");
}

#[test]
fn branch_type_defaults_to_feat() {
    assert_eq!(branch_type("Epic", &mapping()), "feat");
}

#[test]
fn task_request_carries_issue_fields_and_browse_url() {
    let issue = Issue {
        key: "EXAMPLE-123".into(),
        summary: "Fix login redirect".into(),
        issue_type: "Bug".into(),
        description: Some("Steps to reproduce".into()),
        ..Issue::default()
    };

    let request = task_request(&issue, "example.atlassian.net", &mapping(), draft());

    assert_eq!(
        request,
        TaskAgentRequest {
            title: "Fix login redirect".into(),
            key: Some("EXAMPLE-123".into()),
            body: Some("Steps to reproduce".into()),
            url: Some("https://example.atlassian.net/browse/EXAMPLE-123".into()),
            branch_type: Some("fix".into()),
            ..draft()
        }
    );
}
