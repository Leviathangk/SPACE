//! Rendering findings for people, GitHub Actions annotations, and Markdown.

mod markdown;

use std::str::FromStr;

use thiserror::Error;

use crate::{config::Mode, escape::log_line, finding::Finding};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Findings grouped by rule and behaviour, then a one-line summary.
    Human,
    /// `::warning`/`::error` workflow commands, then the summary.
    Github,
    /// A Markdown report for a job summary or pull request comment.
    Markdown,
}

#[derive(Debug, Error)]
#[error("unknown format {0:?}; expected human, github or markdown")]
pub struct UnknownFormatError(pub String);

impl FromStr for Format {
    type Err = UnknownFormatError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "human" => Ok(Self::Human),
            "github" => Ok(Self::Github),
            "markdown" => Ok(Self::Markdown),
            other => Err(UnknownFormatError(other.to_owned())),
        }
    }
}

/// GitHub shows at most 10 `::warning` and 10 `::error` annotations per step;
/// later findings are counted in one line instead.
pub const MAX_ANNOTATIONS_PER_LEVEL: usize = 10;

/// What a report shows.
#[derive(Debug, Clone, Copy)]
pub struct Report<'a> {
    /// In [`crate::check`] order, which keeps each rule's behaviours
    /// contiguous.
    pub findings: &'a [Finding],
    /// Findings left out because they are in files the change did not touch.
    pub hidden: usize,
    pub files_checked: usize,
}

pub fn render(format: Format, report: &Report<'_>) -> String {
    let findings = report.findings;
    let mut out = String::new();
    match format {
        Format::Human => {
            let mut group = None;
            for finding in findings {
                let key = (finding.rule, finding.behaviour, finding.mode);
                if group != Some(key) {
                    group = Some(key);
                    let heading =
                        format!("{}.{} ({}):", finding.rule, finding.behaviour, finding.mode);
                    out.push_str(&log_line(&heading));
                    out.push('\n');
                }
                out.push_str(&log_line(&format!("  {}", finding.message())));
                out.push('\n');
            }
            if !findings.is_empty() {
                out.push('\n');
            }
        }
        Format::Github => {
            let (mut warnings, mut errors) = (0, 0);
            for finding in findings {
                let (level, count) = match finding.mode {
                    Mode::Advisory => ("warning", &mut warnings),
                    Mode::Required => ("error", &mut errors),
                };
                *count += 1;
                if *count > MAX_ANNOTATIONS_PER_LEVEL {
                    continue;
                }
                // GitHub may not show an annotation without a line on the
                // file in the pull request diff.
                out.push_str(&format!(
                    "::{level} file={},line=1,title={}::{}\n",
                    escape_property(&finding.path),
                    escape_property(&format!(
                        "archcheck {}.{} ({})",
                        finding.rule, finding.behaviour, finding.mode
                    )),
                    escape_data(&finding.message()),
                ));
            }
            for (count, mode) in [(warnings, Mode::Advisory), (errors, Mode::Required)] {
                let rest = count.saturating_sub(MAX_ANNOTATIONS_PER_LEVEL);
                if rest > 0 {
                    out.push_str(&log_line(&format!(
                        "archcheck: {rest} more {mode} finding{} not annotated; the job summary \
                         lists them, and `cargo arch` reproduces them.",
                        if rest == 1 { "" } else { "s" },
                    )));
                    out.push('\n');
                }
            }
        }
        Format::Markdown => return markdown::render(report),
    }
    out.push_str(&log_line(&summary(report)));
    out.push('\n');
    out
}

fn summary(report: &Report<'_>) -> String {
    let Report {
        findings,
        hidden,
        files_checked,
    } = *report;
    let checked = format!("{files_checked} production Rust files checked");
    let elsewhere = match hidden {
        0 => String::new(),
        1 => "; 1 finding elsewhere in the tree not shown".to_owned(),
        _ => format!("; {hidden} findings elsewhere in the tree not shown"),
    };
    if findings.is_empty() {
        let scope = if hidden == 0 {
            ""
        } else {
            " in the changed files"
        };
        return format!("archcheck: no findings{scope} ({checked}{elsewhere})");
    }
    let required = findings
        .iter()
        .filter(|finding| finding.mode.is_required())
        .count();
    format!(
        "archcheck: {} finding{} ({} advisory, {required} required; {checked}{elsewhere})",
        findings.len(),
        if findings.len() == 1 { "" } else { "s" },
        findings.len() - required,
    )
}

/// Workflow-command message escaping.
fn escape_data(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Workflow-command property escaping, which also reserves `:` and `,`.
fn escape_property(value: &str) -> String {
    escape_data(value).replace(':', "%3A").replace(',', "%2C")
}
