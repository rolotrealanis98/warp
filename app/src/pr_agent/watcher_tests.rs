use super::super::mirror::parse_review_comments;
use super::*;

/// `gh pr view --json` output before anything happened.
const VIEW_BEFORE: &str = r#"{
  "headRefOid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "commits": [
    {"oid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "messageHeadline": "Add login redirect"}
  ],
  "reviews": [
    {"id": "PRR_1", "author": {"login": "alice"}, "state": "COMMENTED", "body": ""}
  ],
  "comments": [
    {"id": "IC_1", "author": {"login": "alice"}, "body": "Thanks for this!"}
  ],
  "statusCheckRollup": [
    {"__typename": "CheckRun", "name": "build", "status": "IN_PROGRESS", "conclusion": ""},
    {"__typename": "StatusContext", "context": "lint", "state": "PENDING"}
  ],
  "reviewDecision": "REVIEW_REQUIRED"
}"#;

/// The same pull request after a push, new reviews and comments, and finished checks.
const VIEW_AFTER: &str = r#"{
  "headRefOid": "cccccccccccccccccccccccccccccccccccccccc",
  "commits": [
    {"oid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "messageHeadline": "Add login redirect"},
    {"oid": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "messageHeadline": "Handle expired sessions"},
    {"oid": "cccccccccccccccccccccccccccccccccccccccc", "messageHeadline": "Fix test"}
  ],
  "reviews": [
    {"id": "PRR_1", "author": {"login": "alice"}, "state": "COMMENTED", "body": ""},
    {"id": "PRR_2", "author": {"login": "bob"}, "state": "CHANGES_REQUESTED", "body": "Please add a test."},
    {"id": "PRR_3", "author": {"login": "carol"}, "state": "COMMENTED", "body": ""},
    {"id": "PRR_4", "author": {"login": "Me"}, "state": "APPROVED", "body": "LGTM"},
    {"id": "PRR_5", "author": {"login": "dave"}, "state": "PENDING", "body": "draft"}
  ],
  "comments": [
    {"id": "IC_1", "author": {"login": "alice"}, "body": "Thanks for this!"},
    {"id": "IC_2", "author": null, "body": "Ghost says hi"},
    {"id": "IC_3", "author": {"login": "me"}, "body": "Agent summary"}
  ],
  "statusCheckRollup": [
    {"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "FAILURE"},
    {"__typename": "CheckRun", "name": "docs", "status": "COMPLETED", "conclusion": "SKIPPED"},
    {"__typename": "StatusContext", "context": "lint", "state": "SUCCESS"}
  ],
  "reviewDecision": "CHANGES_REQUESTED"
}"#;

/// `gh api --paginate .../pulls/12/comments` output: two pages back to back.
const LINE_COMMENTS_AFTER: &str = r#"[
  {"id": 101, "user": {"login": "carol"}, "body": "Off by one?", "path": "src/login.rs",
   "line": 42, "original_line": 40, "side": "RIGHT", "diff_hunk": "@@ -1 +1 @@",
   "html_url": "https://github.com/octo/repo/pull/12#discussion_r101",
   "updated_at": "2026-01-02T03:04:05Z"}
][
  {"id": 102, "user": {"login": "me"}, "body": "Agent note", "path": "src/login.rs",
   "line": null, "original_line": 7, "side": "LEFT", "diff_hunk": "@@ -1 +1 @@",
   "html_url": "", "updated_at": "2026-01-02T03:04:05Z"}
]"#;

fn snapshot(view: &str, line_comments: &str) -> PrSnapshot {
    let comments = parse_review_comments(line_comments).unwrap();
    PrSnapshot::parse(view, &comments).unwrap()
}

fn pr() -> PrRef {
    PrRef {
        owner: "octo".to_string(),
        repo: "repo".to_string(),
        number: 12,
    }
}

#[test]
fn parse_summarizes_checks_and_review_decision() {
    let before = snapshot(VIEW_BEFORE, "[]");
    let after = snapshot(VIEW_AFTER, LINE_COMMENTS_AFTER);

    assert_eq!(before.checks.state(), ChecksState::Pending);
    assert_eq!(before.review_decision.as_deref(), Some("REVIEW_REQUIRED"));
    assert_eq!(
        after.checks,
        ChecksSummary {
            passed: 2,
            pending: 0,
            failed: vec!["build".to_string()],
        }
    );
    assert_eq!(after.checks.state(), ChecksState::Failing);
}

#[test]
fn parse_merges_conversation_and_line_comments() {
    let after = snapshot(VIEW_AFTER, LINE_COMMENTS_AFTER);

    assert_eq!(after.comments.len(), 5);
    assert_eq!(
        after.comments[3],
        Comment {
            id: "101".to_string(),
            author: "carol".to_string(),
            path: Some("src/login.rs".to_string()),
            line: Some(42),
            body: "Off by one?".to_string(),
        }
    );
    assert_eq!(
        after.comments[4].line,
        Some(7),
        "outdated comments keep their original line"
    );
}

