//! Rendering findings for people and for GitHub Actions annotations.

use std::str::FromStr;

use crate::{config::Mode, finding::Finding};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Findings grouped by rule and behaviour, then a one-line summary.
    Human,
    /// `::warning`/`::error` workflow commands, then the summary.
    Github,
}

impl FromStr for Format {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "human" => Ok(Self::Human),
            "github" => Ok(Self::Github),
            other => Err(format!(
                "unknown format {other:?}; expected human or github"
            )),
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
                out.push_str(&format!(
                    "::{level} file={},title={}::{}\n",
                    escape_property(&finding.path),
                    escape_property(&format!(
                        "archcheck {}.{} ({})",
                        finding.rule, finding.behaviour, finding.mode
                    )),
                    escape_data(&finding.message),
                ));
            }
        }
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
