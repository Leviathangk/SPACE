//! Shared plumbing: clients, synthetic data, error rendering, tables, and a
//! tracing layer that records the object-store client's failed attempts.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aws_credential_types::Credentials;
use aws_sdk_s3::config::{BehaviorVersion, Region, RequestChecksumCalculation};
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use pplx_space_object_store::{ObjectCredentials, ObjectTarget, S3ObjectStore};
use sha2::{Digest as _, Sha256};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt as _};
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer};

pub const MIB: usize = 1024 * 1024;

pub struct Ctx {
    pub endpoint: String,
    pub region: String,
    pub admin: ObjectCredentials,
    pub label: String,
    pub store: S3ObjectStore,
    pub attempts: AttemptLog,
}

impl Ctx {
    pub fn new(
        endpoint: &str,
        region: &str,
        access_key: &str,
        secret_key: &str,
        label: &str,
        attempts: AttemptLog,
    ) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            region: region.to_string(),
            admin: creds(access_key, secret_key, None),
            label: label.to_string(),
            store: S3ObjectStore::new(),
            attempts,
        }
    }

    pub fn target<'a>(&'a self, bucket: &'a str, key: &'a str) -> ObjectTarget<'a> {
        ObjectTarget {
            bucket,
            key,
            region: &self.region,
            endpoint: Some(&self.endpoint),
            path_style: true,
        }
    }

    /// A plain SDK client for requests the object-store client never makes.
    /// Same addressing and checksum policy as the object-store client.
    pub fn sdk(&self, c: &ObjectCredentials) -> aws_sdk_s3::Client {
        let conf = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(self.region.clone()))
            .endpoint_url(&self.endpoint)
            .force_path_style(true)
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .credentials_provider(Credentials::new(
                c.access_key_id.clone(),
                c.secret_access_key.clone(),
                c.session_token.clone(),
                None,
                "bakeoff",
            ))
            .build();
        aws_sdk_s3::Client::from_conf(conf)
    }

    pub async fn ensure_bucket(&self, bucket: &str) -> Result<(), String> {
        let s3 = self.sdk(&self.admin);
        if s3.head_bucket().bucket(bucket).send().await.is_ok() {
            return Ok(());
        }
        s3.create_bucket()
            .bucket(bucket)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| sdk_err(&e))
    }
}

pub fn creds(ak: &str, sk: &str, token: Option<String>) -> ObjectCredentials {
    ObjectCredentials {
        access_key_id: ak.to_string(),
        secret_access_key: sk.to_string(),
        session_token: token,
    }
}

/// Small parts and chunks, so modest files exercise multipart uploads and
/// ranged If-Match downloads.
pub fn set_small_transfer_tuning() {
    // SAFETY: called from `main` before any other thread exists.
    unsafe {
        std::env::set_var("SPACE_OBJECT_STORE_MULTIPART_PART_SIZE_BYTES", (8 * MIB).to_string());
        std::env::set_var("SPACE_OBJECT_STORE_DOWNLOAD_CHUNK_SIZE_BYTES", (8 * MIB).to_string());
        std::env::set_var("SPACE_OBJECT_STORE_DOWNLOAD_READER_CONCURRENCY", "4");
    }
}

/// `HTTP <status> <code>` for any SDK error.
pub fn sdk_err<E, R>(e: &SdkError<E, R>) -> String
where
    E: ProvideErrorMetadata,
    R: HttpStatus,
{
    let status = e.raw_response().map(|r| r.status_code()).unwrap_or(0);
    let code = e.code().unwrap_or(match e {
        SdkError::TimeoutError(_) => "timeout",
        SdkError::DispatchFailure(_) => "dispatch-failure",
        SdkError::ResponseError(_) => "response-error",
        SdkError::ConstructionFailure(_) => "construction-failure",
        _ => "-",
    });
    format!("HTTP {status} {code}")
}

pub trait HttpStatus {
    fn status_code(&self) -> u16;
}

impl HttpStatus for aws_smithy_runtime_api::http::Response {
    fn status_code(&self) -> u16 {
        self.status().as_u16()
    }
}

/// Deterministic pseudo-random bytes (splitmix64), so runs are repeatable.
pub fn synthetic(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 8);
    let mut state = seed ^ 0x9e37_79b9_7f4a_7c15;
    while out.len() < len {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        out.extend_from_slice(&z.to_le_bytes());
    }
    out.truncate(len);
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub async fn sha256_file(path: &Path) -> std::io::Result<(u64, String)> {
    let data = tokio::fs::read(path).await?;
    Ok((data.len() as u64, sha256_hex(&data)))
}

pub fn scratch_dir() -> PathBuf {
    let dir = std::env::var_os("BAKEOFF_SCRATCH")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustfs-bakeoff"));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

pub async fn write_synthetic(name: &str, len: usize, seed: u64) -> (PathBuf, String) {
    let path = scratch_dir().join(name);
    let data = synthetic(len, seed);
    let sha = sha256_hex(&data);
    tokio::fs::write(&path, &data).await.expect("write scratch file");
    (path, sha)
}

pub fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{nanos:x}")
}

