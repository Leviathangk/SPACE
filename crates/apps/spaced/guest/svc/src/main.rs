use clap::{ArgAction, Parser};
use pplx_space_spaced_guest_server::SpacedGuestRunConfig;

#[derive(Parser)]
#[command(
    name = "spaced-svc",
    about = "SPACE guest daemon",
    version,
    disable_version_flag = true
)]
struct Cli {
    /// Print version.
    #[arg(
        short = 'v',
        long = "version",
        action = ArgAction::Version,
        required = false
    )]
    _version: (),

    /// Hold the calling unit's cgroup open and do nothing else, for
    /// `space-workloads.service`: systemd removes an empty cgroup, and this
    /// binary is in every guest, where `sleep` may not be.
    #[arg(long = "hold-cgroup", hide = true)]
    hold_cgroup: bool,

    #[arg(long, hide = true)]
    runtime_runner: bool,

    #[arg(long, hide = true)]
    runtime_inspect: bool,

    #[arg(long, hide = true)]
    runtime_unit: bool,
}

impl From<Cli> for SpacedGuestRunConfig {
    fn from(_cli: Cli) -> Self {
        // The guest daemon takes no flags: the microVM's environment is its
        // configuration, and the library reads it.
        Self::default()
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if cli.runtime_unit {
        use std::io::Write;
        std::io::stdout()
            .write_all(pplx_space_spaced_guest_server::template_runtime::SYSTEMD_UNIT)?;
        return Ok(());
    }
    if cli.runtime_inspect {
        use std::io::Write;
        let bytes = pplx_space_spaced_guest_server::template_runtime::inspect_template_runtime()?;
        std::io::stdout().write_all(&bytes)?;
        return Ok(());
    }
    if cli.runtime_runner {
        return Ok(pplx_space_spaced_guest_server::template_runtime::run_runtime_launcher()?);
    }
    if cli.hold_cgroup {
        // No runtime, telemetry or cgroup setup: SIGTERM from the unit's stop
        // ends the process with the default disposition.
        loop {
            std::thread::park();
        }
    }
    Ok(pplx_space_spaced_guest_server::run(cli.into())?)
}
