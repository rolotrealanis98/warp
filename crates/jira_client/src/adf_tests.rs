use serde_json::json;

use super::{adf_to_markdown, text_to_adf};

fn doc(content: serde_json::Value) -> serde_json::Value {
    json!({ "type": "doc", "version": 1, "content": content })
}

fn text(value: &str) -> serde_json::Value {
    json!({ "type": "text", "text": value })
}

fn paragraph(value: &str) -> serde_json::Value {
    json!({ "type": "paragraph", "content": [text(value)] })
}

fn list_item(value: &str) -> serde_json::Value {
    json!({ "type": "listItem", "content": [paragraph(value)] })
}

#[test]
fn paragraphs_are_separated_by_blank_lines() {
    let adf = doc(json!([paragraph("First."), paragraph("Second.")]));

    assert_eq!(adf_to_markdown(&adf), "First.\n\nSecond.");
}

#[test]
fn headings_use_their_level() {
    let adf = doc(json!([
        { "type": "heading", "attrs": { "level": 2 }, "content": [text("Acceptance criteria")] },
        paragraph("Body"),
    ]));

    assert_eq!(adf_to_markdown(&adf), "## Acceptance criteria\n\nBody");
}

#[test]
fn bullet_list_renders_dash_items() {
    let adf = doc(json!([
        { "type": "bulletList", "content": [list_item("one"), list_item("two")] }
    ]));

    assert_eq!(adf_to_markdown(&adf), "- one\n- two");
}

#[test]
fn ordered_list_starts_at_its_order_attribute() {
    let adf = doc(json!([
        { "type": "orderedList", "attrs": { "order": 3 }, "content": [list_item("three"), list_item("four")] }
    ]));

    assert_eq!(adf_to_markdown(&adf), "3. three\n4. four");
}

#[test]
fn nested_lists_are_indented_under_their_item() {
    let adf = doc(json!([
        { "type": "bulletList", "content": [
            { "type": "listItem", "content": [
                paragraph("parent"),
                { "type": "orderedList", "content": [
                    list_item("child one"),
                    { "type": "listItem", "content": [
                        paragraph("child two"),
                        { "type": "bulletList", "content": [list_item("grandchild")] }
                    ]}
                ]}
            ]},
            list_item("sibling")
        ]}
    ]));

    assert_eq!(
        adf_to_markdown(&adf),
        "- parent\n  1. child one\n  2. child two\n     - grandchild\n- sibling"
    );
}

#[test]
fn task_list_renders_checkboxes() {
    let adf = doc(json!([
        { "type": "taskList", "content": [
            { "type": "taskItem", "attrs": { "state": "DONE" }, "content": [text("done")] },
            { "type": "taskItem", "attrs": { "state": "TODO" }, "content": [text("todo")] }
        ]}
    ]));

    assert_eq!(adf_to_markdown(&adf), "- [x] done\n- [ ] todo");
}

#[test]
fn code_block_keeps_language_and_text() {
    let adf = doc(json!([
        { "type": "codeBlock", "attrs": { "language": "rust" }, "content": [text("fn main() {}\n")] }
    ]));

    assert_eq!(adf_to_markdown(&adf), "```rust\nfn main() {}\n```");
}

#[test]
fn inline_marks_render_code_bold_italic_and_links() {
    let adf = doc(json!([
        { "type": "paragraph", "content": [
            { "type": "text", "text": "Run " },
            { "type": "text", "text": "cargo test", "marks": [{ "type": "code" }] },
            { "type": "text", "text": ", see " },
            { "type": "text", "text": "docs", "marks": [{ "type": "link", "attrs": { "href": "https://example.com/docs" } }] },
            { "type": "text", "text": " and " },
            { "type": "text", "text": "this", "marks": [{ "type": "strong" }, { "type": "em" }] },
        ]}
    ]));

    assert_eq!(
        adf_to_markdown(&adf),
        "Run `cargo test`, see [docs](https://example.com/docs) and ***this***"
    );
}

#[test]
fn mentions_render_as_at_names() {
    let adf = doc(json!([
        { "type": "paragraph", "content": [
            { "type": "text", "text": "Ask " },
            { "type": "mention", "attrs": { "id": "abc", "text": "@Example User" } },
            { "type": "text", "text": " or " },
            { "type": "mention", "attrs": { "id": "def", "text": "Other User" } },
        ]}
    ]));

    assert_eq!(adf_to_markdown(&adf), "Ask @Example User or @Other User");
}

#[test]
fn hard_breaks_become_newlines() {
    let adf = doc(json!([
        { "type": "paragraph", "content": [text("line one"), { "type": "hardBreak" }, text("line two")] }
    ]));

    assert_eq!(adf_to_markdown(&adf), "line one\nline two");
}

#[test]
fn blockquote_prefixes_each_line() {
    let adf = doc(json!([
        { "type": "blockquote", "content": [paragraph("quoted"), paragraph("more")] }
    ]));

    assert_eq!(adf_to_markdown(&adf), "> quoted\n>\n> more");
}

#[test]
fn table_renders_header_separator() {
    let cell = |value: &str| json!({ "type": "tableCell", "content": [paragraph(value)] });
    let adf = doc(json!([
        { "type": "table", "content": [
            { "type": "tableRow", "content": [cell("Name"), cell("Value")] },
            { "type": "tableRow", "content": [cell("a"), cell("b|c")] }
        ]}
    ]));

    assert_eq!(
        adf_to_markdown(&adf),
        "| Name | Value |\n| --- | --- |\n| a | b\\|c |"
    );
}

#[test]
fn unknown_nodes_fall_back_to_their_children() {
    let adf = doc(json!([
        { "type": "panel", "attrs": { "panelType": "info" }, "content": [paragraph("inside a panel")] }
    ]));

    assert_eq!(adf_to_markdown(&adf), "inside a panel");
}

#[test]
fn empty_document_is_empty() {
    assert_eq!(adf_to_markdown(&doc(json!([]))), "");
}

#[test]
fn text_to_adf_splits_paragraphs_and_hard_breaks() {
    let adf = text_to_adf("First line\nsecond line\n\n\nNext paragraph");

    assert_eq!(
        adf,
        json!({ "type": "doc", "version": 1, "content": [
            { "type": "paragraph", "content": [
                { "type": "text", "text": "First line" },
                { "type": "hardBreak" },
                { "type": "text", "text": "second line" }
            ]},
            { "type": "paragraph", "content": [{ "type": "text", "text": "Next paragraph" }] }
        ]})
    );
}

#[test]
fn text_to_adf_round_trips_through_markdown() {
    assert_eq!(
        adf_to_markdown(&text_to_adf("Done.\n\nSee the PR.")),
        "Done.\n\nSee the PR."
    );
}
