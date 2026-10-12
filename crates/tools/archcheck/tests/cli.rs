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

/// GitHub shows at most 10 annotations of each level per step, so the rest
/// are counted in one line that points to the job summary.
#[test]
fn github_annotations_are_capped_per_level() {
    let ws = Workspace::new("advisory");
    ws.config("advisory", "required", "advisory");
    let mut listed = Vec::new();
    for index in 0..12 {
        ws.rust_file(&format!("crates/a/src/new{index:02}.rs"), LIMIT + 1);
        ws.rust_file(&format!("crates/b/src/listed{index:02}.rs"), LIMIT + 2);
        listed.push(format!("crates/b/src/listed{index:02}.rs"));
    }
    let entries: Vec<(&str, usize)> = listed.iter().map(|path| (path.as_str(), 6)).collect();
    ws.baseline(&entries);

    let run = ws.run(&["--format", "github"]);

    assert_eq!(run.code, 1, "{}", run.stderr);
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("::warning ")).count(),
        10
    );
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("::error ")).count(),
        10
    );
    assert!(lines[9].starts_with("::warning file=crates/a/src/new09.rs,"));
    assert!(lines[19].starts_with("::error file=crates/b/src/listed09.rs,"));
    assert_eq!(
        lines[20..],
        [
            "archcheck: 2 more advisory findings not annotated; the job summary lists them, and \
             `cargo arch` reproduces them.",
            "archcheck: 2 more required findings not annotated; the job summary lists them, and \
             `cargo arch` reproduces them.",
            "archcheck: 24 findings (12 advisory, 12 required; 24 production Rust files checked)",
        ]
    );
}

