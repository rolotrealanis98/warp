use super::{PrRef, parse_pr_ref};

fn octo_repo(number: u64) -> Option<PrRef> {
    Some(PrRef {
        owner: "octo".to_string(),
        repo: "repo".to_string(),
        number,
    })
}

#[test]
fn parses_pull_request_url() {
    assert_eq!(
        parse_pr_ref("https://github.com/octo/repo/pull/12"),
        octo_repo(12)
    );
}

#[test]
fn parses_url_with_trailing_tab_and_query() {
    assert_eq!(
        parse_pr_ref("https://github.com/octo/repo/pull/12/files?diff=split"),
        octo_repo(12)
    );
    assert_eq!(
        parse_pr_ref("https://github.com/octo/repo/pull/12?w=1"),
        octo_repo(12)
    );
    assert_eq!(
        parse_pr_ref("https://github.com/octo/repo/pull/12#discussion_r1"),
        octo_repo(12)
    );
}

#[test]
fn parses_url_without_scheme_and_with_whitespace() {
    assert_eq!(
        parse_pr_ref("  github.com/octo/repo/pull/7 \n"),
        octo_repo(7)
    );
}

#[test]
fn parses_short_reference() {
    assert_eq!(parse_pr_ref("octo/repo#12"), octo_repo(12));
}

#[test]
fn accepts_dots_dashes_and_underscores_in_names() {
    assert_eq!(
        parse_pr_ref("my-org/my_repo.rs#3"),
        Some(PrRef {
            owner: "my-org".to_string(),
            repo: "my_repo.rs".to_string(),
            number: 3,
        })
    );
}

#[test]
fn rejects_urls_that_are_not_pull_requests() {
    assert_eq!(parse_pr_ref("https://github.com/octo/repo/issues/12"), None);
    assert_eq!(parse_pr_ref("https://github.com/octo/repo"), None);
    assert_eq!(parse_pr_ref("https://example.com/octo/repo/pull/12"), None);
}

#[test]
fn rejects_missing_or_invalid_numbers() {
    assert_eq!(parse_pr_ref("octo/repo#"), None);
    assert_eq!(parse_pr_ref("octo/repo#0"), None);
    assert_eq!(parse_pr_ref("octo/repo#12a"), None);
    assert_eq!(parse_pr_ref("https://github.com/octo/repo/pull/abc"), None);
}

#[test]
fn rejects_names_unsafe_for_a_shell_command() {
    assert_eq!(parse_pr_ref("octo/re po#1"), None);
    assert_eq!(parse_pr_ref("octo;rm/repo#1"), None);
    assert_eq!(parse_pr_ref("/repo#1"), None);
    assert_eq!(parse_pr_ref("12"), None);
}

#[test]
fn formats_slug_and_display() {
    let pr = octo_repo(12).unwrap();

    assert_eq!(pr.slug(), "octo/repo");
    assert_eq!(pr.to_string(), "octo/repo#12");
}
