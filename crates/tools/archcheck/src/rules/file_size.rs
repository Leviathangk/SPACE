//! A code-line limit on production Rust files.
//!
//! `no_crossing` keeps files outside the baseline under the limit;
//! `listed_no_growth` keeps every baseline entry from growing past its cap;
//! `stale_entries` asks for an entry to be lowered or removed once its file
//! shrinks, falls under the limit, or is gone. The last two are separate so
//! that requiring no growth does not also fail the change that shrinks a file.

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
const STALE_ENTRIES: &str = "stale_entries";

impl Rule for FileSize {
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

        let growth_mode = config.listed_no_growth.mode;
        let stale_mode = config.stale_entries.mode;
        let mut stale = Vec::new();
        for (path, &cap) in caps {
            match ctx.file(path).map(|file| file.code_lines) {
                None => stale.push(finding(
                    STALE_ENTRIES,
                    stale_mode,
                    path,
                    format!(
                        "{path}: listed in {BASELINE_FILE} but no longer a production Rust \
                         file. Remove the entry; a renamed file is checked under its new path."
                    ),
                )),
                Some(lines) if lines <= limit => stale.push(finding(
                    STALE_ENTRIES,
                    stale_mode,
                    path,
                    format!(
                        "{path} is down to {lines} code lines, within the {}-line limit. Remove \
                         it from {BASELINE_FILE}.",
                        thousands(limit),
                    ),
                )),
                Some(lines) if lines > cap => {
                    let advice = if growth_mode.is_required() {
                        "Split the file before adding to it."
                    } else {
                        "Once this rule is required, split the file before adding to it."
                    };
                    findings.push(finding(
                        LISTED_NO_GROWTH,
                        growth_mode,
                        path,
                        format!("{path} grew from {cap} to {lines} code lines. {advice}"),
                    ));
                }
                Some(lines) if lines < cap => stale.push(finding(
                    STALE_ENTRIES,
                    stale_mode,
                    path,
                    format!(
                        "{path} shrank from {cap} to {lines} code lines. Lower its cap in \
                         {BASELINE_FILE} to {lines}."
                    ),
                )),
                Some(_) => {}
            }
        }
        // Each behaviour's findings stay contiguous for the reports.
        findings.append(&mut stale);

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