#[test]
fn parse_tolerates_missing_fields_and_empty_decision() {
    let snapshot =
        PrSnapshot::parse(r#"{"headRefOid": "abc", "reviewDecision": ""}"#, &[]).unwrap();

    assert_eq!(snapshot.review_decision, None);
    assert_eq!(snapshot.checks.state(), ChecksState::NoChecks);
}

#[test]
fn diff_reports_new_commits_reviews_comments_and_finished_checks() {
    let before = snapshot(VIEW_BEFORE, "[]");
    let after = snapshot(VIEW_AFTER, LINE_COMMENTS_AFTER);

    let events = diff_snapshots(&before, &after, Some("me"));

    assert_eq!(
        events,
        vec![
            PrEvent::NewCommits {
                head: "ccccccc".to_string(),
                headlines: vec![
                    "Handle expired sessions".to_string(),
                    "Fix test".to_string()
                ],
            },
            PrEvent::NewReview {
                author: "bob".to_string(),
                state: "CHANGES_REQUESTED".to_string(),
                body: "Please add a test.".to_string(),
            },
            PrEvent::NewReviewComment {
                author: String::new(),
                path: None,
                line: None,
                body: "Ghost says hi".to_string(),
            },
            PrEvent::NewReviewComment {
                author: "carol".to_string(),
                path: Some("src/login.rs".to_string()),
                line: Some(42),
                body: "Off by one?".to_string(),
            },
            PrEvent::ChecksChanged(ChecksSummary {
                passed: 2,
                pending: 0,
                failed: vec!["build".to_string()],
            }),
        ]
    );
}

#[test]
fn diff_without_viewer_keeps_everyone() {
    let before = snapshot(VIEW_BEFORE, "[]");
    let after = snapshot(VIEW_AFTER, LINE_COMMENTS_AFTER);

    let events = diff_snapshots(&before, &after, None);

    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, PrEvent::NewReview { author, .. } if author == "Me"))
            .count(),
        1
    );
}

#[test]
fn diff_of_identical_snapshots_is_empty() {
    let after = snapshot(VIEW_AFTER, LINE_COMMENTS_AFTER);

    assert_eq!(diff_snapshots(&after, &after.clone(), Some("me")), vec![]);
}

#[test]
fn diff_reports_force_push_as_moved_head() {
    let before = PrSnapshot {
        head_oid: "1111111aaaa".to_string(),
        commits: vec![Commit {
            oid: "1111111aaaa".to_string(),
            headline: "One".to_string(),
        }],
        ..Default::default()
    };
    let after = PrSnapshot {
        head_oid: "2222222bbbb".to_string(),
        commits: vec![Commit {
            oid: "2222222bbbb".to_string(),
            headline: "One, reworded".to_string(),
        }],
        ..Default::default()
    };

    assert_eq!(
        diff_snapshots(&before, &after, None),
        vec![PrEvent::NewCommits {
            head: "2222222".to_string(),
            headlines: vec!["One, reworded".to_string()],
        }]
    );
}

#[test]
fn diff_ignores_checks_going_back_to_pending() {
    let passing = PrSnapshot {
        checks: ChecksSummary {
            passed: 3,
            ..Default::default()
        },
        ..Default::default()
    };
    let pending = PrSnapshot {
        checks: ChecksSummary {
            passed: 1,
            pending: 2,
            ..Default::default()
        },
        ..Default::default()
    };

    assert_eq!(diff_snapshots(&passing, &pending, None), vec![]);
    assert_eq!(
        diff_snapshots(&pending, &passing, None),
        vec![PrEvent::ChecksChanged(ChecksSummary {
            passed: 3,
            ..Default::default()
        })]
    );
}

#[test]
fn format_lists_each_event_with_quoted_bodies() {
    let events = vec![
        PrEvent::NewCommits {
            head: "ccccccc".to_string(),
            headlines: vec![
                "Handle expired sessions".to_string(),
                "Fix test".to_string(),
            ],
        },
        PrEvent::NewReviewComment {
            author: "carol".to_string(),
            path: Some("src/login.rs".to_string()),
            line: Some(42),
            body: "Off by one?\nCheck the loop.".to_string(),
        },
        PrEvent::NewReview {
            author: "bob".to_string(),
            state: "CHANGES_REQUESTED".to_string(),
            body: "Please add a test.".to_string(),
        },
        PrEvent::ChecksChanged(ChecksSummary {
            passed: 2,
            pending: 1,
            failed: vec!["build".to_string(), "lint".to_string()],
        }),
    ];

    assert_eq!(
        format_events(&pr(), &events),
        "Update on pull request octo/repo#12:\n\
         - 2 new commits (head ccccccc): Handle expired sessions; Fix test\n\
         - New comment from carol on src/login.rs:42:\n  \
         > Off by one?\n  \
         > Check the loop.\n\
         - bob reviewed: changes requested.\n  \
         > Please add a test.\n\
         - Checks failed: build, lint (2 passed, 1 pending)."
    );
}

#[test]
fn format_handles_moved_head_passing_checks_and_conversation_comments() {
    let events = vec![
        PrEvent::NewCommits {
            head: "2222222".to_string(),
            headlines: vec![],
        },
        PrEvent::NewReviewComment {
            author: "alice".to_string(),
            path: None,
            line: None,
            body: "Nice".to_string(),
        },
        PrEvent::ChecksChanged(ChecksSummary {
            passed: 4,
            ..Default::default()
        }),
    ];

    assert_eq!(
        format_events(&pr(), &events),
        "Update on pull request octo/repo#12:\n\
         - The branch head moved to 2222222.\n\
         - New comment from alice:\n  \
         > Nice\n\
         - All 4 checks passed."
    );
}

#[test]
fn format_cuts_long_bodies() {
    let events = vec![PrEvent::NewReview {
        author: "bob".to_string(),
        state: "COMMENTED".to_string(),
        body: "x".repeat(2000),
    }];

    let message = format_events(&pr(), &events);

    assert!(message.ends_with(&format!("> {}…", "x".repeat(1500))));
}
