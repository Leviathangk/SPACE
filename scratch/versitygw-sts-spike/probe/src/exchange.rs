//! Criterion C: what the credential-manager authority's storage exchange
//! needs from the store. The session policies are built exactly as
//! `AuthorizedExchange::session_policy`
//! (crates/libs/credential/mgr/server/src/storage_exchange.rs) builds them,
//! and AssumeRole is called the way `assume_s3_vend_role` calls it: session
//! name `space-storage-exchange`, `DurationSeconds`, `Policy`, signed with the
//! vend identity's keys.
//!
//! `--sts-mode` picks where sessions come from: `web-identity` calls
//! versitygw's AssumeRoleWithWebIdentity directly with a token from `--mint`
//! (the bridge's signer and claims); `bridge` calls AssumeRole on the bridge
//! with the SDK's STS client, as the bake-off called RustFS; `provider` runs
//! the authority's `assume_s3_vend_role` client code verbatim against the
//! bridge, configured from the environment as the authority is.

use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::Credentials;
use aws_sdk_s3::primitives::ByteStream;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use bytes::Bytes;
use clap::Args;
use hmac::{KeyInit as _, Mac as _};
use pplx_space_object_store::{ObjectCredentials, ObjectStore};

use crate::behaviours::put_stream_with;
use crate::common::{self, Ctx, MIB, Table, creds, sdk_err, sha256_hex, synthetic};

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum StsMode {
    WebIdentity,
    Bridge,
    Provider,
}

#[derive(Args, Clone)]
pub struct ExchangeArgs {
    #[arg(long, env = "BAKEOFF_VEND_ACCESS_KEY")]
    pub vend_access_key: String,
    #[arg(long, env = "BAKEOFF_VEND_SECRET_KEY")]
    pub vend_secret_key: String,
    #[arg(long, default_value = "arn:aws:iam::000000000000:role/kind-storage-vend")]
    pub role_arn: String,
    #[arg(long, default_value = "snapshots")]
    pub bucket: String,
    #[arg(long, default_value = "templates")]
    pub other_bucket: String,
    /// A bucket outside the vend identity's policy.
    #[arg(long, default_value = "bakeoff-unvended")]
    pub foreign_bucket: String,
    #[arg(long, value_enum, default_value = "bridge")]
    pub sts_mode: StsMode,
    /// versitygw's IAM/STS endpoint (AssumeRoleWithWebIdentity).
    #[arg(long, env = "SPIKE_IAM_ENDPOINT", default_value = "")]
    pub iam_endpoint: String,
    /// The bridge's STS endpoint (AssumeRole).
    #[arg(long, env = "SPIKE_BRIDGE_ENDPOINT", default_value = "")]
    pub bridge_endpoint: String,
    /// Command printing one token; `-iss/-sub/-aud` and any overrides are appended.
    #[arg(long, env = "SPIKE_MINT", default_value = "")]
    pub mint: String,
    #[arg(long, env = "SPIKE_ISSUER", default_value = "")]
    pub issuer: String,
    #[arg(long, env = "SPIKE_AUDIENCE", default_value = "")]
    pub audience: String,
    #[arg(long, env = "SPIKE_SUBJECT", default_value = "")]
    pub subject: String,
}

impl ExchangeArgs {
    /// The authority's STS client reads its keys, region and endpoint from
    /// the environment (`AWS_ENDPOINT_URL_STS` in Kind); `provider` mode
    /// does the same.
    pub fn export_provider_env(&self, region: &str) {
        if self.sts_mode != StsMode::Provider {
            return;
        }
        // SAFETY: called from `main` before any other thread exists.
        unsafe {
            std::env::set_var("AWS_ACCESS_KEY_ID", &self.vend_access_key);
            std::env::set_var("AWS_SECRET_ACCESS_KEY", &self.vend_secret_key);
            std::env::remove_var("AWS_SESSION_TOKEN");
            std::env::remove_var("AWS_PROFILE");
            std::env::set_var("AWS_REGION", region);
            std::env::set_var("AWS_ENDPOINT_URL_STS", &self.bridge_endpoint);
        }
    }

