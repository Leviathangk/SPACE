//! A code-line limit on production Rust files.
//!
//! `no_crossing` keeps files outside the baseline under the limit;
//! `listed_no_growth` keeps every baseline entry from growing past its cap and
//! asks for the entry to be lowered or removed once the file shrinks.

use crate::{
    baseline::{BASELINE_FILE, Baseline},
    config::Mode,
    context::Context,
    finding::Finding,
    rules::Rule,
};

pub struct FileSize;

const RULE: &str = "file_size";
const NO_CROSSING: &str = "no_crossing";
const LISTED_NO_GROWTH: &str = "listed_no_growth";

impl Rule for FileSize {
    fn id(&self) -> &'static str {
        RULE
    }

    fn check(&self, ctx: &Context) -> Vec<Finding> {
        let config = &ctx.config.rules.file_size;
        let limit = config.limit;
        let caps = &ctx.baseline.file_size;
        let mut findings = Vec::new();

        let mode = config.no_crossing.mode;
        for file in &ctx.files {
            if file.code_lines > limit && !caps.contains_key(&file.path) {
                findings.push(finding(
                    NO_CROSSING,
                    mode,
                    &file.path,
                    format!(
                        "{}: {} code lines, over the {}-line limit (comments and blank lines \
                         are not counted). Split it into submodules by responsibility.",
                        file.path,
                        file.code_lines,
                        thousands(limit),
                    ),
                ));
            }
        }

        let mode = config.listed_no_growth.mode;
        for (path, &cap) in caps {
            let message = match ctx.file(path).map(|file| file.code_lines) {
                None => format!(
                    "{path}: listed in {BASELINE_FILE} but no longer a production Rust file. \
                     Remove the entry; a renamed file is checked under its new path."
                ),
                Some(lines) if lines <= limit => format!(
                    "{path} is down to {lines} code lines, within the {}-line limit. Remove it \
                     from {BASELINE_FILE}.",
                    thousands(limit),
                ),
                Some(lines) if lines > cap => {
                    let advice = if mode.is_required() {
                        "Split the file before adding to it."
                    } else {
                        "Once this rule is required, split the file before adding to it."
                    };
                    format!("{path} grew from {cap} to {lines} code lines. {advice}")
                }
                Some(lines) if lines < cap => format!(
                    "{path} shrank from {cap} to {lines} code lines. Lower its cap in \
                     {BASELINE_FILE} to {lines}."
                ),
                Some(_) => continue,
            };
            findings.push(finding(LISTED_NO_GROWTH, mode, path, message));
        }

        findings
    }

    fn update_baseline(&self, ctx: &Context, baseline: &mut Baseline) {
        let limit = ctx.config.rules.file_size.limit;
        baseline.file_size = ctx
            .files
            .iter()
            .filter(|file| file.code_lines > limit)
            .map(|file| (file.path.clone(), file.code_lines))
            .collect();
    }
}

fn finding(behaviour: &'static str, mode: Mode, path: &str, message: String) -> Finding {
    Finding {
        rule: RULE,
        behaviour,
        path: path.to_owned(),
        message,
        mode,
    }
}

/// `1000` as `1,000`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}
