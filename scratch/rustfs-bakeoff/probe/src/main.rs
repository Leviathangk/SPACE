//! Object-store bake-off probe.
//!
//! Each subcommand runs one criterion against an S3 endpoint and prints a
//! Markdown result table. SPACE's own requests go through
//! `pplx-space-object-store` (the client volume-manager and build-manager
//! use); the AWS SDK is used directly only for requests that client never
//! issues (ListObjectsV2, DeleteObjects, explicit aborts, deliberately wrong
//! digests, STS). All data is synthetic.

mod behaviours;
mod burst;
mod common;
mod digest;
mod exchange;
mod freeze;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Kind object-store bake-off probe")]
struct Cli {
    /// S3 endpoint, e.g. http://127.0.0.1:19000.
    #[arg(long, env = "BAKEOFF_ENDPOINT", global = true, default_value = "")]
    endpoint: String,
    #[arg(long, default_value = "us-east-1", global = true)]
    region: String,
    /// Admin (root) access key: creates buckets and seeds objects.
    #[arg(long, env = "BAKEOFF_ACCESS_KEY", global = true, default_value = "")]
    access_key: String,
    #[arg(long, env = "BAKEOFF_SECRET_KEY", global = true, default_value = "")]
    secret_key: String,
    /// Store name for the table titles.
    #[arg(long, default_value = "store", global = true)]
    label: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Criterion A: the S3 behaviours SPACE uses.
    Behaviours {
        #[arg(long, default_value = "bakeoff-a")]
        bucket: String,
    },
    /// Criterion B1: a mismatched Content-MD5 is refused and nothing is stored.
    Digest {
        #[arg(long, default_value = "bakeoff-b1")]
        bucket: String,
    },
    /// Criterion B2: freeze the store's container mid-upload.
    Freeze(freeze::FreezeArgs),
    /// Criterion C: AssumeRole and session-policy enforcement.
    Exchange(exchange::ExchangeArgs),
    /// Criterion C: a session is refused once it expires.
    Expiry(exchange::ExpiryArgs),
    /// Criterion D: concurrent upload burst.
    Burst(burst::BurstArgs),
    /// Criterion D: first write to a bucket created just before it.
    FirstWrite {
        #[arg(long, default_value_t = 5)]
        repeats: usize,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    // The object-store client reads its tuning from the environment once, at
    // construction; set it before any runtime thread exists.
    if matches!(cli.cmd, Cmd::Behaviours { .. } | Cmd::Exchange(_)) {
        common::set_small_transfer_tuning();
    }
    let capture = common::install_tracing();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let ctx: &'static common::Ctx = Box::leak(Box::new(common::Ctx::new(
        &cli.endpoint,
        &cli.region,
        &cli.access_key,
        &cli.secret_key,
        &cli.label,
        capture,
    )));
    let ok = runtime.block_on(async move {
        match cli.cmd {
            Cmd::Behaviours { bucket } => behaviours::run(ctx, &bucket).await,
            Cmd::Digest { bucket } => digest::run(ctx, &bucket).await,
            Cmd::Freeze(args) => freeze::run(ctx, args).await,
            Cmd::Exchange(args) => exchange::run(ctx, args).await,
            Cmd::Expiry(args) => exchange::run_expiry(ctx, args).await,
            Cmd::Burst(args) => burst::run(ctx, args).await,
            Cmd::FirstWrite { repeats } => burst::first_write(ctx, repeats).await,
        }
    });
    if ok {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
