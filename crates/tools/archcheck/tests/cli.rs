//! End-to-end runs of the `archcheck` binary over temporary workspaces.

use std::{fs, path::Path, process::Command};

use tempfile::TempDir;

const LIMIT: usize = 5;

struct Workspace {
    dir: TempDir,
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Workspace {
    /// A workspace with members `a` and `b`, every behaviour in `mode`, and an
    /// empty baseline.
    fn new(mode: &str) -> Self {
        let workspace = Self {
            dir: tempfile::tempdir().expect("create a temporary workspace"),
        };
        workspace.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
        );
        for member in ["crates/a", "crates/b"] {
            workspace.write(&format!("{member}/Cargo.toml"), "[package]\n");
        }
        workspace.config(mode, mode, mode);
        workspace.write("architecture-baseline.toml", "[file_size]\n");
        workspace
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.root().join(path);
        let parent = path
            .parent()
            .expect("workspace files have a parent directory");
        fs::create_dir_all(parent).expect("create the parent directory");
        fs::write(&path, contents).expect("write a workspace file");
    }

    fn config(&self, no_crossing: &str, listed_no_growth: &str, stale_entries: &str) {
        self.write(
            "ARCHITECTURE.toml",
            &format!(
                "[rules.file_size]\nlimit = {LIMIT}\n\n\
                 [rules.file_size.no_crossing]\nmode = \"{no_crossing}\"\n\n\
                 [rules.file_size.listed_no_growth]\nmode = \"{listed_no_growth}\"\n\n\
                 [rules.file_size.stale_entries]\nmode = \"{stale_entries}\"\n"
            ),
        );
    }

    fn baseline(&self, entries: &[(&str, usize)]) {
        let mut text = String::from("[file_size]\n");
        for (path, cap) in entries {
            text.push_str(&format!("\"{path}\" = {cap}\n"));
        }
        self.write("architecture-baseline.toml", &text);
    }

    /// A Rust file with `lines` code lines.
    fn rust_file(&self, path: &str, lines: usize) {
        self.write(path, &"let x = 1;\n".repeat(lines));
    }

    fn run(&self, args: &[&str]) -> Run {
        let output = Command::new(env!("CARGO_BIN_EXE_archcheck"))
            .arg("--root")
            .arg(self.root())
            .args(args)
            .output()
            .expect("run archcheck");
        Run {
            code: output
                .status
                .code()
                .expect("archcheck exited with a status code"),
            stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
            stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        }
    }
}

impl Run {
    fn assert_clean(&self) {
        assert_eq!(self.code, 0, "{}{}", self.stdout, self.stderr);
        assert!(
            self.stdout.contains("archcheck: no findings"),
            "{}",
            self.stdout
        );
    }

    /// The indented finding lines of the human report.
    fn findings(&self) -> Vec<&str> {
        self.stdout
            .lines()
            .filter_map(|line| line.strip_prefix("  "))
            .collect()
    }
}

#[test]
fn new_file_over_the_limit_is_reported() {
    let ws = Workspace::new("advisory");
    ws.rust_file("crates/a/src/lib.rs", LIMIT);
    ws.rust_file("crates/a/src/big/nested.rs", LIMIT + 1);

    let run = ws.run(&[]);

    assert_eq!(run.code, 0);
    assert_eq!(
        run.findings(),
        [
            "crates/a/src/big/nested.rs: 6 code lines, over the 5-line limit (comments and blank \
          lines are not counted). Split it into submodules by responsibility."
        ]
    );
    assert!(run.stdout.contains("file_size.no_crossing (advisory):"));
    assert!(
        run.stdout
            .contains("archcheck: 1 finding (1 advisory, 0 required;")
    );
}

#[test]
fn file_crossing_the_limit_is_reported() {
    let ws = Workspace::new("advisory");
    ws.rust_file("crates/a/src/lib.rs", LIMIT);
    ws.run(&[]).assert_clean();

    ws.rust_file("crates/a/src/lib.rs", LIMIT + 1);

    let findings = ws.run(&[]).stdout;
    assert!(
        findings.contains("crates/a/src/lib.rs: 6 code lines, over the 5-line limit"),
        "{findings}"
    );
}

