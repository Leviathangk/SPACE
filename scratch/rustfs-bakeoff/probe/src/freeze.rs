//! Criterion B2: freeze the store mid-upload and check that no short or wrong
//! object is ever stored or visible as complete.
//!
//! The upload goes through the object-store client: `put_file` (one
//! PutObject, the client's file path) or `put_stream` in 64 MiB parts at
//! concurrency 16 (volume-manager's streaming snapshot push). The store is
//! frozen with `docker pause`. Rate-limit the store's ingress first (run.sh
//! does, with tc) so the upload is still in flight when the freeze starts.

use std::process::Command;
use std::time::{Duration, Instant};

use clap::Args;
use pplx_space_object_store::ObjectStore;

use crate::behaviours::put_stream;
use crate::common::{self, Ctx, MIB, Table, sha256_hex, synthetic};

#[derive(Args)]
pub struct FreezeArgs {
    #[arg(long, default_value = "bakeoff-b2")]
    bucket: String,
    /// Docker container to pause.
    #[arg(long)]
    container: String,
    #[arg(long, value_delimiter = ',', default_value = "7,17,25")]
    freeze_secs: Vec<u64>,
    #[arg(long, default_value_t = 200)]
    size_mib: usize,
    /// Seconds after the upload starts at which the freeze begins.
    #[arg(long, default_value_t = 2.0)]
    freeze_after: f64,
    #[arg(long, value_delimiter = ',', default_value = "single,multipart")]
    modes: Vec<String>,
    /// How long after the upload returns to keep re-checking the stored
    /// object, for a late commit of an abandoned attempt.
    #[arg(long, default_value_t = 60)]
    settle_secs: u64,
}

pub async fn run(ctx: &'static Ctx, args: FreezeArgs) -> bool {
    let mut t = Table::default();
    if let Err(e) = ctx.ensure_bucket(&args.bucket).await {
        t.check("B2.0", "create bucket", "200", e, false);
        t.print(&format!("B2. Freeze — {}", ctx.label));
        return false;
    }
    let mut details = Vec::new();
    let mut n = 0;
    for mode in &args.modes {
        for &freeze in &args.freeze_secs {
            n += 1;
            let key = format!("{}/{mode}-{freeze}s", common::unique("b2"));
            let data = synthetic(args.size_mib * MIB, 100 + n);
            let want_sha = sha256_hex(&data);
            let want_len = data.len() as u64;
            let since = log_since();
            ctx.attempts.take();
            let started = Instant::now();
            let upload = {
                let ctx_ptr = ctx;
                let bucket = args.bucket.clone();
                let key = key.clone();
                let mode = mode.clone();
                let path = common::scratch_dir().join(format!("b2-{n}.bin"));
                std::fs::write(&path, &data).expect("write source");
                drop(data);
                tokio::spawn(async move {
                    let r = if mode == "single" {
                        ctx_ptr.store.put_file(&ctx_ptr.admin, ctx_ptr.target(&bucket, &key), &path).await.map(|n| format!("Ok({n})")).unwrap_or_else(|e| format!("Err({e})"))
                    } else {
                        let data = std::fs::read(&path).expect("read source");
                        put_stream(ctx_ptr, &bucket, &key, data, 64 * MIB, 16).await
                    };
                    let _ = std::fs::remove_file(&path);
                    r
                })
            };
            tokio::time::sleep(Duration::from_secs_f64(args.freeze_after)).await;
            let paused_at = started.elapsed();
            let pause = docker(&["pause", &args.container]);
            // While frozen, nothing can be read from the store; just wait.
            tokio::time::sleep(Duration::from_secs(freeze)).await;
            let unpause = docker(&["unpause", &args.container]);
            let resumed_at = started.elapsed();
            let result = upload.await.unwrap_or_else(|e| format!("task panicked: {e}"));
            let returned_at = started.elapsed();
            let attempts = ctx.attempts.take();

            // Re-check the key until the settle window ends: an abandoned
            // attempt that commits late would show up as a size or hash change.
            let mut checks = Vec::new();
            let settle_end = Instant::now() + Duration::from_secs(args.settle_secs);
            let mut bad_seen = false;
            loop {
                let (state, bad) = stored_state(ctx, &args.bucket, &key, want_len, &want_sha).await;
                bad_seen |= bad;
                checks.push(format!("+{:.0}s {state}", started.elapsed().as_secs_f64()));
                if Instant::now() >= settle_end {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(15)).await;
            }
            let store_log = store_attempts(&args.container, &since, &key);
            let id = format!("B2.{n}");
            let client = format!(
                "{} after {}; {} failed attempt(s){}",
                result,
                common::secs(returned_at),
                attempts.len(),
                if attempts.is_empty() { String::new() } else { format!(": {}", attempts.iter().map(|a| a.render()).collect::<Vec<_>>().join("; ")) }
            );
            let last = checks.last().cloned().unwrap_or_default();
            let ok_result = result.starts_with("Ok");
            let pass = !bad_seen && (!ok_result || last.contains("identical"));
            t.check(
                &id,
                &format!("{mode} {} MiB, frozen {freeze} s at +{:.1} s", args.size_mib, paused_at.as_secs_f64()),
                "never a short or wrong object",
                format!("client: {client} | stored: {last}"),
                pass,
            );
            details.push(format!(
                "#### {id}: {mode}, freeze {freeze} s\n\n- pause: {pause} at +{:.1} s; unpause: {unpause} at +{:.1} s\n- client: {client}\n- store log for the key ({} requests):\n{}\n- stored object over time: {}\n",
                paused_at.as_secs_f64(),
                resumed_at.as_secs_f64(),
                store_log.len(),
                store_log.iter().map(|l| format!("  - {l}")).collect::<Vec<_>>().join("\n"),
                checks.join("; "),
            ));
        }
    }
    t.print(&format!("B2. Store frozen mid-upload — {}", ctx.label));
    println!("{}", details.join("\n"));
    t.all_pass()
}

