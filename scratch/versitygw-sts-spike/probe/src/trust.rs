//! Role trust and the bridge.
//!
//! `trust`: tokens minted by the bridge's signer with the bridge's claims, and
//! deliberate departures from them, sent straight to versitygw's
//! AssumeRoleWithWebIdentity; roles and durations it must refuse; and forged
//! session credentials on S3.
//!
//! `bridge-auth`: AssumeRole through the bridge, from the authority's own
//! STS client and from the SDK's STS client with signing faults injected
//! after signing (interceptors) or by the signer's clock.

use std::time::{Duration, SystemTime};

use aws_credential_types::Credentials;
use aws_smithy_runtime_api::box_error::BoxError;
use aws_smithy_runtime_api::client::interceptors::Intercept;
use aws_smithy_runtime_api::client::interceptors::context::BeforeTransmitInterceptorContextMut;
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::config_bag::ConfigBag;
use bytes::Bytes;
use clap::Args;
use pplx_space_object_store::{ObjectCredentials, ObjectStore};

use crate::common::{self, Ctx, Table, creds, sdk_err};
use crate::exchange::{Access, ExchangeArgs, Session, assume_web_identity, authority_assume, refused_any, session_policy, show, sts_config};

#[derive(Args)]
pub struct TrustArgs {
    #[command(flatten)]
    pub base: ExchangeArgs,
    /// A signing key versitygw does not trust.
    #[arg(long, env = "SPIKE_OTHER_KEY")]
    pub other_key: String,
    /// A role whose trust policy names another subject.
    #[arg(long, default_value = "arn:aws:iam::000000000000:role/kind-storage-other")]
    pub other_role_arn: String,
}

#[derive(Args)]
pub struct EarlyArgs {
    #[command(flatten)]
    pub base: ExchangeArgs,
    #[arg(long, default_value_t = 20)]
    pub repeats: usize,
}

#[derive(Args)]
pub struct BridgeArgs {
    #[command(flatten)]
    pub base: ExchangeArgs,
    #[arg(long, default_value = "arn:aws:iam::000000000000:role/kind-storage-other")]
    pub other_role_arn: String,
}

fn issued(r: &Result<Session, String>) -> Result<String, String> {
    r.as_ref().map(|s| format!("session for {} s", s.expires_in)).map_err(Clone::clone)
}

fn refused(r: &Result<Session, String>) -> bool {
    matches!(r, Err(e) if e.contains("HTTP 4") || e.contains("HTTP 5") || e.contains("provider error"))
}

async fn seed(ctx: &Ctx, bucket: &str) -> String {
    let key = format!("suspend_snapshots/07/{}/trust/vm_state", common::unique("t"));
    let _ = ctx.ensure_bucket(bucket).await;
    let _ = ctx.store.put_bytes(&ctx.admin, ctx.target(bucket, &key), Bytes::from_static(b"trust probe")).await;
    key
}

async fn head(ctx: &Ctx, c: &ObjectCredentials, bucket: &str, key: &str) -> Result<String, String> {
    ctx.store.head(c, ctx.target(bucket, key)).await.map(|m| format!("{m:?}")).map_err(|e| common::short_err(&e.to_string()))
}