#[test]
fn comments_doc_comments_and_blank_lines_are_not_counted() {
    let ws = Workspace::new("advisory");
    ws.write(
        "crates/a/src/lib.rs",
        "//! Crate docs.\n\
         \n\
         /// Item docs.\n\
         #[derive(Debug)]\n\
         pub struct A;\n\
         \x20   \n\
         \t// indented comment\n\
         /* block comments count */\n\
         fn f() {\n\
         \x20   // body comment\n\
         }\n\
         \n\
         pub use A as B;\n",
    );

    let run = ws.run(&[]);

    assert_eq!(
        run.findings(),
        [
            "crates/a/src/lib.rs: 6 code lines, over the 5-line limit (comments and blank lines \
          are not counted). Split it into submodules by responsibility."
        ]
    );
}

#[test]
fn a_byte_order_mark_does_not_make_a_comment_count() {
    let ws = Workspace::new("advisory");
    ws.write(
        "crates/a/src/lib.rs",
        &format!("\u{feff}//! Crate docs.\n{}", "let x = 1;\n".repeat(LIMIT)),
    );

    ws.run(&[]).assert_clean();
}

#[test]
fn sibling_test_files_and_non_members_are_excluded() {
    let ws = Workspace::new("advisory");
    ws.rust_file("crates/a/src/tests.rs", LIMIT + 1);
    ws.rust_file("crates/a/src/module/tests.rs", LIMIT + 1);
    ws.rust_file("crates/a/src/module_tests.rs", LIMIT + 1);
    ws.rust_file("crates/a/src/module/tests/mod.rs", LIMIT + 1);
    ws.rust_file("crates/a/src/tests/nested/helpers.rs", LIMIT + 1);
    ws.rust_file("crates/a/tests/integration.rs", LIMIT + 1);
    ws.rust_file("crates/a/build.rs", LIMIT + 1);
    ws.rust_file("crates/not-a-member/src/lib.rs", LIMIT + 1);
    ws.rust_file("crates/b/src/latests.rs", LIMIT + 1);
    ws.rust_file("crates/b/src/latests/mod.rs", LIMIT + 1);

    let run = ws.run(&[]);

    let paths: Vec<&str> = run
        .findings()
        .iter()
        .map(|finding| finding.split_once(':').unwrap().0)
        .collect();
    assert_eq!(
        paths,
        ["crates/b/src/latests.rs", "crates/b/src/latests/mod.rs"],
        "{}",
        run.stdout
    );
}

/// Only `.rs` names must be UTF-8: other files are ignored, and a `.rs` name
/// that is not UTF-8 is an error that shows the name escaped.
#[cfg(unix)]
#[test]
fn non_utf8_names_are_ignored_unless_they_are_rust_files() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let ws = Workspace::new("advisory");
    ws.rust_file("crates/a/src/lib.rs", LIMIT);
    let src = ws.root().join("crates/a/src");
    fs::write(src.join(OsStr::from_bytes(b"notes-\xff.txt")), "x\n").unwrap();
    fs::write(src.join(OsStr::from_bytes(b"\xff")), "x\n").unwrap();
    ws.run(&[]).assert_clean();

    fs::write(src.join(OsStr::from_bytes(b"bad-\xff.rs")), "let x = 1;\n").unwrap();
    let run = ws.run(&[]);
    assert_eq!(run.code, 2, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stderr.contains("bad-\\xFF.rs") && run.stderr.contains("is not valid UTF-8"),
        "{}",
        run.stderr
    );
    assert!(run.stdout.is_empty(), "{}", run.stdout);
}

