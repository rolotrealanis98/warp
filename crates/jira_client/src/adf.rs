//! Atlassian Document Format (ADF) to markdown, and plain text to ADF.
//!
//! Only what an issue description needs to read well as an agent prompt: paragraphs, headings,
//! lists (bullet, ordered, task, nested), code, quotes, tables, and the common inline nodes.
//! Unknown nodes fall back to their children, so new node types degrade to plain text.

use serde_json::{Value, json};

/// Node types that hold inline content rather than blocks.
const INLINE_TYPES: [&str; 8] = [
    "text",
    "hardBreak",
    "mention",
    "emoji",
    "inlineCard",
    "status",
    "date",
    "mediaInline",
];

/// Converts an ADF document (or any ADF node) to markdown.
pub fn adf_to_markdown(node: &Value) -> String {
    block(node).trim().to_string()
}

/// A plain-text comment as an ADF document: blank lines separate paragraphs, single newlines
/// become hard breaks.
pub fn text_to_adf(text: &str) -> Value {
    let paragraphs: Vec<Value> = text
        .split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .map(|paragraph| {
            let mut content = Vec::new();
            for (index, line) in paragraph.lines().enumerate() {
                if index > 0 {
                    content.push(json!({ "type": "hardBreak" }));
                }
                if !line.is_empty() {
                    content.push(json!({ "type": "text", "text": line }));
                }
            }
            json!({ "type": "paragraph", "content": content })
        })
        .collect();
    json!({ "type": "doc", "version": 1, "content": paragraphs })
}

fn node_type(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or_default()
}

fn content(node: &Value) -> &[Value] {
    node.get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn attr<'a>(node: &'a Value, name: &str) -> Option<&'a Value> {
    node.get("attrs")?.get(name)
}

fn attr_str<'a>(node: &'a Value, name: &str) -> Option<&'a str> {
    attr(node, name).and_then(Value::as_str)
}

fn blocks(nodes: &[Value], separator: &str) -> String {
    nodes
        .iter()
        .map(block)
        .filter(|rendered| !rendered.trim().is_empty())
        .collect::<Vec<_>>()
        .join(separator)
}

fn block(node: &Value) -> String {
    let children = content(node);
    match node_type(node) {
        "paragraph" => inline(children),
        "heading" => {
            let level = attr(node, "level")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 6) as usize;
            format!("{} {}", "#".repeat(level), inline(children))
        }
        "bulletList" => list(children, |_, _| "- ".to_string()),
        "orderedList" => {
            let start = attr(node, "order").and_then(Value::as_u64).unwrap_or(1);
            list(children, |index, _| format!("{}. ", start + index as u64))
        }
        "taskList" => list(children, |_, item| {
            if attr_str(item, "state") == Some("DONE") {
                "- [x] ".to_string()
            } else {
                "- [ ] ".to_string()
            }
        }),
        "codeBlock" => {
            let language = attr_str(node, "language").unwrap_or_default();
            let code: String = children
                .iter()
                .filter_map(|child| child.get("text").and_then(Value::as_str))
                .collect();
            format!("```{language}\n{}\n```", code.trim_end_matches('\n'))
        }
        "blockquote" => blocks(children, "\n\n")
            .lines()
            .map(|line| format!("> {line}").trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        "rule" => "---".to_string(),
        "table" => table(children),
        "mediaSingle" | "mediaGroup" | "media" => "[attachment]".to_string(),
        "blockCard" | "embedCard" => inline_node(node),
        kind if INLINE_TYPES.contains(&kind) => inline_node(node),
        // doc, panel, expand, layouts, decision lists, ...
        _ => blocks(children, "\n\n"),
    }
}

/// Renders list items with `marker(index, item)`, indenting continuation lines (nested lists,
/// extra paragraphs) under the item's text.
fn list(items: &[Value], marker: impl Fn(usize, &Value) -> String) -> String {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let marker = marker(index, item);
            let children = content(item);
            // listItem holds blocks; taskItem and decisionItem hold inline content.
            let body = if children
                .first()
                .is_some_and(|child| INLINE_TYPES.contains(&node_type(child)))
            {
                inline(children)
            } else {
                // ponytail: tight lists, so two paragraphs in one item join as one; fine for prompts.
                blocks(children, "\n")
            };
            let pad = " ".repeat(marker.chars().count());
            let mut lines = body.lines();
            let mut rendered = format!("{marker}{}", lines.next().unwrap_or_default());
            for line in lines {
                rendered.push('\n');
                if !line.is_empty() {
                    rendered.push_str(&pad);
                    rendered.push_str(line);
                }
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn table(rows: &[Value]) -> String {
    let rendered: Vec<String> = rows
        .iter()
        .map(|row| {
            let cells: Vec<String> = content(row)
                .iter()
                .map(|cell| {
                    blocks(content(cell), " ")
                        .replace('\n', " ")
                        .replace('|', "\\|")
                })
                .collect();
            format!("| {} |", cells.join(" | "))
        })
        .collect();
    let Some(first) = rows.first() else {
        return String::new();
    };
    let separator = format!("|{}", " --- |".repeat(content(first).len()));
    let mut lines = vec![rendered[0].clone(), separator];
    lines.extend(rendered[1..].iter().cloned());
    lines.join("\n")
}

fn inline(nodes: &[Value]) -> String {
    nodes.iter().map(inline_node).collect()
}

fn inline_node(node: &Value) -> String {
    match node_type(node) {
        "text" => with_marks(
            node.get("text").and_then(Value::as_str).unwrap_or_default(),
            node.get("marks")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        ),
        "hardBreak" => "\n".to_string(),
        "mention" => {
            let name = attr_str(node, "text").unwrap_or_default().trim();
            match name {
                "" => "@someone".to_string(),
                name if name.starts_with('@') => name.to_string(),
                name => format!("@{name}"),
            }
        }
        "emoji" => attr_str(node, "text")
            .or_else(|| attr_str(node, "shortName"))
            .unwrap_or_default()
            .to_string(),
        "inlineCard" | "blockCard" | "embedCard" => attr_str(node, "url")
            .map(|url| format!("<{url}>"))
            .unwrap_or_default(),
        "status" => attr_str(node, "text")
            .map(|text| format!("[{text}]"))
            .unwrap_or_default(),
        "date" => attr_str(node, "timestamp")
            .and_then(|millis| millis.parse::<i64>().ok())
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|date| date.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        _ => inline(content(node)),
    }
}

// ponytail: marks wrap each text node on its own, so adjacent bold nodes render `**a****b**`
// and marks over leading/trailing spaces may not parse as markdown; agents read it fine.
fn with_marks(text: &str, marks: &[Value]) -> String {
    if text.is_empty() {
        return String::new();
    }
    let has = |kind: &str| marks.iter().any(|mark| node_type(mark) == kind);
    let mut rendered = if has("code") {
        if text.contains('`') {
            format!("`` {text} ``")
        } else {
            format!("`{text}`")
        }
    } else {
        text.to_string()
    };
    if has("em") {
        rendered = format!("*{rendered}*");
    }
    if has("strong") {
        rendered = format!("**{rendered}**");
    }
    if has("strike") {
        rendered = format!("~~{rendered}~~");
    }
    if let Some(href) = marks
        .iter()
        .find(|mark| node_type(mark) == "link")
        .and_then(|mark| attr_str(mark, "href"))
    {
        rendered = format!("[{rendered}]({href})");
    }
    rendered
}

#[cfg(test)]
#[path = "adf_tests.rs"]
mod tests;