pub async fn run(ctx: &Ctx, args: TrustArgs) -> bool {
    let a = &args.base;
    let mut t = Table::default();
    let key = seed(ctx, &a.bucket).await;
    let policy = || Some(session_policy(&a.bucket, &key, false, Access::Read));
    let role = Some(a.role_arn.as_str());
    let other_key = format!("-signing-key {}", args.other_key);
    let other_key: Vec<&str> = other_key.split_whitespace().collect();

    // T0: the bridge's own claims are trusted, and the session works.
    let r = match a.mint_token(&[]) {
        Ok(tok) => assume_web_identity(ctx, a, role, policy(), 3600, &tok).await,
        Err(e) => Err(e),
    };
    t.check("T0", "control: the bridge's sub, aud and issuer, its signing key", "issued", show(&issued(&r)), r.is_ok());
    if let Ok(s) = &r {
        let h = head(ctx, &s.creds, &a.bucket, &key).await;
        t.check("T0s", "control session: HeadObject on its object", "allowed", show(&h), h.is_ok());
    }

    let cases: Vec<(&str, &str, Vec<&str>)> = vec![
        ("T1", "a subject the trust policy does not name", vec!["-sub", "kind-storage-impostor"]),
        ("T2", "an audience not in the provider's ClientIDList", vec!["-aud", "unknown-audience"]),
        ("T3", "an audience in the ClientIDList that the trust policy does not name", vec!["-aud", "kind-other-client"]),
        ("T4", "an issuer with no registered OIDC provider", vec!["-iss", "https://untrusted-issuer.kind.invalid"]),
        ("T5", "the trusted issuer's name, signed by an untrusted key under the trusted kid", vec![]),
        ("T6", "the trusted issuer's name, signed by an untrusted key with its own kid", vec![]),
        ("T7", "an unsigned token (alg none)", vec!["-alg", "none"]),
        ("T8", "a token that expired 10 minutes ago", vec!["-exp-in", "-10m"]),
    ];
    let trusted_kid = kid_of(&a.mint_token(&[]).unwrap_or_default());
    for (id, what, mut overrides) in cases {
        let kid_arg;
        match id {
            "T5" => {
                overrides.extend(other_key.iter().copied());
                kid_arg = trusted_kid.clone();
                overrides.extend(["-kid", kid_arg.as_str()]);
            }
            "T6" => overrides.extend(other_key.iter().copied()),
            _ => {}
        }
        let r = match a.mint_token(&overrides) {
            Ok(tok) => assume_web_identity(ctx, a, role, policy(), 3600, &tok).await,
            Err(e) => Err(e),
        };
        t.check(id, &format!("token: {what}"), "refused", show(&issued(&r)), refused(&r));
    }
    let r = match a.mint_token(&["-exp-in", "-2m"]) {
        Ok(tok) => assume_web_identity(ctx, a, role, policy(), 3600, &tok).await,
        Err(e) => Err(e),
    };
    t.add("T8i", "token: expired 2 minutes ago (versitygw allows 5 minutes of skew, as AWS does)", "info", show(&issued(&r)), None);

    let tok = a.mint_token(&[]).unwrap_or_default();
    for (id, what, role, duration, info) in [
        ("T9", "a role that does not exist", "arn:aws:iam::000000000000:role/no-such-role", 3600, false),
        ("T10", "a role whose trust policy names another subject", args.other_role_arn.as_str(), 3600, false),
        ("T11", "DurationSeconds 3601, above the role's MaxSessionDuration 3600", a.role_arn.as_str(), 3601, false),
        ("T12", "DurationSeconds 43201, above STS's maximum", a.role_arn.as_str(), 43201, false),
        ("T13", "the vend role's name in another account", "arn:aws:iam::123456789012:role/kind-storage-vend", 3600, false),
        ("T14", "the vend role's name under another path", "arn:aws:iam::000000000000:role/other/kind-storage-vend", 3600, false),
        ("T15i", "DurationSeconds 899, below STS's minimum", a.role_arn.as_str(), 899, true),
    ] {
        let r = assume_web_identity(ctx, a, Some(role), policy(), duration, &tok).await;
        if info {
            t.add(id, &format!("role: {what}"), "info (AWS refuses)", show(&issued(&r)), None);
        } else {
            t.check(id, &format!("role: {what}"), "refused", show(&issued(&r)), refused(&r));
        }
    }

    // Forged session credentials beyond criterion C's C36–C39b.
    if let Ok(tok) = a.mint_token(&[]) {
        let one = assume_web_identity(ctx, a, role, policy(), 3600, &tok).await;
        let two = assume_web_identity(ctx, a, role, policy(), 3600, &tok).await;
        if let (Ok(one), Ok(two)) = (one, two) {
            let c = &one.creds;
            let token = c.session_token.clone().unwrap_or_default();
            let random: String = common::synthetic(token.len(), 77).iter().map(|b| (b'A' + b % 26) as char).collect();
            let forged: [(&str, &str, ObjectCredentials); 4] = [
                ("F1", "a made-up token of the same length with this session's keys", creds(&c.access_key_id, &c.secret_access_key, Some(random))),
                ("F2", "this session's secret and token under another session's access key", creds(&two.creds.access_key_id, &c.secret_access_key, Some(token.clone()))),
                ("F3", "this session's access key with one character changed", creds(&flip_last(&c.access_key_id), &c.secret_access_key, Some(token.clone()))),
                ("F4", "this session's secret with one character changed", creds(&c.access_key_id, &flip_last(&c.secret_access_key), Some(token.clone()))),
            ];
            for (id, what, fc) in forged {
                let r = head(ctx, &fc, &a.bucket, &key).await;
                t.check(id, &format!("forged: {what}"), "refused (AWS: 403)", show(&r), refused_any(&r));
            }
            let r = head(ctx, c, &a.bucket, &key).await;
            t.check("F5", "the genuine session afterwards", "allowed", show(&r), r.is_ok());
        }
    }

    // How a refusal of a large single PUT reaches SPACE's client. The store
    // answers before reading the body and closes the connection.
    let (path, _) = common::write_synthetic("r-20m.bin", 20 * common::MIB, 90).await;
    let ro = match a.mint_token(&[]) {
        Ok(tok) => assume_web_identity(ctx, a, role, policy(), 3600, &tok).await.ok(),
        Err(_) => None,
    };
    let wrong = creds(&ctx.admin.access_key_id, "not-the-admin-secret", None);
    let mut early: Vec<(&str, &str, ObjectCredentials, &str)> = vec![
        ("R1", "admin keys, a bucket that does not exist", ctx.admin.clone(), "spike-no-such-bucket"),
        ("R2", "admin key, wrong secret", wrong, a.bucket.as_str()),
    ];
    if let Some(ro) = &ro {
        early.push(("R3", "read-only session, its own object", ro.creds.clone(), a.bucket.as_str()));
    }
    for (id, what, c, bucket) in early {
        ctx.attempts.take();
        let r = ctx.store.put_file(&c, ctx.target(bucket, &key), &path).await.map(|n| format!("{n} bytes")).map_err(|e| common::short_err(&e.to_string()));
        let attempts: Vec<String> = ctx.attempts.take().iter().map(|x| x.render()).collect();
        t.add(id, &format!("20 MiB put_file refused early: {what}"), "info", format!("{}; attempts: {}", show(&r), attempts.join("; ")), None);
    }

    t.print(&format!("Trust and forged credentials — {}", ctx.label));
    t.all_pass()
}

