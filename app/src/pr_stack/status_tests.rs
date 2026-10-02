use super::{ChecksState, PrStatus, parse_pr_list};

#[test]
fn parses_pr_list_keyed_by_head_with_newest_first() {
    let json = r#"[
        {"number": 12, "url": "https://github.com/octo/repo/pull/12", "state": "OPEN",
         "baseRefName": "a", "headRefName": "b", "reviewDecision": "APPROVED",
         "statusCheckRollup": [
            {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SUCCESS"},
            {"__typename": "StatusContext", "state": "SUCCESS"}
         ], "mergedAt": null},
        {"number": 7, "url": "https://github.com/octo/repo/pull/7", "state": "CLOSED",
         "baseRefName": "main", "headRefName": "b", "reviewDecision": "",
         "statusCheckRollup": [], "mergedAt": null}
    ]"#;

    let prs = parse_pr_list(json).unwrap();

    assert_eq!(
        prs.get("b"),
        Some(&PrStatus {
            number: 12,
            url: "https://github.com/octo/repo/pull/12".to_string(),
            state: "OPEN".to_string(),
            base: "a".to_string(),
            review: Some("APPROVED".to_string()),
            checks: Some(ChecksState::Passing),
        })
    );
}

#[test]
fn failing_check_outranks_pending_ones() {
    let json = r#"[
        {"number": 3, "url": "u", "state": "OPEN", "baseRefName": "main", "headRefName": "a",
         "reviewDecision": null, "mergedAt": null, "statusCheckRollup": [
            {"__typename": "CheckRun", "status": "IN_PROGRESS", "conclusion": ""},
            {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "FAILURE"}
         ]},
        {"number": 4, "url": "u", "state": "OPEN", "baseRefName": "main", "headRefName": "c",
         "reviewDecision": null, "mergedAt": null, "statusCheckRollup": [
            {"__typename": "CheckRun", "status": "IN_PROGRESS", "conclusion": ""},
            {"__typename": "StatusContext", "state": "SUCCESS"}
         ]}
    ]"#;

    let prs = parse_pr_list(json).unwrap();

    assert_eq!(prs["a"].checks, Some(ChecksState::Failing));
    assert_eq!(prs["c"].checks, Some(ChecksState::Pending));
}