#[test]
fn listed_file_findings_follow_its_count() {
    let ws = Workspace::new("advisory");
    ws.baseline(&[
        ("crates/a/src/deleted.rs", 9),
        ("crates/a/src/grew.rs", 8),
        ("crates/a/src/same.rs", 8),
        ("crates/a/src/shrank.rs", 8),
        ("crates/a/src/under.rs", 8),
    ]);
    ws.rust_file("crates/a/src/grew.rs", 9);
    ws.rust_file("crates/a/src/same.rs", 8);
    ws.rust_file("crates/a/src/shrank.rs", 7);
    ws.rust_file("crates/a/src/under.rs", LIMIT);

    let run = ws.run(&[]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        run.findings(),
        [
            "crates/a/src/grew.rs grew from 8 to 9 code lines. Once this rule is required, split \
             the file before adding to it.",
            "crates/a/src/deleted.rs: listed in architecture-baseline.toml but no longer a \
             production Rust file. Remove the entry; a renamed file is checked under its new path.",
            "crates/a/src/shrank.rs shrank from 8 to 7 code lines. Lower its cap in \
             architecture-baseline.toml to 7.",
            "crates/a/src/under.rs is down to 5 code lines, within the 5-line limit. Remove it \
             from architecture-baseline.toml.",
        ]
    );
    let headings: Vec<&str> = run
        .stdout
        .lines()
        .filter(|line| line.starts_with("file_size."))
        .collect();
    assert_eq!(
        headings,
        [
            "file_size.listed_no_growth (advisory):",
            "file_size.stale_entries (advisory):"
        ]
    );
}

/// Requiring no growth must not fail the change that shrinks, splits or
/// removes a listed file; those only leave a stale entry.
#[test]
fn stale_entries_have_their_own_mode() {
    let ws = Workspace::new("advisory");
    ws.config("required", "required", "advisory");
    ws.baseline(&[
        ("crates/a/src/deleted.rs", 9),
        ("crates/a/src/shrank.rs", 8),
        ("crates/a/src/under.rs", 8),
    ]);
    ws.rust_file("crates/a/src/shrank.rs", 7);
    ws.rust_file("crates/a/src/under.rs", LIMIT);

    let run = ws.run(&[]);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains("(3 advisory, 0 required;"),
        "{}",
        run.stdout
    );

    ws.config("advisory", "advisory", "required");
    let run = ws.run(&[]);
    assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("file_size.stale_entries (required):"));
}

#[test]
fn renamed_listed_file_is_checked_under_its_new_path() {
    let ws = Workspace::new("advisory");
    ws.baseline(&[("crates/a/src/old.rs", 8)]);
    ws.rust_file("crates/a/src/new.rs", 8);

    let findings = ws.run(&[]).stdout;

    assert!(findings.contains("crates/a/src/new.rs: 8 code lines, over"));
    assert!(findings.contains("crates/a/src/old.rs: listed in architecture-baseline.toml"));
}

#[test]
fn only_required_findings_fail_the_check() {
    let ws = Workspace::new("advisory");
    ws.baseline(&[("crates/a/src/listed.rs", 6)]);
    ws.rust_file("crates/a/src/listed.rs", 7);
    ws.rust_file("crates/b/src/new.rs", 6);

    let advisory = ws.run(&["--format", "github"]);
    assert_eq!(advisory.code, 0, "{}", advisory.stderr);
    let lines: Vec<&str> = advisory.stdout.lines().collect();
    assert_eq!(
        lines,
        [
            "::warning file=crates/b/src/new.rs,line=1,title=archcheck file_size.no_crossing \
             (advisory)::crates/b/src/new.rs: 6 code lines, over the 5-line limit (comments and \
             blank lines are not counted). Split it into submodules by responsibility.",
            "::warning file=crates/a/src/listed.rs,line=1,title=archcheck \
             file_size.listed_no_growth (advisory)::crates/a/src/listed.rs grew from 6 to 7 code lines. Once this rule is \
             required, split the file before adding to it.",
            "archcheck: 2 findings (2 advisory, 0 required; 2 production Rust files checked)",
        ]
    );

    // Each behaviour graduates on its own.
    ws.config("required", "advisory", "advisory");
    let crossing_required = ws.run(&["--format", "github"]);
    assert_eq!(crossing_required.code, 1);
    assert!(crossing_required.stdout.contains(
        "::error file=crates/b/src/new.rs,line=1,title=archcheck file_size.no_crossing \
         (required)::"
    ));
    assert!(
        crossing_required
            .stdout
            .contains("::warning file=crates/a/src/listed.rs,line=1,")
    );

    ws.config("advisory", "required", "advisory");
    let growth_required = ws.run(&[]);
    assert_eq!(growth_required.code, 1);
    assert!(growth_required.stdout.contains(
        "file_size.listed_no_growth (required):\n  crates/a/src/listed.rs grew from 6 to 7 code \
         lines. Split the file before adding to it.\n"
    ));
    assert!(growth_required.stdout.contains("(1 advisory, 1 required;"));
}