/// How often a refused 20 MiB single PUT reaches SPACE's client as its HTTP
/// status rather than as a connection reset on the body write, per kind of
/// refusal. Static keys are the control: Kind's store refuses them today.
pub async fn run_early(ctx: &Ctx, args: EarlyArgs) -> bool {
    let a = &args.base;
    let mut t = Table::default();
    let key = seed(ctx, &a.bucket).await;
    let (path, _) = common::write_synthetic("r-20m.bin", 20 * common::MIB, 90).await;
    let ro = match a.mint_token(&[]) {
        Ok(tok) => assume_web_identity(ctx, a, Some(&a.role_arn), Some(session_policy(&a.bucket, &key, false, Access::Read)), 3600, &tok).await.ok(),
        Err(_) => None,
    };
    let mut cases: Vec<(&str, &str, ObjectCredentials, String)> = vec![
        ("R1", "static admin keys, a bucket that does not exist", ctx.admin.clone(), "spike-no-such-bucket".into()),
        ("R2", "static admin key, wrong secret", creds(&ctx.admin.access_key_id, "not-the-admin-secret", None), a.bucket.clone()),
    ];
    if let Some(ro) = ro {
        cases.push(("R3", "read-only session (policy refusal)", ro.creds, a.bucket.clone()));
    }
    for (id, what, c, bucket) in cases {
        let (mut with_status, mut reset) = (0, 0);
        for _ in 0..args.repeats {
            let r = ctx.store.put_file(&c, ctx.target(&bucket, &key), &path).await.map_err(|e| common::short_err(&e.to_string()));
            if std::env::var_os("SPIKE_EARLY_DEBUG").is_some() {
                eprintln!("{id}: {r:?}");
            }
            match &r {
                Err(e) if e.contains("HTTP 4") => with_status += 1,
                Err(e) if e.contains("reset") => reset += 1,
                _ => {}
            }
        }
        t.add(id, &format!("20 MiB put_file refused early: {what}"), "info", format!(
            "after the client's retries: HTTP status {with_status}/{n}, connection reset {reset}/{n}", n = args.repeats), None);
    }
    t.print(&format!("Early refusals of a large single PUT — {}", ctx.label));
    true
}

fn kid_of(token: &str) -> String {
    use base64::Engine as _;
    let header = token.split('.').next().unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(header)
        .ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        .and_then(|v| v["kid"].as_str().map(str::to_string))
        .unwrap_or_default()
}

fn flip_last(s: &str) -> String {
    let mut out = s.to_string();
    if let Some(last) = out.pop() {
        out.push(if last == 'A' { 'B' } else { 'A' });
    }
    out
}

