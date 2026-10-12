//! `archcheck`: check repository structure rules declared in `ARCHITECTURE.toml`.
//!
//! Exit status: 0 when no required-mode finding exists, 1 when one does, and 2
//! for configuration, usage or I/O errors.

use std::{env, ffi::OsString, path::PathBuf, process::ExitCode};

use pplx_space_archcheck::{
    BASELINE_FILE, Context, check, context::find_workspace_root, report::Format, write_baseline,
};

const USAGE: &str = "\
usage: archcheck [--root <path>] [--format human|github] [--write-baseline]

  --root <path>       repository root (default: the Cargo workspace containing
                      the current directory)
  --format <format>   human (default) or github workflow annotations
  --write-baseline    regenerate architecture-baseline.toml from the current tree";

const EXIT_REQUIRED_FINDINGS: u8 = 1;
const EXIT_ERROR: u8 = 2;

struct Options {
    root: Option<PathBuf>,
    format: Format,
    write_baseline: bool,
}

fn main() -> ExitCode {
    let options = match parse_args(env::args_os().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("archcheck: {message}\n{USAGE}");
            return ExitCode::from(EXIT_ERROR);
        }
    };
    match run(options) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("archcheck: error: {message}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

fn run(options: Options) -> Result<ExitCode, String> {
    let root = match options.root {
        Some(root) => root,
        None => {
            let cwd = env::current_dir()
                .map_err(|err| format!("could not read the current directory: {err}"))?;
            find_workspace_root(&cwd).map_err(|err| err.to_string())?
        }
    };

    if options.write_baseline {
        let baseline = write_baseline(&root).map_err(|err| err.to_string())?;
        println!(
            "archcheck: wrote {} file_size entries to {}",
            baseline.file_size.len(),
            root.join(BASELINE_FILE).display()
        );
        return Ok(ExitCode::SUCCESS);
    }

    let ctx = Context::load(&root).map_err(|err| err.to_string())?;
    let findings = check(&ctx);
    print!(
        "{}",
        pplx_space_archcheck::report::render(options.format, &findings, ctx.files.len())
    );
    if findings.iter().any(|finding| finding.mode.is_required()) {
        Ok(ExitCode::from(EXIT_REQUIRED_FINDINGS))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// `Ok(None)` asks for help.
fn parse_args(args: impl Iterator<Item = OsString>) -> Result<Option<Options>, String> {
    let mut options = Options {
        root: None,
        format: Format::Human,
        write_baseline: false,
    };
    let mut args = args.map(|arg| {
        arg.into_string()
            .map_err(|arg| format!("argument {arg:?} is not valid UTF-8"))
    });
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
                .unwrap_or_else(|| Err(format!("{flag} needs a value"))),
        };
        match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "--root" => options.root = Some(PathBuf::from(value()?)),
            "--format" => options.format = value()?.parse()?,
            "--write-baseline" if inline_value.is_none() => options.write_baseline = true,
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    Ok(Some(options))
}