    /// A token from the bridge's signer with the bridge's claims, unless
    /// `overrides` (mint flags, later ones win) change them.
    pub fn mint_token(&self, overrides: &[&str]) -> Result<String, String> {
        let mut words = self.mint.split_whitespace();
        let program = words.next().ok_or("no --mint command")?;
        let out = std::process::Command::new(program)
            .args(words)
            .args(["-iss", &self.issuer, "-sub", &self.subject, "-aud", &self.audience])
            .args(overrides)
            .output()
            .map_err(|e| format!("mint: {e}"))?;
        if !out.status.success() {
            return Err(format!("mint failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

#[derive(Args)]
pub struct ExpiryArgs {
    #[command(flatten)]
    pub base: ExchangeArgs,
    /// Durations to request, in order; the first one the store accepts is
    /// used for the wait.
    #[arg(long, value_delimiter = ',', default_value = "1,60,900")]
    durations: Vec<i32>,
    #[arg(long, default_value_t = 30)]
    past_expiry_secs: u64,
}

#[derive(Args)]
pub struct SessionArgs {
    #[command(flatten)]
    pub base: ExchangeArgs,
    /// The prefix the session may read and write, in the authority's prefix shape.
    #[arg(long)]
    pub prefix: String,
}

#[derive(Clone, Copy)]
pub enum Access {
    Read,
    ReadWrite,
}

/// `AuthorizedExchange::session_policy`, verbatim in shape.
pub fn session_policy(bucket: &str, key: &str, prefix: bool, access: Access) -> String {
    let mut actions = vec!["s3:GetObject"];
    if let Access::ReadWrite = access {
        actions.push("s3:PutObject");
        actions.push("s3:AbortMultipartUpload");
    }
    let resource = if prefix { format!("arn:aws:s3:::{bucket}/{key}/*") } else { format!("arn:aws:s3:::{bucket}/{key}") };
    let condition = if prefix {
        serde_json::json!({ "StringLike": { "s3:prefix": [key, format!("{key}/*")] } })
    } else {
        serde_json::json!({ "StringEquals": { "s3:prefix": key } })
    };
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": actions, "Resource": resource },
            { "Effect": "Allow", "Action": "s3:ListBucket", "Resource": format!("arn:aws:s3:::{bucket}"), "Condition": condition },
        ],
    })
    .to_string()
}

pub fn sts_config(ctx: &Ctx, endpoint: &str) -> aws_sdk_sts::config::Builder {
    aws_sdk_sts::Config::builder()
        .behavior_version(aws_sdk_sts::config::BehaviorVersion::latest())
        .region(aws_sdk_sts::config::Region::new(ctx.region.clone()))
        .endpoint_url(endpoint)
}

fn sts(ctx: &Ctx, endpoint: &str, ak: &str, sk: &str) -> aws_sdk_sts::Client {
    let conf = sts_config(ctx, endpoint).credentials_provider(Credentials::new(ak, sk, None, None, "bakeoff-vend")).build();
    aws_sdk_sts::Client::from_conf(conf)
}

pub struct Session {
    pub creds: ObjectCredentials,
    pub expires_in: i64,
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64
}

pub async fn assume(ctx: &Ctx, a: &ExchangeArgs, role: Option<&str>, policy: Option<String>, duration: i32) -> Result<Session, String> {
    match a.sts_mode {
        StsMode::Bridge => assume_sdk(ctx, a, role, policy, duration).await,
        StsMode::WebIdentity => {
            let token = a.mint_token(&[])?;
            assume_web_identity(ctx, a, role, policy, duration, &token).await
        }
        StsMode::Provider => authority_assume(role.unwrap_or_default(), policy, duration).await,
    }
}

/// AssumeRoleWithWebIdentity at versitygw: unsigned, as STS defines it.
pub async fn assume_web_identity(ctx: &Ctx, a: &ExchangeArgs, role: Option<&str>, policy: Option<String>, duration: i32, token: &str) -> Result<Session, String> {
    let client = aws_sdk_sts::Client::from_conf(sts_config(ctx, &a.iam_endpoint).build());
    let out = client
        .assume_role_with_web_identity()
        .set_role_arn(role.map(str::to_string))
        .role_session_name("space-storage-exchange")
        .web_identity_token(token)
        .duration_seconds(duration)
        .set_policy(policy)
        .send()
        .await
        .map_err(|e| sdk_err(&e))?;
    let c = out.credentials().ok_or("no credentials in response")?;
    Ok(Session {
        creds: creds(c.access_key_id(), c.secret_access_key(), Some(c.session_token().to_string())),
        expires_in: c.expiration().secs() - now_unix(),
    })
}

/// `assume_s3_vend_role` (crates/libs/credential/mgr/server/src/service.rs),
/// minus its tracing: the authority's own STS client path.
pub async fn authority_assume(role_arn: &str, session_policy: Option<String>, ttl_seconds: i32) -> Result<Session, String> {
    use aws_credential_types::provider::ProvideCredentials as _;
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let mut builder = aws_config::sts::AssumeRoleProvider::builder(role_arn)
        .session_name("space-storage-exchange")
        .session_length(Duration::from_secs(ttl_seconds.max(0) as u64));
    if let Some(p) = session_policy {
        builder = builder.policy(p);
    }
    let c = builder
        .configure(&config)
        .build()
        .await
        .provide_credentials()
        .await
        .map_err(|e| common::short_provider_err(&aws_smithy_types::error::display::DisplayErrorContext(&e).to_string()))?;
    let expiry = c.expiry().ok_or("no expiry")?.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64;
    Ok(Session {
        creds: creds(c.access_key_id(), c.secret_access_key(), c.session_token().map(str::to_string)),
        expires_in: expiry - now_unix(),
    })
}

async fn assume_sdk(ctx: &Ctx, a: &ExchangeArgs, role: Option<&str>, policy: Option<String>, duration: i32) -> Result<Session, String> {
    let mut req = sts(ctx, &a.bridge_endpoint, &a.vend_access_key, &a.vend_secret_key)
        .assume_role()
        .role_session_name("space-storage-exchange")
        .duration_seconds(duration)
        .set_policy(policy);
    if let Some(r) = role {
        req = req.role_arn(r);
    }
    let out = req.send().await.map_err(|e| sdk_err(&e))?;
    let c = out.credentials().ok_or("no credentials in response")?;
    Ok(Session {
        creds: creds(c.access_key_id(), c.secret_access_key(), Some(c.session_token().to_string())),
        expires_in: c.expiration().secs() - now_unix(),
    })
}

pub fn denied(r: &Result<String, String>) -> bool {
    matches!(r, Err(e) if e.contains("403") || e.to_lowercase().contains("denied"))
}

/// Refused in any way that serves nothing (RustFS answers a bad session token
/// with 500 InternalError rather than AWS's 403; the table shows which).
pub fn refused_any(r: &Result<String, String>) -> bool {
    matches!(r, Err(e) if e.contains("HTTP 4") || e.contains("HTTP 5") || e.to_lowercase().contains("denied"))
}

pub fn show(r: &Result<String, String>) -> String {
    match r {
        Ok(s) => format!("allowed ({s})"),
        Err(e) => format!("refused ({e})"),
    }
}

pub async fn run(ctx: &Ctx, a: ExchangeArgs) -> bool {
    let mut t = Table::default();
    for b in [&a.bucket, &a.other_bucket, &a.foreign_bucket] {
        if let Err(e) = ctx.ensure_bucket(b).await {
            t.check("C0", &format!("create bucket {b}"), "200", e, false);
        }
    }
    let admin = ctx.sdk(&ctx.admin);
    let sbx_a = format!("{}-aaaa-4aaa-8aaa-aaaaaaaaaaaa", &common::unique("a")[2..10]);
    let sbx_b = format!("{}-bbbb-4bbb-8bbb-bbbbbbbbbbbb", &common::unique("b")[2..10]);
    let snap = "11111111-2222-4333-8444-555555555555";
    let key_a = format!("suspend_snapshots/07/{sbx_a}/{snap}/vm_state");
    let key_b = format!("suspend_snapshots/07/{sbx_b}/{snap}/vm_state");
    let prefix_a = format!("suspend_snapshots/07/{sbx_a}/{snap}/artifacts");
    // Seed B's object (and one in the other bucket) with admin keys.
    let seed = synthetic(64 * 1024, 21);
    for (b, k) in [(&a.bucket, &key_b), (&a.other_bucket, &key_a), (&a.bucket, &format!("{key_a}x")), (&a.bucket, &format!("{prefix_a}x/escape"))] {
        let _ = ctx.store.put_bytes(&ctx.admin, ctx.target(b, k), Bytes::from(seed.clone())).await;
    }

    // C1 AssumeRole with Policy and DurationSeconds.
    let rw = assume(ctx, &a, Some(&a.role_arn), Some(session_policy(&a.bucket, &key_a, false, Access::ReadWrite)), 3600).await;
    let Ok(rw) = rw else {
        let e = rw.err().unwrap_or_default();
        t.check("C1", "AssumeRole (Policy, DurationSeconds 3600) signed by the vend identity", "credentials, ~3600 s", e, false);
        t.print(&format!("C. Exchange requirements — {}", ctx.label));
        return false;
    };
    t.check("C1", "AssumeRole (Policy, DurationSeconds 3600) signed by the vend identity", "credentials, expiry ~3600 s", format!("session expires in {} s", rw.expires_in), (3500..=3700).contains(&rw.expires_in));
    let s = &rw.creds;

    // Exact-object read-write session.
    let file_len = 20 * MIB;
    let (path, sha) = common::write_synthetic("c-file.bin", file_len, 22).await;
    let r = ctx.store.put_file(s, ctx.target(&a.bucket, &key_a), &path).await.map(|n| format!("{n} bytes")).map_err(|e| common::short_err(&e.to_string()));
    t.check("C2", "exact-object RW: put_file multipart (Create/UploadPart/Complete under s3:PutObject)", "allowed", show(&r), r.is_ok());
    let r = put_stream_with(ctx, s, &a.bucket, &key_a, synthetic(12 * MIB, 23), 5 * MIB, 4).await;
    t.check("C3", "exact-object RW: put_stream 12 MiB in 5 MiB parts", "allowed", r.clone(), r.starts_with("Ok"));
    let _ = ctx.store.put_file(s, ctx.target(&a.bucket, &key_a), &path).await;
    let r = ctx.store.head(s, ctx.target(&a.bucket, &key_a)).await.map(|m| format!("{m:?}")).map_err(|e| common::short_err(&e.to_string()));
    t.check("C4", "exact-object RW: HeadObject (under s3:GetObject)", "allowed", show(&r), matches!(&r, Ok(m) if m.contains("Some")));
    let dst = common::scratch_dir().join("c-file.down");
    let r = ctx.store.get_file(s, ctx.target(&a.bucket, &key_a), &dst).await.map_err(|e| common::short_err(&e.to_string()));
    let same = common::sha256_file(&dst).await.map(|(_, h)| h == sha).unwrap_or(false);
    t.check("C5", "exact-object RW: get_file (ranged If-Match GETs)", "allowed, identical", format!("{}, identical = {same}", show(&r.map(|n| format!("{n} bytes")))), same);
    let r = list(ctx, s, &a.bucket, Some(&key_a)).await;
    t.check("C6", "ListObjectsV2 prefix = the object key (StringEquals s3:prefix)", "allowed", show(&r), r.is_ok());
    let r = list(ctx, s, &a.bucket, Some(&format!("suspend_snapshots/07/{sbx_a}/"))).await;
    t.check("C7", "ListObjectsV2 prefix = the sandbox's directory", "refused", show(&r), denied(&r));
    let r = list(ctx, s, &a.bucket, None).await;
    t.check("C8", "ListObjectsV2 with no prefix", "refused", show(&r), denied(&r));
    let r = list(ctx, s, &a.bucket, Some(&key_b)).await;
    t.check("C9", "ListObjectsV2 prefix = another sandbox's key", "refused", show(&r), denied(&r));
    let r = mpu_abort(ctx, s, &a.bucket, &key_a).await;
    t.check("C10", "CreateMultipartUpload + AbortMultipartUpload on the object", "allowed", show(&r), r.is_ok());
    for (id, what, k) in [("C11", "another sandbox's object", key_b.clone()), ("C12", "a key extending the object's name", format!("{key_a}x")), ("C13", "a key under the object's name", format!("{key_a}/child"))] {
        let r = ctx.store.head(s, ctx.target(&a.bucket, &k)).await.map(|m| format!("{m:?}")).map_err(|e| common::short_err(&e.to_string()));
        t.check(id, &format!("HeadObject on {what}"), "refused", show(&r), denied(&r));
        let r = ctx.store.put_bytes(s, ctx.target(&a.bucket, &k), Bytes::from_static(b"overwrite")).await.map(|_| "stored".to_string()).map_err(|e| common::short_err(&e.to_string()));
        t.check(&format!("{id}w"), &format!("PutObject on {what}"), "refused", show(&r), denied(&r));
    }
    let r = ctx.store.get_bytes(s, ctx.target(&a.other_bucket, &key_a)).await.map(|b| format!("{} bytes", b.len())).map_err(|e| common::short_err(&e.to_string()));
    t.check("C14", "GetObject on the same key in the other vended bucket", "refused", show(&r), denied(&r));
    let r = ctx.store.put_bytes(s, ctx.target(&a.other_bucket, &key_a), Bytes::from_static(b"x")).await.map(|_| "stored".to_string()).map_err(|e| common::short_err(&e.to_string()));
    t.check("C14w", "PutObject on the same key in the other vended bucket", "refused", show(&r), denied(&r));
    let r = ctx.sdk(s).delete_object().bucket(&a.bucket).key(&key_a).send().await.map(|_| "deleted".to_string()).map_err(|e| sdk_err(&e));
    t.check("C15", "DeleteObject on the object (not granted)", "refused", show(&r), denied(&r));
    let src = format!("{}/{}", a.bucket, key_b);
    let r = ctx.sdk(s).copy_object().bucket(&a.bucket).key(&key_a).copy_source(&src).send().await.map(|_| "copied".to_string()).map_err(|e| sdk_err(&e));
    t.check("C16", "CopyObject from another sandbox's object onto the object", "refused", show(&r), denied(&r));
    let still = ctx.store.get_bytes(&ctx.admin, ctx.target(&a.bucket, &key_b)).await.map(|b| sha256_hex(&b) == sha256_hex(&seed)).unwrap_or(false);
    t.check("C17", "other sandbox's object afterwards", "unchanged", format!("unchanged = {still}"), still);

    // Read-only exact-object session.
    match assume(ctx, &a, Some(&a.role_arn), Some(session_policy(&a.bucket, &key_a, false, Access::Read)), 3600).await {
        Ok(ro) => {
            let s = &ro.creds;
            let r = ctx.store.head(s, ctx.target(&a.bucket, &key_a)).await.map(|m| format!("{m:?}")).map_err(|e| common::short_err(&e.to_string()));
            t.check("C18", "read-only: HeadObject", "allowed", show(&r), matches!(&r, Ok(m) if m.contains("Some")));
            let r = ctx.store.get_bytes(s, ctx.target(&a.bucket, &key_a)).await.map(|b| format!("{} bytes", b.len())).map_err(|e| common::short_err(&e.to_string()));
            t.check("C19", "read-only: GetObject", "allowed", show(&r), r.is_ok());
            let r = ctx.store.put_bytes(s, ctx.target(&a.bucket, &key_a), Bytes::from_static(b"w")).await.map(|_| "stored".to_string()).map_err(|e| common::short_err(&e.to_string()));
            t.check("C20", "read-only: PutObject", "refused", show(&r), denied(&r));
            let r = ctx.store.put_file(s, ctx.target(&a.bucket, &key_a), &path).await.map(|n| format!("{n}")).map_err(|e| common::short_err(&e.to_string()));
            t.check("C21", "read-only: put_file multipart", "refused", show(&r), denied(&r));
            let r = mpu_abort(ctx, s, &a.bucket, &key_a).await;
            t.check("C22", "read-only: CreateMultipartUpload", "refused", show(&r), denied(&r));
            let intact = ctx.store.get_bytes(&ctx.admin, ctx.target(&a.bucket, &key_a)).await.map(|b| sha256_hex(&b) == sha).unwrap_or(false);
            t.check("C23", "object after the refused writes", "unchanged", format!("unchanged = {intact}"), intact);
        }
        Err(e) => t.check("C18", "AssumeRole read-only", "credentials", e, false),
    }

    // Prefix session (push/pull artifacts: is_prefix = true).
    match assume(ctx, &a, Some(&a.role_arn), Some(session_policy(&a.bucket, &prefix_a, true, Access::ReadWrite)), 3600).await {
        Ok(ps) => {
            let s = &ps.creds;
            let inside = [format!("{prefix_a}/memory"), format!("{prefix_a}/nested/rootfs")];
            for (i, k) in inside.iter().enumerate() {
                let r = ctx.store.put_bytes(s, ctx.target(&a.bucket, k), Bytes::from(synthetic(1000, 30 + i as u64))).await.map(|_| "stored".to_string()).map_err(|e| common::short_err(&e.to_string()));
                t.check(&format!("C24.{i}"), &format!("prefix RW: PutObject {}", k.rsplit_once(&prefix_a).map(|(_, r)| format!("<prefix>{r}")).unwrap_or_default()), "allowed", show(&r), r.is_ok());
                let r = ctx.store.head(s, ctx.target(&a.bucket, k)).await.map(|m| format!("{m:?}")).map_err(|e| common::short_err(&e.to_string()));
                t.check(&format!("C25.{i}"), "prefix RW: HeadObject on it", "allowed", show(&r), matches!(&r, Ok(m) if m.contains("Some")));
            }
            let r = put_stream_with(ctx, s, &a.bucket, &format!("{prefix_a}/memory.zst"), synthetic(11 * MIB, 33), 5 * MIB, 4).await;
            t.check("C26", "prefix RW: put_stream multipart under the prefix", "allowed", r.clone(), r.starts_with("Ok"));
            for (id, p, want) in [("C27", prefix_a.clone(), true), ("C28", format!("{prefix_a}/"), true), ("C29", format!("{prefix_a}/nested"), true), ("C30", format!("{prefix_a}x"), false), ("C31", format!("suspend_snapshots/07/{sbx_a}/{snap}"), false)] {
                let r = list(ctx, s, &a.bucket, Some(&p)).await;
                let label = p.replace(&prefix_a, "<prefix>");
                t.check(id, &format!("prefix: ListObjectsV2 prefix={label}"), if want { "allowed" } else { "refused" }, show(&r), if want { r.is_ok() } else { denied(&r) });
            }
            for (id, what, k) in [("C32", "the prefix itself as a key", prefix_a.clone()), ("C33", "a sibling sharing the prefix string", format!("{prefix_a}x/escape")), ("C34", "another sandbox's object", key_b.clone())] {
                let r = ctx.store.put_bytes(s, ctx.target(&a.bucket, &k), Bytes::from_static(b"x")).await.map(|_| "stored".to_string()).map_err(|e| common::short_err(&e.to_string()));
                t.check(id, &format!("prefix: PutObject on {what}"), "refused", show(&r), denied(&r));
            }
            let r = ctx.store.put_bytes(s, ctx.target(&a.other_bucket, &format!("{prefix_a}/memory")), Bytes::from_static(b"x")).await.map(|_| "stored".to_string()).map_err(|e| common::short_err(&e.to_string()));
            t.check("C35", "prefix: PutObject under the same prefix in the other bucket", "refused", show(&r), denied(&r));
        }
        Err(e) => t.check("C24", "AssumeRole prefix session", "credentials", e, false),
    }

    // Forged and altered tokens.
    forged(ctx, &mut t, &a, s, &a.bucket, &key_a).await;

    // The vend identity's own keys and role validation.
    let vend = creds(&a.vend_access_key, &a.vend_secret_key, None);
    let r = ctx.store.head(&vend, ctx.target(&a.bucket, &key_b)).await.map(|m| format!("{m:?}")).map_err(|e| common::short_err(&e.to_string()));
    t.add("C40", "vend identity's own keys (no session): HeadObject in the vended bucket", "info", show(&r), None);
    let r = ctx.store.put_bytes(&vend, ctx.target(&a.foreign_bucket, "x"), Bytes::from_static(b"x")).await.map(|_| "stored".to_string()).map_err(|e| common::short_err(&e.to_string()));
    t.check("C41", "vend identity's own keys: PutObject in a bucket outside its policy", "refused", show(&r), denied(&r));
    let r = admin.list_buckets().send().await.map(|o| o.buckets().len()).map_err(|e| sdk_err(&e));
    let _ = r;
    for (id, what, role) in [("C42", "a role ARN that names no role", Some("arn:aws:iam::000000000000:role/no-such-role")), ("C43", "a role ARN in another account", Some("arn:aws:iam::123456789012:role/kind-storage-vend")), ("C44", "a malformed role ARN", Some("not-an-arn")), ("C45", "no RoleArn", None)] {
        let r = assume(ctx, &a, role, Some(session_policy(&a.bucket, &key_a, false, Access::Read)), 3600).await.map(|s| format!("session for {} s", s.expires_in));
        t.add(id, &format!("role validation: AssumeRole with {what}"), "AWS refuses; recorded only", show(&r), None);
    }
    let r = assume(ctx, &a, Some(&a.role_arn), None, 3600).await;
    match r {
        Ok(full) => {
            let r = ctx.store.head(&full.creds, ctx.target(&a.bucket, &key_b)).await.map(|m| format!("{m:?}")).map_err(|e| common::short_err(&e.to_string()));
            t.add("C46", "AssumeRole without Policy: HeadObject on any vended object", "info: parent's policy only", show(&r), None);
        }
        Err(e) => t.add("C46", "AssumeRole without Policy", "info", format!("refused ({e})"), None),
    }

    t.print(&format!("C. Exchange requirements — {}", ctx.label));
    t.all_pass()
}

async fn forged(ctx: &Ctx, t: &mut Table, a: &ExchangeArgs, s: &ObjectCredentials, bucket: &str, key: &str) {
    let token = s.session_token.clone().unwrap_or_default();
    let head = |c: ObjectCredentials| async move { ctx.store.head(&c, ctx.target(bucket, key)).await.map(|m| format!("{m:?}")).map_err(|e| common::short_err(&e.to_string())) };
    let with_token = |tok: String| creds(&s.access_key_id, &s.secret_access_key, Some(tok));

    let mut flipped = token.clone();
    if let Some(last) = flipped.pop() {
        flipped.push(if last == 'A' { 'B' } else { 'A' });
    }
    let r = head(with_token(flipped)).await;
    t.check("C36", "session token with its last character changed", "refused (AWS: 403)", show(&r), refused_any(&r));

    let segments: Vec<&str> = token.split('.').collect();
    if segments.len() == 3 {
        // Widen the embedded session policy, keep the original signature.
        let widened = widen_jwt_policy(segments[1]);
        let altered = format!("{}.{}.{}", segments[0], widened, segments[2]);
        let r = head(with_token(altered)).await;
        t.check("C37", "JWT session token with a widened sessionPolicy, original signature", "refused (AWS: 403)", show(&r), refused_any(&r));
        // Same claims re-signed with a key the caller knows (its own secret).
        let signing_input = format!("{}.{}", segments[0], widened);
        let mut mac = hmac::Hmac::<sha2::Sha512>::new_from_slice(a.vend_secret_key.as_bytes()).expect("hmac key");
        mac.update(signing_input.as_bytes());
        let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        let r = head(with_token(format!("{signing_input}.{sig}"))).await;
        t.check("C38", "JWT re-signed with the vend identity's own secret", "refused (AWS: 403)", show(&r), refused_any(&r));
    } else {
        // An opaque token (versitygw's) embeds no policy to widen or
        // signature to redo; these are the analogous forgeries.
        match assume(ctx, a, Some(&a.role_arn), None, 3600).await {
            Ok(wide) => {
                let r = head(with_token(wide.creds.session_token.clone().unwrap_or_default())).await;
                t.check("C37", "opaque token: a wider session's token (no session Policy) with this session's keys", "refused (AWS: 403)", show(&r), refused_any(&r));
            }
            Err(e) => t.check("C37", "opaque token: a wider session", "credentials", e, false),
        }
        let r = head(creds(&s.access_key_id, &a.vend_secret_key, Some(token.clone()))).await;
        t.check("C38", "opaque token: this session's key and token, signed with the vend identity's own secret", "refused (AWS: 403)", show(&r), refused_any(&r));
    }
    let r = head(creds(&s.access_key_id, &s.secret_access_key, None)).await;
    t.check("C39", "session access key and secret without the token", "refused (AWS: 403)", show(&r), refused_any(&r));
    match assume(ctx, a, Some(&a.role_arn), Some(session_policy(bucket, key, false, Access::Read)), 3600).await {
        Ok(other) => {
            let r = head(creds(&s.access_key_id, &s.secret_access_key, other.creds.session_token.clone())).await;
            t.check("C39b", "session keys with another session's token", "refused (AWS: 403)", show(&r), refused_any(&r));
        }
        Err(e) => t.add("C39b", "second session", "credentials", e, None),
    }
}

fn widen_jwt_policy(payload_b64: &str) -> String {
    let Ok(raw) = URL_SAFE_NO_PAD.decode(payload_b64) else { return payload_b64.to_string() };
    let Ok(mut claims) = serde_json::from_slice::<serde_json::Value>(&raw) else { return payload_b64.to_string() };
    let wide = serde_json::json!({"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:*"],"Resource":["arn:aws:s3:::*"]}]}).to_string();
    claims["sessionPolicy"] = serde_json::Value::String(STANDARD.encode(wide));
    URL_SAFE_NO_PAD.encode(claims.to_string())
}