/// Rewrites the signed form body after signing, keeping its length.
#[derive(Debug)]
struct TamperBody;

impl Intercept for TamperBody {
    fn name(&self) -> &'static str {
        "TamperBody"
    }
    fn modify_before_transmit(&self, ctx: &mut BeforeTransmitInterceptorContextMut<'_>, _: &RuntimeComponents, _: &mut ConfigBag) -> Result<(), BoxError> {
        let req = ctx.request_mut();
        let body = String::from_utf8(req.body().bytes().unwrap_or_default().to_vec())?;
        *req.body_mut() = SdkBody::from(body.replace("GetObject", "PutObject"));
        Ok(())
    }
}

/// Drops the Authorization header after signing: an unsigned request.
#[derive(Debug)]
struct StripAuth;

impl Intercept for StripAuth {
    fn name(&self) -> &'static str {
        "StripAuth"
    }
    fn modify_before_transmit(&self, ctx: &mut BeforeTransmitInterceptorContextMut<'_>, _: &RuntimeComponents, _: &mut ConfigBag) -> Result<(), BoxError> {
        ctx.request_mut().headers_mut().remove("authorization");
        Ok(())
    }
}

enum Fault {
    None,
    Tamper,
    Strip,
}

async fn bridge_call(
    ctx: &Ctx,
    a: &ExchangeArgs,
    keys: (&str, &str, Option<String>),
    signed_at: Option<SystemTime>,
    role: &str,
    policy: Option<String>,
    duration: i32,
    fault: Fault,
    external_id: Option<&str>,
) -> Result<Session, String> {
    let mut conf = sts_config(ctx, &a.bridge_endpoint).credentials_provider(Credentials::new(keys.0, keys.1, keys.2, None, "spike"));
    if let Some(at) = signed_at {
        // No retries: the SDK reads the clock skew off a refusal's Date
        // header and re-signs, which would hide the bridge's answer.
        conf = conf.time_source(aws_smithy_async::time::StaticTimeSource::new(at)).retry_config(aws_sdk_sts::config::retry::RetryConfig::disabled());
    }
    let client = aws_sdk_sts::Client::from_conf(conf.build());
    let req = client
        .assume_role()
        .role_arn(role)
        .role_session_name("space-storage-exchange")
        .duration_seconds(duration)
        .set_policy(policy)
        .set_external_id(external_id.map(str::to_string));
    let out = match fault {
        Fault::None => req.send().await,
        Fault::Tamper => req.customize().interceptor(TamperBody).send().await,
        Fault::Strip => req.customize().interceptor(StripAuth).send().await,
    }
    .map_err(|e| sdk_err(&e))?;
    let c = out.credentials().ok_or("no credentials in response")?;
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64;
    Ok(Session {
        creds: creds(c.access_key_id(), c.secret_access_key(), Some(c.session_token().to_string())),
        expires_in: c.expiration().secs() - now,
    })
}