pub struct Row {
    pub id: String,
    pub check: String,
    pub expected: String,
    pub observed: String,
    pub pass: Option<bool>,
}

#[derive(Default)]
pub struct Table {
    pub rows: Vec<Row>,
}

impl Table {
    pub fn add(&mut self, id: &str, check: &str, expected: &str, observed: impl Into<String>, pass: Option<bool>) {
        self.rows.push(Row {
            id: id.to_string(),
            check: check.to_string(),
            expected: expected.to_string(),
            observed: observed.into(),
            pass,
        });
    }

    /// A check that passes when `observed` satisfies `ok`.
    pub fn check(&mut self, id: &str, check: &str, expected: &str, observed: impl Into<String>, ok: bool) {
        self.add(id, check, expected, observed, Some(ok));
    }

    pub fn all_pass(&self) -> bool {
        self.rows.iter().all(|r| r.pass != Some(false))
    }

    pub fn print(&self, title: &str) {
        let mut out = String::new();
        let _ = writeln!(out, "\n### {title}\n");
        let _ = writeln!(out, "| # | Check | Expected | Observed | Result |");
        let _ = writeln!(out, "|---|---|---|---|---|");
        for r in &self.rows {
            let result = match r.pass {
                Some(true) => "PASS",
                Some(false) => "FAIL",
                None => "info",
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                r.id,
                r.check,
                r.expected,
                r.observed.replace('|', "\\|").replace('\n', " "),
                result
            );
        }
        let passed = self.rows.iter().filter(|r| r.pass == Some(true)).count();
        let failed = self.rows.iter().filter(|r| r.pass == Some(false)).count();
        let _ = writeln!(out, "\n{passed} passed, {failed} failed, {} informational", self.rows.len() - passed - failed);
        println!("{out}");
    }
}

/// One failed attempt the object-store client logged.
#[derive(Clone, Debug)]
pub struct Attempt {
    pub operation: String,
    pub attempt: String,
    pub kind: String,
    pub status: String,
    pub code: String,
    pub will_retry: String,
    pub elapsed_ms: String,
    pub key: String,
}

impl Attempt {
    pub fn render(&self) -> String {
        format!(
            "{}{} #{} {} {}{} after {} ms (retry={})",
            self.operation,
            if self.key.is_empty() { String::new() } else { format!("[{}]", self.key.rsplit('/').next().unwrap_or("")) },
            self.attempt,
            self.kind,
            if self.status.is_empty() { String::new() } else { format!("HTTP {} ", self.status) },
            self.code,
            self.elapsed_ms,
            self.will_retry
        )
    }
}

#[derive(Clone, Default)]
pub struct AttemptLog(Arc<Mutex<Vec<Attempt>>>);

impl AttemptLog {
    pub fn take(&self) -> Vec<Attempt> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

struct AttemptLayer(AttemptLog);

#[derive(Default)]
struct AttemptVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl Visit for AttemptVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}").trim_matches('"').to_string();
        if field.name() == "message" {
            self.message = rendered;
        } else {
            self.fields.push((field.name().to_string(), rendered));
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push((field.name().to_string(), value.to_string()));
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for AttemptLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut v = AttemptVisitor::default();
        event.record(&mut v);
        if v.message != "S3 request attempt failed" {
            return;
        }
        let get = |name: &str| {
            v.fields
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, val)| val.trim_start_matches("Some(").trim_end_matches(')').to_string())
                .unwrap_or_default()
        };
        self.0.0.lock().unwrap().push(Attempt {
            operation: get("operation"),
            attempt: get("attempt"),
            kind: get("failure_kind"),
            status: get("http_status"),
            code: get("error_code"),
            will_retry: get("will_retry"),
            elapsed_ms: get("elapsed_ms"),
            key: get("key"),
        });
    }
}

/// Installs a stderr logger (RUST_LOG, default `warn`) plus the attempt
/// recorder, and returns the recorder's handle.
pub fn install_tracing() -> AttemptLog {
    let log = AttemptLog::default();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::registry()
        .with(AttemptLayer(log.clone()).with_filter(tracing_subscriber::filter::Targets::new().with_target("pplx_space_object_store", tracing::Level::WARN)))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_filter(filter))
        .init();
    log
}

/// Condenses an object-store client error to its HTTP status and code.
pub fn short_err(e: &str) -> String {
    let status = e.split("StatusCode(").nth(1).and_then(|r| r.split(')').next());
    let code = e.split("code: Some(\"").nth(1).and_then(|r| r.split('"').next());
    let kind = e.split(':').next().unwrap_or("");
    match status {
        Some(st) => format!("{kind}: HTTP {st}{}", code.map(|c| format!(" {c}")).unwrap_or_default()),
        None => e.chars().take(160).collect(),
    }
}

pub fn ms(d: std::time::Duration) -> String {
    format!("{:.0} ms", d.as_secs_f64() * 1000.0)
}

pub fn secs(d: std::time::Duration) -> String {
    format!("{:.2} s", d.as_secs_f64())
}