#[test]
fn markdown_report_groups_findings_by_behaviour() {
    let ws = Workspace::new("advisory");
    ws.config("required", "advisory", "advisory");
    ws.baseline(&[("crates/a/src/listed.rs", 6)]);
    ws.rust_file("crates/a/src/listed.rs", 7);
    ws.rust_file("crates/b/src/new.rs", 6);

    let run = ws.run(&["--format", "markdown"]);

    assert_eq!(run.code, 1, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "### archcheck: 2 findings (1 advisory, 1 required; 2 production Rust files checked)\n\
         \n\
         #### `file_size.no_crossing` (required)\n\
         \n\
         - `crates/b/src/new.rs`: 6 code lines, over the 5-line limit (comments and blank lines \
         are not counted). Split it into submodules by responsibility.\n\
         \n\
         #### `file_size.listed_no_growth` (advisory)\n\
         \n\
         - `crates/a/src/listed.rs` grew from 6 to 7 code lines. Once this rule is required, \
         split the file before adding to it.\n\
         \n\
         Advisory findings are reported but do not block merging; required findings fail the \
         check. Run `cargo arch` locally to reproduce this report. Comments and blank lines are \
         not counted as code lines.\n"
    );
}

#[test]
fn markdown_report_keeps_paths_inside_code_spans() {
    let ws = Workspace::new("advisory");
    ws.rust_file("crates/a/src/x``y|<b>&@z.rs", LIMIT + 1);
    ws.rust_file("crates/a/src/`start.rs", LIMIT + 1);
    ws.rust_file("crates/a/src/new\n::error::line\u{202e}.rs", LIMIT + 1);
    let long = format!(
        "crates/b/src/{}end.rs",
        format!("{}/", "d".repeat(49)).repeat(5)
    );
    ws.rust_file(&long, LIMIT + 1);
    let long_tail: String = long.chars().skip(long.len() - 200).collect();

    let run = ws.run(&["--format", "markdown"]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    let items: Vec<&str> = run
        .stdout
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .map(|item| {
            item.split_once(": 6 code lines")
                .expect("each item is one line")
                .0
        })
        .collect();
    assert_eq!(
        items,
        [
            "``crates/a/src/`start.rs``",
            "`crates/a/src/new\\n::error::line\\u{202e}.rs`",
            "```crates/a/src/x``y|<b>&@z.rs```",
            &format!("`…{long_tail}`"),
        ]
    );
}

#[test]
fn markdown_report_caps_the_findings_shown() {
    let ws = Workspace::new("advisory");
    for index in 0..53 {
        ws.rust_file(&format!("crates/a/src/f{index:02}.rs"), LIMIT + 1);
    }

    let run = ws.run(&["--format", "markdown"]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    let items: Vec<&str> = run
        .stdout
        .lines()
        .filter(|line| line.starts_with("- "))
        .collect();
    assert_eq!(items.len(), 50);
    assert!(items[49].starts_with("- `crates/a/src/f49.rs`"));
    assert!(run.stdout.contains("\n3 more findings not shown.\n"));
    assert!(run.stdout.starts_with("### archcheck: 53 findings"));
}

#[test]
fn markdown_report_without_findings_is_one_line() {
    let ws = Workspace::new("advisory");
    ws.rust_file("crates/a/src/lib.rs", LIMIT);

    let run = ws.run(&["--format", "markdown"]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "### archcheck: no findings (1 production Rust files checked)\n"
    );
}

#[test]
fn summary_writes_the_markdown_report_and_keeps_the_exit_status() {
    let ws = Workspace::new("advisory");
    ws.rust_file("crates/a/src/lib.rs", LIMIT + 1);
    let summary = ws.root().join("out/summary.md");
    fs::create_dir_all(summary.parent().unwrap()).unwrap();
    let summary_arg = summary.to_str().unwrap();

    let advisory = ws.run(&["--format", "github", "--summary", summary_arg]);
    assert_eq!(advisory.code, 0, "{}", advisory.stderr);
    assert!(
        advisory
            .stdout
            .starts_with("::warning file=crates/a/src/lib.rs,line=1,")
    );
    let markdown = ws.run(&["--format", "markdown"]).stdout;
    assert_eq!(fs::read_to_string(&summary).unwrap(), markdown);

    ws.config("required", "required", "required");
    let required = ws.run(&["--summary", summary_arg]);
    assert_eq!(required.code, 1, "{}", required.stderr);
    assert!(
        fs::read_to_string(&summary)
            .unwrap()
            .contains("#### `file_size.no_crossing` (required)")
    );

    let unwritable = ws.root().join("missing/summary.md");
    let run = ws.run(&["--summary", unwritable.to_str().unwrap()]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("could not write"), "{}", run.stderr);

    ws.write("ARCHITECTURE.toml", "[rules.file_size\n");
    fs::remove_file(&summary).unwrap();
    assert_eq!(ws.run(&["--summary", summary_arg]).code, 2);
    assert!(!summary.exists());
}

#[test]
fn required_mode_without_findings_passes() {
    let ws = Workspace::new("required");
    ws.rust_file("crates/a/src/lib.rs", LIMIT);
    ws.run(&[]).assert_clean();
}

#[test]
fn configuration_errors_exit_2() {
    let config = "[rules.file_size]\nlimit = 5\n\
                  [rules.file_size.no_crossing]\nmode = \"advisory\"\n\
                  [rules.file_size.listed_no_growth]\nmode = \"advisory\"\n\
                  [rules.file_size.stale_entries]\nmode = \"advisory\"\n";
    let members = "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n";
    let cases: &[(&str, &str, &str)] = &[
        (
            "unknown key",
            "ARCHITECTURE.toml",
            &format!("{config}\n[rules.file_size.no_crossing.extra]\n"),
        ),
        (
            "unknown key",
            "ARCHITECTURE.toml",
            &config.replace("limit = 5", "limit = 5\nlimt = 5"),
        ),
        (
            "unknown rule",
            "ARCHITECTURE.toml",
            &format!("{config}[rules.crate_names]\n"),
        ),
        (
            "unknown mode",
            "ARCHITECTURE.toml",
            &config.replacen("advisory", "warn", 1),
        ),
        (
            "missing behaviour",
            "ARCHITECTURE.toml",
            "[rules.file_size]\nlimit = 5\n[rules.file_size.no_crossing]\nmode = \"advisory\"\n",
        ),
        ("unparseable", "ARCHITECTURE.toml", "[rules.file_size\n"),
        (
            "unknown baseline section",
            "architecture-baseline.toml",
            "[file_size]\n[file_sizes]\n",
        ),
        (
            "non-integer cap",
            "architecture-baseline.toml",
            "[file_size]\n\"crates/a/src/lib.rs\" = \"9\"\n",
        ),
        (
            "baseline key with a leading ./",
            "architecture-baseline.toml",
            "[file_size]\n\"./crates/a/src/lib.rs\" = 9\n",
        ),
        (
            "baseline key with ..",
            "architecture-baseline.toml",
            "[file_size]\n\"crates/b/../a/src/lib.rs\" = 9\n",
        ),
        (
            "baseline key with an empty segment",
            "architecture-baseline.toml",
            "[file_size]\n\"crates//a/src/lib.rs\" = 9\n",
        ),
        (
            "absolute baseline key",
            "architecture-baseline.toml",
            "[file_size]\n\"/crates/a/src/lib.rs\" = 9\n",
        ),
        (
            "baseline key that is not a Rust file",
            "architecture-baseline.toml",
            "[file_size]\n\"crates/a/src/lib\" = 9\n",
        ),
        (
            "empty baseline key",
            "architecture-baseline.toml",
            "[file_size]\n\"\" = 9\n",
        ),
        (
            "glob member",
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/*\"]\n",
        ),
        (
            "member outside the root",
            "Cargo.toml",
            "[workspace]\nmembers = [\"../elsewhere\"]\n",
        ),
        (
            "missing member",
            "Cargo.toml",
            &members.replace("crates/b", "crates/c"),
        ),
        ("no members", "Cargo.toml", "[workspace]\n"),
    ];
    for (name, file, contents) in cases {
        let ws = Workspace::new("advisory");
        ws.write(file, contents);
        let run = ws.run(&[]);
        assert_eq!(run.code, 2, "{name}: {}{}", run.stdout, run.stderr);
        assert!(run.stderr.contains(file), "{name}: {}", run.stderr);
        assert!(run.stdout.is_empty(), "{name}: {}", run.stdout);
    }

    for missing in ["ARCHITECTURE.toml", "architecture-baseline.toml"] {
        let ws = Workspace::new("advisory");
        fs::remove_file(ws.root().join(missing)).unwrap();
        let run = ws.run(&[]);
        assert_eq!(run.code, 2, "missing {missing}: {}", run.stderr);
        assert!(run.stderr.contains(missing), "{}", run.stderr);
    }
}

#[test]
fn usage_errors_exit_2() {
    let ws = Workspace::new("advisory");
    for args in [
        &["--format", "json"][..],
        &["--format"],
        &["--bogus"],
        &["--summary"],
        &["--write-baseline", "--summary", "summary.md"],
    ] {
        assert_eq!(ws.run(args).code, 2, "{args:?}");
    }
}

#[test]
fn write_baseline_records_files_over_the_limit() {
    let ws = Workspace::new("advisory");
    ws.write("architecture-baseline.toml", "not toml");
    ws.rust_file("crates/b/src/lib.rs", LIMIT + 3);
    ws.rust_file("crates/a/src/z.rs", LIMIT + 1);
    ws.rust_file("crates/a/src/a/b.rs", LIMIT + 2);
    ws.rust_file("crates/a/src/ok.rs", LIMIT);
    ws.rust_file("crates/a/src/tests.rs", LIMIT + 9);

    let run = ws.run(&["--write-baseline"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("wrote 3 file_size entries"));

    let written = fs::read_to_string(ws.root().join("architecture-baseline.toml")).unwrap();
    let body: Vec<&str> = written
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .collect();
    assert_eq!(
        body,
        [
            "[file_size]",
            "\"crates/a/src/a/b.rs\" = 7",
            "\"crates/a/src/z.rs\" = 6",
            "\"crates/b/src/lib.rs\" = 8",
        ]
    );
    assert!(written.starts_with('#'));
    ws.run(&[]).assert_clean();

    // With nothing over the limit the table is still valid input.
    for path in [
        "crates/b/src/lib.rs",
        "crates/a/src/z.rs",
        "crates/a/src/a/b.rs",
    ] {
        fs::remove_file(ws.root().join(path)).unwrap();
    }
    assert_eq!(ws.run(&["--write-baseline"]).code, 0);
    ws.run(&[]).assert_clean();
}