pub async fn run_bridge(ctx: &Ctx, args: BridgeArgs) -> bool {
    let a = &args.base;
    let mut t = Table::default();
    let key = seed(ctx, &a.bucket).await;
    let other = seed(ctx, &a.bucket).await;
    let policy = || Some(session_policy(&a.bucket, &key, false, Access::Read));
    let vend = (a.vend_access_key.as_str(), a.vend_secret_key.as_str(), None);

    // The authority's own client, end to end.
    let r = authority_assume(&a.role_arn, policy(), 3600).await;
    t.check("B0", "authority's AssumeRoleProvider (load_defaults, AWS_ENDPOINT_URL_STS = bridge)", "credentials, expiry ~3600 s", show(&issued(&r)), matches!(&r, Ok(s) if (3500..=3700).contains(&s.expires_in)));
    if let Ok(s) = &r {
        let h = head(ctx, &s.creds, &a.bucket, &key).await;
        t.check("B0a", "its session: HeadObject on the policy's object", "allowed", show(&h), h.is_ok());
        let h = head(ctx, &s.creds, &a.bucket, &other).await;
        t.check("B0b", "its session: HeadObject on another object", "refused", show(&h), refused_any(&h));
        let w = ctx.store.put_bytes(&s.creds, ctx.target(&a.bucket, &key), Bytes::from_static(b"w")).await.map(|_| "stored".to_string()).map_err(|e| common::short_err(&e.to_string()));
        t.check("B0c", "its session: PutObject under a read policy", "refused", show(&w), refused_any(&w));
    }
    let r = bridge_call(ctx, a, vend.clone(), None, &a.role_arn, policy(), 3600, Fault::None, None).await;
    t.check("B1", "SDK assume_role (Policy, DurationSeconds 3600), vend keys", "credentials", show(&issued(&r)), r.is_ok());
    let session = r.ok();

    let cases: Vec<(&str, &str, Result<Session, String>)> = vec![
        ("B2", "unsigned (Authorization removed after signing)", bridge_call(ctx, a, vend.clone(), None, &a.role_arn, policy(), 3600, Fault::Strip, None).await),
        ("B3", "signed with a wrong secret", bridge_call(ctx, a, (vend.0, "not-the-vend-secret", None), None, &a.role_arn, policy(), 3600, Fault::None, None).await),
        ("B4", "an access key the bridge does not know", bridge_call(ctx, a, ("AKIDUNKNOWNSPIKE", vend.1, None), None, &a.role_arn, policy(), 3600, Fault::None, None).await),
        ("B5", "Policy changed after signing (GetObject to PutObject, same length)", bridge_call(ctx, a, vend.clone(), None, &a.role_arn, policy(), 3600, Fault::Tamper, None).await),
        ("B6", "signed 20 minutes ago", bridge_call(ctx, a, vend.clone(), Some(SystemTime::now() - Duration::from_secs(1200)), &a.role_arn, policy(), 3600, Fault::None, None).await),
        ("B7", "vend keys with a session token", bridge_call(ctx, a, (vend.0, vend.1, Some("bogus-session-token".into())), None, &a.role_arn, policy(), 3600, Fault::None, None).await),
    ];
    for (id, what, r) in cases {
        t.check(id, &format!("SigV4: {what}"), "refused (403)", show(&issued(&r)), matches!(&r, Err(e) if e.contains("HTTP 403")));
    }
    let conf = sts_config(ctx, &a.bridge_endpoint)
        .credentials_provider(Credentials::new(vend.0, vend.1, None, None, "spike"))
        .time_source(aws_smithy_async::time::StaticTimeSource::new(SystemTime::now() - Duration::from_secs(1200)));
    let r = aws_sdk_sts::Client::from_conf(conf.build()).assume_role().role_arn(&a.role_arn).role_session_name("space-storage-exchange").set_policy(policy()).send().await;
    t.add("B6i", "signed 20 minutes ago, SDK retries on (its default)", "info", match r {
        Ok(_) => "issued: the SDK corrected its clock from the refusal and re-signed".to_string(),
        Err(e) => format!("refused ({})", sdk_err(&e)),
    }, None);
    if let Some(s) = &session {
        let c = &s.creds;
        let r = bridge_call(ctx, a, (&c.access_key_id, &c.secret_access_key, c.session_token.clone()), None, &a.role_arn, None, 3600, Fault::None, None).await;
        t.check("B8", "SigV4: a vended session's own credentials (role chaining)", "refused (403)", show(&issued(&r)), matches!(&r, Err(e) if e.contains("HTTP 403")));
    }
    let r = bridge_call(ctx, a, vend.clone(), None, &a.role_arn, policy(), 3600, Fault::None, Some("external")).await;
    t.check("B9", "an AssumeRole parameter the bridge does not forward (ExternalId)", "refused", show(&issued(&r)), refused(&r));
    for (id, what, role, duration) in [
        ("B10", "a role that does not exist", "arn:aws:iam::000000000000:role/no-such-role", 3600),
        ("B11", "a role whose trust policy names another subject", args.other_role_arn.as_str(), 3600),
        ("B12", "DurationSeconds 3601, above the role's maximum", a.role_arn.as_str(), 3601),
        ("B13", "a malformed role ARN", "not-an-arn", 3600),
    ] {
        let r = bridge_call(ctx, a, vend.clone(), None, role, policy(), duration, Fault::None, None).await;
        t.check(id, &format!("trust through the bridge: {what}"), "refused", show(&issued(&r)), refused(&r));
    }
    let r = authority_assume("arn:aws:iam::000000000000:role/no-such-role", policy(), 3600).await;
    t.check("B14", "authority's client: a role that does not exist", "refused (CredentialsError)", show(&issued(&r)), r.is_err());

    t.print(&format!("Bridge — {}", ctx.label));
    t.all_pass()
}