async fn list(ctx: &Ctx, c: &ObjectCredentials, bucket: &str, prefix: Option<&str>) -> Result<String, String> {
    ctx.sdk(c)
        .list_objects_v2()
        .bucket(bucket)
        .set_prefix(prefix.map(str::to_string))
        .send()
        .await
        .map(|o| format!("{} keys", o.key_count().unwrap_or(0)))
        .map_err(|e| sdk_err(&e))
}

async fn mpu_abort(ctx: &Ctx, c: &ObjectCredentials, bucket: &str, key: &str) -> Result<String, String> {
    let s3 = ctx.sdk(c);
    let up = s3.create_multipart_upload().bucket(bucket).key(key).send().await.map_err(|e| format!("create: {}", sdk_err(&e)))?;
    let id = up.upload_id().unwrap_or_default();
    s3.upload_part().bucket(bucket).key(key).upload_id(id).part_number(1).body(ByteStream::from(synthetic(MIB, 40))).send().await.map_err(|e| format!("part: {}", sdk_err(&e)))?;
    s3.abort_multipart_upload().bucket(bucket).key(key).upload_id(id).send().await.map_err(|e| format!("abort: {}", sdk_err(&e)))?;
    Ok("created, part uploaded, aborted".into())
}

pub async fn run_expiry(ctx: &Ctx, args: ExpiryArgs) -> bool {
    let a = &args.base;
    let mut t = Table::default();
    let _ = ctx.ensure_bucket(&a.bucket).await;
    let key = format!("suspend_snapshots/07/{}/expiry/vm_state", common::unique("e"));
    let _ = ctx.store.put_bytes(&ctx.admin, ctx.target(&a.bucket, &key), Bytes::from_static(b"expiry probe")).await;
    let policy = session_policy(&a.bucket, &key, false, Access::Read);
    let mut chosen = None;
    for &d in &args.durations {
        let r = assume(ctx, a, Some(&a.role_arn), Some(policy.clone()), d).await;
        t.add(&format!("E.{d}"), &format!("AssumeRole DurationSeconds={d}"), "info", match &r { Ok(s) => format!("issued, expires in {} s", s.expires_in), Err(e) => format!("refused ({e})") }, None);
        if chosen.is_none()
            && let Ok(s) = r
        {
            chosen = Some((d, s));
        }
    }
    let Some((d, session)) = chosen else {
        t.check("E1", "a session to wait out", "issued", "no duration accepted", false);
        t.print(&format!("C. Session expiry — {}", ctx.label));
        return false;
    };
    let started = Instant::now();
    let head = || async { ctx.store.head(&session.creds, ctx.target(&a.bucket, &key)).await.map(|m| format!("{m:?}")).map_err(|e| common::short_err(&e.to_string())) };
    let r = head().await;
    t.check("E1", &format!("fresh session (requested {d} s, expires in {} s): HeadObject", session.expires_in), "allowed", show(&r), r.is_ok());
    let before = Duration::from_secs(session.expires_in.saturating_sub(60).max(0) as u64);
    sleep_logged(before, "until 60 s before expiry").await;
    let r = head().await;
    t.check("E2", &format!("~60 s before expiry (+{:.0} s)", started.elapsed().as_secs_f64()), "allowed", show(&r), r.is_ok());
    let after = Duration::from_secs(session.expires_in.max(0) as u64 + args.past_expiry_secs).saturating_sub(started.elapsed());
    sleep_logged(after, "until past expiry").await;
    let r = head().await;
    t.check("E3", &format!("{} s past expiry (+{:.0} s)", args.past_expiry_secs, started.elapsed().as_secs_f64()), "refused", show(&r), denied(&r));
    let r = ctx.store.get_bytes(&session.creds, ctx.target(&a.bucket, &key)).await.map(|b| format!("{} bytes", b.len())).map_err(|e| common::short_err(&e.to_string()));
    t.check("E4", "GetObject past expiry", "refused", show(&r), denied(&r));
    t.print(&format!("C. Session expiry — {}", ctx.label));
    t.all_pass()
}

