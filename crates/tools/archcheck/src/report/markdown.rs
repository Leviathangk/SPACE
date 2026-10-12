//! The Markdown report, for a job summary or a pull request comment.
//!
//! On a pull request the checked tree, and so every path, is controlled by the
//! pull request's author. Paths are rendered only inside inline code spans,
//! with control and invisible formatting characters escaped and long paths
//! shortened; other text is backslash-escaped; no raw HTML is emitted; and at
//! most [`MAX_FINDINGS_SHOWN`] findings are listed.

use super::summary;
use crate::{
    escape::{escape_where, is_line_separator},
    finding::Finding,
};

/// Keeps the report far below GitHub's 65,536-character comment limit.
pub const MAX_FINDINGS_SHOWN: usize = 50;

/// Longer paths keep their last characters, after a leading `…`.
const MAX_PATH_CHARS: usize = 200;

const FOOTER: &str = "Advisory findings are reported but do not block merging; required \
                      findings fail the check. Run `cargo arch` locally to reproduce this \
                      report. Comments and blank lines are not counted as code lines.";

/// A heading with the summary line, then up to [`MAX_FINDINGS_SHOWN`] findings
/// grouped by rule and behaviour, then a footer. With no findings, the report
/// is the heading alone.
pub fn render(findings: &[Finding], files_checked: usize) -> String {
    let mut out = format!("### {}\n", text(&summary(findings, files_checked)));
    if findings.is_empty() {
        return out;
    }
    let mut group = None;
    for finding in findings.iter().take(MAX_FINDINGS_SHOWN) {
        let key = (finding.rule, finding.behaviour, finding.mode);
        if group != Some(key) {
            group = Some(key);
            out.push_str(&format!(
                "\n#### {} ({})\n\n",
                code(&format!("{}.{}", finding.rule, finding.behaviour)),
                finding.mode
            ));
        }
        out.push_str(&format!("- {}\n", item(finding)));
    }
    let hidden = findings.len().saturating_sub(MAX_FINDINGS_SHOWN);
    if hidden > 0 {
        out.push_str(&format!(
            "\n{hidden} more finding{} not shown.\n",
            if hidden == 1 { "" } else { "s" }
        ));
    }
    out.push_str(&format!("\n{FOOTER}\n"));
    out
}

/// The path as code, then the rest of the message as text.
fn item(finding: &Finding) -> String {
    format!("{}{}", code(&finding.path), text(&finding.detail))
}

/// An inline code span that cannot be closed early: its backtick fence is
/// longer than any backtick run inside it, and it stays on one line.
fn code(value: &str) -> String {
    let mut shown = visible(value);
    let chars = shown.chars().count();
    if chars > MAX_PATH_CHARS {
        shown = format!(
            "…{}",
            shown
                .chars()
                .skip(chars - MAX_PATH_CHARS)
                .collect::<String>()
        );
    }
    let mut longest_run = 0;
    let mut run = 0;
    for c in shown.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest_run = longest_run.max(run);
    }
    let fence = "`".repeat(longest_run + 1);
    // A span that starts or ends with a backtick needs a space against its
    // fence; CommonMark strips one space from each end when both are present.
    let pad = if shown.starts_with(['`', ' ']) || shown.ends_with(['`', ' ']) {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{shown}{pad}{fence}")
}

/// Markdown text with its inline syntax, HTML and entities escaped.
fn text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in visible(value).chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|' | '~' | '&' | '#' | '!'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escapes control characters, which could end a line or a code span, line
/// separators, characters that reorder or hide text, and the `[` of every
/// `##[`, as `\u{…}` (`\n`, `\t` and `\r` keep their short forms).
fn visible(value: &str) -> String {
    escape_where(value, |c| {
        c.is_control()
            || is_line_separator(c)
            || matches!(
                c,
                '\u{061C}'
                    | '\u{200B}'..='\u{200F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{2069}'
                    | '\u{FEFF}'
            )
    })
}
