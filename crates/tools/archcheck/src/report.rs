//! Rendering findings for people, GitHub Actions annotations, and Markdown.

mod markdown;

use std::str::FromStr;

use thiserror::Error;

use crate::{config::Mode, finding::Finding};

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

/// `findings` are expected in [`crate::check`] order, which keeps each
/// rule's behaviours contiguous.
pub fn render(format: Format, findings: &[Finding], files_checked: usize) -> String {
    let mut out = String::new();
    match format {
        Format::Human => {
            let mut group = None;
            for finding in findings {
                let key = (finding.rule, finding.behaviour, finding.mode);
                if group != Some(key) {
                    group = Some(key);
                    out.push_str(&format!(
                        "{}.{} ({}):\n",
                        finding.rule, finding.behaviour, finding.mode
                    ));
                }
                out.push_str(&format!("  {}\n", finding.message));
            }
            if !findings.is_empty() {
                out.push('\n');
            }
        }
        Format::Github => {
            for finding in findings {
                let level = match finding.mode {
                    Mode::Advisory => "warning",
                    Mode::Required => "error",
                };
                // GitHub may not show an annotation without a line on the
                // file in the pull request diff.
                out.push_str(&format!(
                    "::{level} file={},line=1,title={}::{}\n",
                    escape_property(&finding.path),
                    escape_property(&format!(
                        "archcheck {}.{} ({})",
                        finding.rule, finding.behaviour, finding.mode
                    )),
                    escape_data(&finding.message),
                ));
            }
        }
        Format::Markdown => return markdown::render(findings, files_checked),
    }
    out.push_str(&summary(findings, files_checked));
    out.push('\n');
    out
}

fn summary(findings: &[Finding], files_checked: usize) -> String {
    if findings.is_empty() {
        return format!("archcheck: no findings ({files_checked} production Rust files checked)");
    }
    let required = findings
        .iter()
        .filter(|finding| finding.mode.is_required())
        .count();
    format!(
        "archcheck: {} finding{} ({} advisory, {required} required; {files_checked} production \
         Rust files checked)",
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