/// Workflow commands need `%`, CR and LF escaped in the message, and also `:`
/// and `,` in properties such as `file=`.
#[test]
fn github_annotations_escape_the_path_and_message() {
    let ws = Workspace::new("advisory");
    ws.rust_file("crates/a/src/p%25,q:r\r\ns.rs", LIMIT + 1);

    let run = ws.run(&["--format", "github"]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout.lines().next().unwrap(),
        "::warning file=crates/a/src/p%2525%2Cq%3Ar%0D%0As.rs,line=1,title=archcheck \
         file_size.no_crossing (advisory)::crates/a/src/p%2525,q:r%0D%0As.rs: 6 code lines, over \
         the 5-line limit (comments and blank lines are not counted). Split it into submodules \
         by responsibility."
    );
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

/// Unicode format characters (general category Cf) are invisible and can
/// hide or reorder text, so the report shows each of them escaped.
#[test]
fn markdown_report_escapes_format_characters() {
    let format_characters = [
        '\u{00AD}',
        '\u{0600}',
        '\u{061C}',
        '\u{200B}',
        '\u{200E}',
        '\u{202E}',
        '\u{2066}',
        '\u{FEFF}',
        '\u{FFF9}',
        '\u{FFFB}',
        '\u{1D173}',
        '\u{E0001}',
        '\u{E0041}',
        '\u{E007F}',
    ];
    let ws = Workspace::new("advisory");
    let name: String = format_characters.iter().collect();
    ws.rust_file(&format!("crates/a/src/{name}.rs"), LIMIT + 1);

    let run = ws.run(&["--format", "markdown"]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    let escaped: String = format_characters
        .iter()
        .map(|c| c.escape_unicode().to_string())
        .collect();
    assert!(
        run.stdout
            .contains(&format!("- `crates/a/src/{escaped}.rs`: 6 code lines")),
        "{}",
        run.stdout
    );
    assert!(!run.stdout.contains(format_characters), "{:?}", run.stdout);
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

/// Lines GitHub's runner would read as a workflow command: `::` after leading
/// whitespace (the runner trims Unicode whitespace, as `trim_start` does), or
/// `##[` anywhere.
fn workflow_commands(output: &str) -> Vec<&str> {
    output
        .lines()
        .filter(|line| line.trim_start().starts_with("::") || line.contains("##["))
        .collect()
}

/// Every way the tree can put text in the output: a workspace member name, a
/// TOML key and a file path. Each must stay on its line in every format and
/// on stderr, and no line may become a workflow command.
#[cfg(unix)]
#[test]
fn text_from_the_tree_cannot_start_a_workflow_command() {
    const MEMBER: &str = "x\n::add-mask::archcheck\n::stop-commands::tok";
    const KEY: &str = "x\n::error title=pwn::spoofed";
    const PATH: &str = "crates/a/src/new\n::error::line.rs";

    // A member, a baseline key and a file path, each reported as a finding.
    let ws = Workspace::new("advisory");
    let members = format!(
        "[workspace]\nmembers = [\"crates/a\", \"crates/b\", {MEMBER:?}, \"::error::m\", \
         \"\u{2003}::error::n\"]\n"
    );
    ws.write("Cargo.toml", &members);
    for member in [MEMBER, "::error::m", "\u{2003}::error::n"] {
        ws.write(&format!("{member}/Cargo.toml"), "[package]\n");
        ws.rust_file(&format!("{member}/src/lib.rs"), LIMIT + 1);
    }
    ws.rust_file(PATH, LIMIT + 1);
    ws.rust_file("crates/b/src/##[add-mask]x.rs", LIMIT + 1);
    ws.write(
        "architecture-baseline.toml",
        &format!("[file_size]\n{:?} = 9\n", format!("crates/a/src/{KEY}.rs")),
    );
    for format in ["human", "github", "markdown"] {
        let run = ws.run(&["--format", format]);
        assert_eq!(run.code, 0, "{format}: {}{}", run.stdout, run.stderr);
        let commands = workflow_commands(&run.stdout);
        if format == "github" {
            // Only archcheck's own annotations, one per finding.
            assert_eq!(commands.len(), 6, "{}", run.stdout);
            assert!(
                commands
                    .iter()
                    .all(|line| line.starts_with("::warning file=")),
                "{}",
                run.stdout
            );
        } else {
            assert_eq!(commands, Vec::<&str>::new(), "{format}");
        }
        for injected in ["::add-mask::", "::error title=pwn::", "::error::line"] {
            let lines = run
                .stdout
                .lines()
                .filter(|line| line.contains(injected))
                .count();
            assert_eq!(lines, 1, "{format}: {injected:?} in\n{}", run.stdout);
        }
        assert!(run.stderr.is_empty(), "{format}: {}", run.stderr);
    }
    let human = ws.run(&[]).stdout;
    for escaped in [
        "  x\\n::add-mask::archcheck\\n::stop-commands::tok/src/lib.rs: 6 code lines",
        "  crates/a/src/new\\n::error::line.rs: 6 code lines",
        "  crates/a/src/x\\n::error title=pwn::spoofed.rs: listed in",
        "  \\u{3a}:error::m/src/lib.rs: 6 code lines",
        "  \u{2003}\\u{3a}:error::n/src/lib.rs: 6 code lines",
        "  crates/b/src/##\\u{5b}add-mask]x.rs: 6 code lines",
    ] {
        assert!(
            human.lines().any(|line| line.starts_with(escaped)),
            "{escaped:?} in\n{human}"
        );
    }

    // The same text in errors, which quote members, keys and paths.
    let errors: [(&str, String); 3] = [
        (
            "Cargo.toml",
            format!(
                "[workspace]\nmembers = [\"crates/a\", {:?}]\n",
                format!("{MEMBER}/y")
            ),
        ),
        (
            "ARCHITECTURE.toml",
            format!("[rules.file_size]\n{KEY:?} = 1\n"),
        ),
        (
            "architecture-baseline.toml",
            format!("[file_size]\n{:?} = 1\n", format!("crates/{KEY}")),
        ),
    ];
    for (file, contents) in errors {
        let ws = Workspace::new("advisory");
        ws.write(file, &contents);
        let run = ws.run(&[]);
        assert_eq!(run.code, 2, "{file}: {}{}", run.stdout, run.stderr);
        assert_eq!(run.stderr.lines().count(), 1, "{file}: {}", run.stderr);
        assert_eq!(workflow_commands(&run.stderr), Vec::<&str>::new(), "{file}");
        assert!(run.stderr.contains("x\\n::"), "{file}: {}", run.stderr);
    }
    // A path in an error: a `.rs` name that is not UTF-8.
    {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let ws = Workspace::new("advisory");
        let src = ws.root().join("crates/a/src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join(OsStr::from_bytes(b"new\n::error::\xff.rs")), "").unwrap();
        let run = ws.run(&[]);
        assert_eq!(run.code, 2, "{}{}", run.stdout, run.stderr);
        assert_eq!(run.stderr.lines().count(), 1, "{}", run.stderr);
        assert_eq!(workflow_commands(&run.stderr), Vec::<&str>::new());
        assert!(run.stderr.contains("new\\n::error::"), "{}", run.stderr);
    }

    let ws = Workspace::new("advisory");
    let run = ws.run(&["--format", "x\n::error::usage"]);
    assert_eq!(run.code, 2);
    assert_eq!(workflow_commands(&run.stderr), Vec::<&str>::new());
    assert!(
        run.stderr
            .starts_with("archcheck: unknown format \"x\\n::error::usage\"")
    );
}

impl Workspace {
    /// Writes `paths` as a changed-file list and returns `--changed-files`
    /// arguments for it. The list lives outside the checked tree.
    fn changed_files(&self, list: &TempDir, paths: &[&str]) -> [String; 2] {
        let path = list.path().join("changed-files");
        let text: String = paths.iter().map(|path| format!("{path}\n")).collect();
        fs::write(&path, text).expect("write the changed-file list");
        [
            "--changed-files".to_owned(),
            path.to_str().expect("temporary paths are UTF-8").to_owned(),
        ]
    }

    fn run_changed(&self, paths: &[&str], args: &[&str]) -> Run {
        let list = tempfile::tempdir().expect("create a directory for the list");
        let changed = self.changed_files(&list, paths);
        let mut all: Vec<&str> = changed.iter().map(String::as_str).collect();
        all.extend_from_slice(args);
        self.run(&all)
    }
}

#[test]
fn changed_files_limit_the_report_to_the_files_a_change_touched() {
    let ws = Workspace::new("advisory");
    ws.rust_file("crates/a/src/touched.rs", LIMIT + 1);
    ws.rust_file("crates/a/src/untouched.rs", LIMIT + 1);
    ws.rust_file("crates/b/src/untouched.rs", LIMIT + 1);
    ws.baseline(&[("crates/b/src/listed.rs", 6)]);
    ws.rust_file("crates/b/src/listed.rs", 7);

    let run = ws.run_changed(&["crates/a/src/touched.rs", "docs/unrelated.md"], &[]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        run.findings(),
        [
            "crates/a/src/touched.rs: 6 code lines, over the 5-line limit (comments and blank \
             lines are not counted). Split it into submodules by responsibility."
        ]
    );
    assert!(
        run.stdout.ends_with(
            "archcheck: 1 finding (1 advisory, 0 required; 4 production Rust files checked; 3 \
             findings elsewhere in the tree not shown)\n"
        ),
        "{}",
        run.stdout
    );

    // Without the list, the whole tree.
    assert!(ws.run(&[]).stdout.contains(
        "archcheck: 4 findings (4 advisory, 0 required; 4 production Rust files \
                       checked)\n"
    ));

    // The hidden count is in every format.
    let markdown = ws.run_changed(&["crates/a/src/touched.rs"], &["--format", "markdown"]);
    assert!(
        markdown.stdout.starts_with(
            "### archcheck: 1 finding (1 advisory, 0 required; 4 production Rust files checked; \
             3 findings elsewhere in the tree not shown)\n"
        ),
        "{}",
        markdown.stdout
    );
    assert_eq!(
        markdown.stdout.matches("\n- ").count(),
        1,
        "{}",
        markdown.stdout
    );
    let github = ws.run_changed(&["crates/a/src/touched.rs"], &["--format", "github"]);
    assert_eq!(
        workflow_commands(&github.stdout).len(),
        1,
        "{}",
        github.stdout
    );
    assert!(
        github
            .stdout
            .contains("; 3 findings elsewhere in the tree not shown)\n")
    );
}

#[test]
fn changed_files_without_findings_say_how_many_were_not_shown() {
    let ws = Workspace::new("required");
    ws.rust_file("crates/a/src/untouched.rs", LIMIT + 1);
    ws.rust_file("crates/a/src/small.rs", LIMIT);

    // A required finding in an untouched file neither shows nor fails.
    let run = ws.run_changed(&["crates/a/src/small.rs"], &["--format", "markdown"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "### archcheck: no findings in the changed files (2 production Rust files checked; 1 \
         finding elsewhere in the tree not shown)\n"
    );
    assert_eq!(ws.run(&[]).code, 1);

    // An empty list shows nothing, and a touched file fails as before.
    let run = ws.run_changed(&[], &[]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout
            .starts_with("archcheck: no findings in the changed files")
    );
    assert_eq!(ws.run_changed(&["crates/a/src/untouched.rs"], &[]).code, 1);
}

/// A deleted or renamed listed file is matched by its old path, which both
/// GitHub's file list (`previous_filename`) and `git diff --no-renames` give.
#[test]
fn changed_files_match_baseline_entries_by_their_old_path() {
    let ws = Workspace::new("advisory");
    ws.baseline(&[
        ("crates/a/src/deleted.rs", 6),
        ("crates/a/src/old.rs", 6),
        ("crates/b/src/stale.rs", 9),
    ]);
    ws.rust_file("crates/a/src/new.rs", 6);
    ws.rust_file("crates/b/src/stale.rs", 7);

    let run = ws.run_changed(
        &[
            "crates/a/src/deleted.rs",
            "crates/a/src/old.rs",
            "crates/a/src/new.rs",
        ],
        &[],
    );

    assert_eq!(run.code, 0, "{}", run.stderr);
    let paths: Vec<&str> = run
        .findings()
        .iter()
        .map(|finding| finding.split_once(':').unwrap().0)
        .collect();
    assert_eq!(
        paths,
        [
            "crates/a/src/new.rs",
            "crates/a/src/deleted.rs",
            "crates/a/src/old.rs"
        ],
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("; 1 finding elsewhere in the tree not shown)\n")
    );
}

/// A change to the rules or the baseline changes what every file is checked
/// against, so it is shown everything.
#[test]
fn changing_the_rules_reports_the_whole_tree() {
    let ws = Workspace::new("advisory");
    ws.rust_file("crates/a/src/one.rs", LIMIT + 1);
    ws.rust_file("crates/b/src/two.rs", LIMIT + 1);

    for rules in ["ARCHITECTURE.toml", "architecture-baseline.toml"] {
        let run = ws.run_changed(&[rules, "crates/a/src/one.rs"], &[]);
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert_eq!(run.findings().len(), 2, "{rules}: {}", run.stdout);
        assert!(
            run.stdout
                .ends_with("(2 advisory, 0 required; 2 production Rust files checked)\n"),
            "{rules}: {}",
            run.stdout
        );
    }
}

#[test]
fn changed_file_list_errors_exit_2() {
    let ws = Workspace::new("advisory");
    let missing = ws.root().join("no-such-list");
    let run = ws.run(&["--changed-files", missing.to_str().unwrap()]);
    assert_eq!(run.code, 2);
    assert!(
        run.stderr.contains("could not read the changed-file list"),
        "{}",
        run.stderr
    );

    let list = tempfile::tempdir().unwrap();
    let path = list.path().join("changed-files");
    fs::write(&path, b"crates/a/src/\xff.rs\n").unwrap();
    let run = ws.run(&["--changed-files", path.to_str().unwrap()]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("is not valid UTF-8"), "{}", run.stderr);
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
        &["--write-baseline", "--changed-files", "changed-files"],
        &["--changed-files"],
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

/// The tree may be untrusted: `--write-baseline` must not write through a
/// symlink, whether it leads outside the root or not.
#[cfg(unix)]
#[test]
fn write_baseline_refuses_a_symlinked_baseline() {
    use std::os::unix::fs::symlink;

    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("victim.toml");
    fs::write(&target, "keep me\n").unwrap();

    let ws = Workspace::new("advisory");
    ws.rust_file("crates/a/src/lib.rs", LIMIT + 1);
    let baseline = ws.root().join("architecture-baseline.toml");
    fs::remove_file(&baseline).unwrap();
    symlink(&target, &baseline).unwrap();

    let run = ws.run(&["--write-baseline"]);
    assert_eq!(run.code, 2, "{}{}", run.stdout, run.stderr);
    assert!(run.stderr.contains("refusing to write"), "{}", run.stderr);
    assert_eq!(fs::read_to_string(&target).unwrap(), "keep me\n");
    assert!(fs::symlink_metadata(&baseline).unwrap().is_symlink());

    // A symlink inside the root is refused too.
    fs::remove_file(&baseline).unwrap();
    ws.write("crates/baseline.toml", "keep me\n");
    symlink("crates/baseline.toml", &baseline).unwrap();
    assert_eq!(ws.run(&["--write-baseline"]).code, 2);
    assert_eq!(
        fs::read_to_string(ws.root().join("crates/baseline.toml")).unwrap(),
        "keep me\n"
    );
}
