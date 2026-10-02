use super::{
    ClassificationRule, Classifier, LineBucket, count_comment_lines, default_rules, parse_numstat_z,
};

#[test]
fn default_rules_classify_tests_docs_config_and_code() {
    let classifier = Classifier::new(&default_rules());

    let cases = [
        ("app/src/stack.rs", LineBucket::Code),
        ("app/src/stack_tests.rs", LineBucket::Tests),
        ("crates/foo/tests/integration.rs", LineBucket::Tests),
        ("web/src/button.test.tsx", LineBucket::Tests),
        ("web/src/button.spec.ts", LineBucket::Tests),
        ("README.md", LineBucket::Docs),
        ("docs/guide/setup.html", LineBucket::Docs),
        ("Cargo.toml", LineBucket::Config),
        ("crates/foo/Cargo.toml", LineBucket::Config),
        ("web/package-lock.json", LineBucket::Config),
        (".github/workflows/ci.yml", LineBucket::Config),
        (".gitignore", LineBucket::Config),
    ];
    let classified: Vec<(&str, LineBucket)> = cases
        .iter()
        .map(|(path, _)| (*path, classifier.classify(path)))
        .collect();
    assert_eq!(classified, cases.to_vec());
}

#[test]
fn first_matching_rule_wins() {
    let rules = vec![
        ClassificationRule {
            pattern: "vendor/**".to_string(),
            bucket: LineBucket::Config,
        },
        ClassificationRule {
            pattern: "*.rs".to_string(),
            bucket: LineBucket::Tests,
        },
    ];
    let classifier = Classifier::new(&rules);

    assert_eq!(classifier.classify("vendor/lib.rs"), LineBucket::Config);
    assert_eq!(classifier.classify("src/lib.rs"), LineBucket::Tests);
    assert_eq!(classifier.classify("src/lib.go"), LineBucket::Code);
}

#[test]
fn invalid_patterns_are_skipped() {
    let rules = vec![ClassificationRule {
        pattern: "[".to_string(),
        bucket: LineBucket::Docs,
    }];

    assert_eq!(Classifier::new(&rules).classify("["), LineBucket::Code);
}

#[test]
fn counts_rust_line_and_block_comments() {
    let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,0 +1,8 @@
+/// Adds two numbers.
+fn add(a: i32, b: i32) -> i32 {
+    // the obvious way
+    a + b
+}
+/* a block
+   spanning lines */
+const X: i32 = 1;
@@ -20,2 +27,0 @@
-// removed comment
-let y = 2;
";

    assert_eq!(count_comment_lines(diff, |_| true), 5);
}

#[test]
fn counts_python_hash_comments_and_docstrings() {
    let diff = "\
diff --git a/app.py b/app.py
--- a/app.py
+++ b/app.py
@@ -0,0 +1,7 @@
+def main():
+    \"\"\"Entry point.
+
+    Runs the app.\"\"\"
+    # start
+    run()
+    return 0
";

    // The blank line inside the docstring is not counted.
    assert_eq!(count_comment_lines(diff, |_| true), 3);
}

#[test]
fn counts_html_block_comments() {
    let diff = "\
diff --git a/index.html b/index.html
--- a/index.html
+++ b/index.html
@@ -0,0 +1,4 @@
+<!-- header -->
+<h1>Title</h1>
+<!--
+  footer -->
";

    assert_eq!(count_comment_lines(diff, |_| true), 3);
}

#[test]
fn skips_files_that_are_not_code_and_deleted_sql_comments_in_headers() {
    let diff = "\
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -0,0 +1,1 @@
+# Heading
diff --git a/schema.sql b/schema.sql
deleted file mode 100644
--- a/schema.sql
+++ /dev/null
@@ -1,2 +0,0 @@
--- a comment
-select 1;
";

    assert_eq!(count_comment_lines(diff, |path| path.ends_with(".sql")), 1);
}

#[test]
fn parses_numstat_with_renames_and_binaries() {
    let out = "3\t1\tsrc/a.rs\0\
               2\t0\t\0src/old.rs\0src/new.rs\0\
               -\t-\timg.png\0";

    assert_eq!(
        parse_numstat_z(out),
        vec![
            ("src/a.rs".to_string(), 3, 1),
            ("src/new.rs".to_string(), 2, 0),
            ("img.png".to_string(), 0, 0),
        ]
    );
}