pub async fn run_session(ctx: &Ctx, args: SessionArgs) -> bool {
    let a = &args.base;
    let _ = ctx.ensure_bucket(&a.bucket).await;
    match assume(ctx, a, Some(&a.role_arn), Some(session_policy(&a.bucket, &args.prefix, true, Access::ReadWrite)), 3600).await {
        Ok(s) => {
            println!(
                "export BAKEOFF_WRITER_ACCESS_KEY='{}' BAKEOFF_WRITER_SECRET_KEY='{}' BAKEOFF_WRITER_SESSION_TOKEN='{}'",
                s.creds.access_key_id,
                s.creds.secret_access_key,
                s.creds.session_token.unwrap_or_default()
            );
            eprintln!("session for {}/{}/* expires in {} s", a.bucket, args.prefix, s.expires_in);
            true
        }
        Err(e) => {
            eprintln!("assume failed: {e}");
            false
        }
    }
}

async fn sleep_logged(total: Duration, what: &str) {
    let end = Instant::now() + total;
    eprintln!("sleeping {:.0} s {what}", total.as_secs_f64());
    while Instant::now() < end {
        let left = end.saturating_duration_since(Instant::now());
        tokio::time::sleep(left.min(Duration::from_secs(60))).await;
        eprintln!("  {:.0} s left", end.saturating_duration_since(Instant::now()).as_secs_f64());
    }
}