async fn stored_state(ctx: &Ctx, bucket: &str, key: &str, want_len: u64, want_sha: &str) -> (String, bool) {
    match ctx.store.head(&ctx.admin, ctx.target(bucket, key)).await {
        Ok(None) => ("absent".into(), false),
        Ok(Some(meta)) => {
            let dst = common::scratch_dir().join("b2-check.down");
            let got = ctx.store.get_file(&ctx.admin, ctx.target(bucket, key), &dst).await;
            let (len, sha) = common::sha256_file(&dst).await.unwrap_or((0, String::new()));
            let _ = std::fs::remove_file(&dst);
            if got.is_ok() && len == want_len && sha == want_sha && meta.size == want_len {
                (format!("identical ({len} bytes, sha256 {}…)", &sha[..12]), false)
            } else {
                (format!("WRONG: head {} bytes, read {len} bytes, sha256 match = {}, read {got:?}", meta.size, sha == want_sha), true)
            }
        }
        Err(e) => (format!("head error {e}"), false),
    }
}

fn docker(args: &[&str]) -> String {
    match Command::new("docker").args(args).output() {
        Ok(o) if o.status.success() => "ok".into(),
        Ok(o) => format!("failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => format!("failed: {e}"),
    }
}

fn log_since() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(1);
    now.to_string()
}

/// Every request the store logged for `key` since `since`, condensed.
fn store_attempts(container: &str, since: &str, key: &str) -> Vec<String> {
    let Ok(out) = Command::new("docker").args(["logs", "--since", since, container]).output() else {
        return vec!["docker logs failed".into()];
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text.lines().filter(|l| l.contains(key)).filter_map(condense).collect()
}

fn condense(line: &str) -> Option<String> {
    if line.starts_with('{') {
        // RustFS: one JSON line per completed request.
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        if v.get("event").and_then(|e| e.as_str()) != Some("http_request_completed") {
            let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("");
            let lvl = v.get("level").and_then(|m| m.as_str()).unwrap_or("");
            return Some(format!("{} {lvl} {msg}", ts(&v)));
        }
        let uri = v.get("uri").and_then(|u| u.as_str()).unwrap_or("");
        let query = uri.split_once('?').map(|(_, q)| q).unwrap_or("");
        return Some(format!(
            "{} {} {} -> {} in {} ms",
            ts(&v),
            v.get("method").and_then(|m| m.as_str()).unwrap_or("?"),
            short_query(query),
            v.get("status_code").map(|s| s.to_string()).unwrap_or_default(),
            v.get("duration_ms").map(|s| s.to_string()).unwrap_or_default(),
        ));
    }
    // versitygw access log: ... s3_<Op> <key> <path> <status> <error> <bytes...> ...
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let i = tokens.iter().position(|t| t.starts_with("s3_"))?;
    let time = tokens.iter().find(|t| t.starts_with('[')).map(|t| t.trim_start_matches('[')).unwrap_or("");
    Some(format!(
        "{} {} -> {} {} (counters {})",
        time.rsplit_once(':').map(|(a, _)| a).unwrap_or(time).split_once(':').map(|(_, b)| b).unwrap_or(""),
        tokens[i],
        tokens.get(i + 3).unwrap_or(&"?"),
        tokens.get(i + 4).unwrap_or(&"?"),
        tokens.get(i + 5..i + 9).map(|s| s.join(" ")).unwrap_or_default(),
    ))
}

fn ts(v: &serde_json::Value) -> String {
    v.get("timestamp").and_then(|t| t.as_str()).map(|t| t.get(11..23).unwrap_or(t).to_string()).unwrap_or_default()
}

fn short_query(q: &str) -> String {
    let mut parts: Vec<&str> = q.split('&').filter(|p| p.starts_with("partNumber") || *p == "uploads" || *p == "uploads=").collect();
    if q.contains("uploadId") && !q.contains("partNumber") {
        parts.push("uploadId");
    }
    if parts.is_empty() { "object".into() } else { parts.join("&") }
}
