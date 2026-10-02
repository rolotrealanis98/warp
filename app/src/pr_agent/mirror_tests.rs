use std::collections::HashSet;

use ai::agent::action::{
    CommentSide, InsertReviewComment, InsertedCommentLine, InsertedCommentLocation,
};

use super::*;

const COMMENTS: &str = r#"[
  {"id": 201, "user": {"login": "Me"}, "body": "Consider a guard here.", "path": "src/login.rs",
   "line": 12, "original_line": 10, "side": "RIGHT",
   "diff_hunk": "@@ -8,3 +8,4 @@ fn login() {\n let a = 1;\n+let b = 2;\n let c = 3;",
   "html_url": "https://github.com/octo/repo/pull/12#discussion_r201",
   "updated_at": "2026-01-02T03:04:05Z"},
  {"id": 202, "user": {"login": "bob"}, "body": "Agreed", "path": "src/login.rs",
   "line": 12, "original_line": 10, "side": "RIGHT", "diff_hunk": "@@ -8,3 +8,4 @@",
   "in_reply_to_id": 201, "html_url": "", "updated_at": "2026-01-02T03:05:00Z"},
  {"id": 203, "user": {"login": "me"}, "body": "File-level note", "path": "README.md",
   "line": null, "original_line": null, "side": null, "diff_hunk": "",
   "in_reply_to_id": 201, "html_url": "", "updated_at": "2026-01-02T03:06:00Z"}
]"#;

#[test]
fn parses_a_single_page_and_concatenated_pages() {
    assert_eq!(parse_review_comments(COMMENTS).unwrap().len(), 3);
    assert_eq!(parse_review_comments("[][]").unwrap(), vec![]);
    assert_eq!(parse_review_comments("").unwrap(), vec![]);
    assert!(parse_review_comments("{\"message\": \"Not Found\"}").is_err());
}

#[test]
fn selects_only_unmirrored_comments_by_the_viewer() {
    let comments = parse_review_comments(COMMENTS).unwrap();
    let mirrored = HashSet::from([203]);

    let ids: Vec<u64> = comments_to_mirror(&comments, "me", &mirrored)
        .iter()
        .map(|comment| comment.id)
        .collect();

    assert_eq!(ids, vec![201]);
}

#[test]
fn line_comment_converts_anchored_at_its_original_line() {
    let comments = parse_review_comments(COMMENTS).unwrap();

    assert_eq!(
        comments[0].to_insert_review_comment(),
        InsertReviewComment {
            comment_id: "201".to_string(),
            author: "Me".to_string(),
            last_modified_timestamp: "2026-01-02T03:04:05Z".to_string(),
            comment_body: "Consider a guard here.".to_string(),
            parent_comment_id: None,
            comment_location: Some(InsertedCommentLocation {
                relative_file_path: "src/login.rs".to_string(),
                line: Some(InsertedCommentLine {
                    comment_line_range: 10..11,
                    diff_hunk_line_range: 10..11,
                    diff_hunk_text:
                        "@@ -8,3 +8,4 @@ fn login() {\n let a = 1;\n+let b = 2;\n let c = 3;"
                            .to_string(),
                    side: Some(CommentSide::Right),
                }),
            }),
            html_url: Some("https://github.com/octo/repo/pull/12#discussion_r201".to_string()),
        }
    );
}

#[test]
fn file_comment_reply_converts_without_line_or_url() {
    let comments = parse_review_comments(COMMENTS).unwrap();

    let converted = comments[2].to_insert_review_comment();

    assert_eq!(converted.parent_comment_id.as_deref(), Some("201"));
    assert_eq!(
        converted.comment_location,
        Some(InsertedCommentLocation {
            relative_file_path: "README.md".to_string(),
            line: None,
        })
    );
    assert_eq!(converted.html_url, None);
}

#[test]
fn review_comments_args_paginate_the_pull_request_comments() {
    let pr = PrRef {
        owner: "octo".to_string(),
        repo: "repo".to_string(),
        number: 12,
    };

    assert_eq!(
        review_comments_args(&pr),
        vec![
            "api",
            "--paginate",
            "repos/octo/repo/pulls/12/comments?per_page=100"
        ]
    );
}
