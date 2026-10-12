//! `archcheck`: check repository structure rules declared in `ARCHITECTURE.toml`.
//!
//! Exit status: 0 when no required-mode finding exists, 1 when one does, and 2
//! for configuration, usage or I/O errors.

use std::{
    env,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::ExitCode,
};

use pplx_space_archcheck::{
    BASELINE_FILE, Context, LoadError, WriteBaselineError, check,
    context::find_workspace_root,
    escape::log_line,
    report::{Format, UnknownFormatError, render},
    write_baseline,
};
use thiserror::Error;

const USAGE: &str = "\
usage: archcheck [--root <path>] [--format human|github|markdown] [--summary <path>]
       archcheck [--root <path>] --write-baseline

  --root <path>       repository root (default: the Cargo workspace containing
                      the current directory)
  --format <format>   human (default), github workflow annotations, or markdown
  --summary <path>    also write the markdown report to <path>
  --write-baseline    regenerate architecture-baseline.toml from the current tree";

const EXIT_REQUIRED_FINDINGS: u8 = 1;
const EXIT_ERROR: u8 = 2;

/// A command line that cannot be run; printed with the usage text.
#[derive(Debug, Error)]
enum UsageError {
    #[error("argument {0:?} is not valid UTF-8")]
    NonUtf8Argument(OsString),
    #[error("{0} needs a value")]
    MissingValue(String),
    #[error("unexpected argument {0:?}")]
    UnexpectedArgument(String),
    #[error(transparent)]
    Format(#[from] UnknownFormatError),
    #[error("--summary cannot be combined with --write-baseline")]
    SummaryWithWriteBaseline,
}

#[derive(Debug, Error)]
enum RunError {
    #[error("could not read the current directory: {0}")]
    CurrentDir(#[source] io::Error),
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error(transparent)]
    WriteBaseline(#[from] WriteBaselineError),
    #[error("could not write {path}: {source}")]
    WriteSummary {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

struct Options {
    root: Option<PathBuf>,
    format: Format,
    summary: Option<PathBuf>,
    write_baseline: bool,
}

fn main() -> ExitCode {
    let options = match parse_args(env::args_os().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(err) => {
            eprintln!("{}\n{USAGE}", log_line(&format!("archcheck: {err}")));
            return ExitCode::from(EXIT_ERROR);
        }
    };
    match run(options) {
        Ok(code) => code,
        Err(err) => {
            // Errors quote paths, member names and TOML keys from the tree.
            eprintln!("{}", log_line(&format!("archcheck: error: {err}")));
            ExitCode::from(EXIT_ERROR)
        }
    }
}

fn run(options: Options) -> Result<ExitCode, RunError> {
    let root = match options.root {
        Some(root) => root,
        None => find_workspace_root(&env::current_dir().map_err(RunError::CurrentDir)?)?,
    };

    if options.write_baseline {
        let baseline = write_baseline(&root)?;
        println!(
            "{}",
            log_line(&format!(
                "archcheck: wrote {} file_size entries to {}",
                baseline.file_size.len(),
                root.join(BASELINE_FILE).display()
            ))
        );
        return Ok(ExitCode::SUCCESS);
    }

    let ctx = Context::load(&root)?;
    let findings = check(&ctx);
    print!("{}", render(options.format, &findings, ctx.files.len()));
    if let Some(path) = &options.summary {
        write_summary(path, &render(Format::Markdown, &findings, ctx.files.len()))?;
    }
    if findings.iter().any(|finding| finding.mode.is_required()) {
        Ok(ExitCode::from(EXIT_REQUIRED_FINDINGS))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn write_summary(path: &Path, report: &str) -> Result<(), RunError> {
    std::fs::write(path, report).map_err(|source| RunError::WriteSummary {
        path: path.to_path_buf(),
        source,
    })
}

/// `Ok(None)` asks for help.
fn parse_args(args: impl Iterator<Item = OsString>) -> Result<Option<Options>, UsageError> {
    let mut options = Options {
        root: None,
        format: Format::Human,
        summary: None,
        write_baseline: false,
    };
    let mut args = args.map(|arg| arg.into_string().map_err(UsageError::NonUtf8Argument));
    while let Some(arg) = args.next() {
        let arg = arg?;
        let (flag, inline_value) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag.to_owned(), Some(value.into())),
            _ => (arg, None),
        };
        let mut value = || match inline_value.clone() {
            Some(value) => Ok(value),
            None => args
                .next()
                .unwrap_or_else(|| Err(UsageError::MissingValue(flag.clone()))),
        };
        match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "--root" => options.root = Some(PathBuf::from(value()?)),
            "--format" => options.format = value()?.parse()?,
            "--summary" => options.summary = Some(PathBuf::from(value()?)),
            "--write-baseline" if inline_value.is_none() => options.write_baseline = true,
            other => return Err(UsageError::UnexpectedArgument(other.to_owned())),
        }
    }
    if options.write_baseline && options.summary.is_some() {
        return Err(UsageError::SummaryWithWriteBaseline);
    }
    Ok(Some(options))
}
