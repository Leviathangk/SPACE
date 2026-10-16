//! Criterion D: a burst of concurrent uploads, and the first write to a new
//! bucket.
//!
//! Each run uploads `count` objects of `size_mib` MiB at once, twice:
//!
//! 1. through the object-store client's `put_file` (one PutObject each, with
//!    Content-MD5 and an UNSIGNED-PAYLOAD body): phase time and slowest PUT;
//! 2. through the SDK in the same request shape, with a body that records
//!    when its last byte was handed to the connection: the acknowledgement
//!    gap, from the last body byte sent to the response.
//!
//! With `--rx-counter` (the host-side veth's `tx_bytes`, which counts bytes
//! sent into the store's container) a sampler records ingress every 100 ms
//! and reports the longest window with no progress while uploads were open.

use std::convert::Infallible;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use aws_smithy_types::body::SdkBody;
use bytes::Bytes;
use clap::Args;
use pplx_space_object_store::ObjectStore;

use crate::behaviours::base64_md5;
use crate::common::{self, Ctx, MIB, Table, sdk_err, synthetic};

#[derive(Args)]
pub struct BurstArgs {
    #[arg(long, default_value = "bakeoff-d")]
    bucket: String,
    #[arg(long, default_value_t = 36)]
    count: usize,
    #[arg(long, default_value_t = 128)]
    size_mib: usize,
    #[arg(long, default_value_t = 3)]
    runs: usize,
    /// File whose integer content counts bytes sent to the store.
    #[arg(long)]
    rx_counter: Option<PathBuf>,
    /// Zero-progress window that counts as a stall.
    #[arg(long, default_value_t = 5.0)]
    stall_secs: f64,
}

struct Sampler {
    samples: Arc<Mutex<Vec<(Instant, u64)>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Sampler {
    fn start(path: Option<PathBuf>) -> Self {
        let samples = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = path.map(|p| {
            let samples = samples.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Ok(s) = std::fs::read_to_string(&p)
                        && let Ok(v) = s.trim().parse::<u64>()
                    {
                        samples.lock().unwrap().push((Instant::now(), v));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
        });
        Self { samples, stop, handle }
    }

    /// Windows in [from, to] during which ingress grew by under 64 KiB while
    /// `open(t)` held throughout: the longest, and how many reached `limit`.
    fn stalls(&self, from: Instant, to: Instant, limit: Duration, open: &dyn Fn(Instant) -> bool) -> Option<(Duration, usize)> {
        let s = self.samples.lock().unwrap();
        let w: Vec<_> = s.iter().filter(|(t, _)| *t >= from && *t <= to).collect();
        if w.len() < 2 {
            return None;
        }
        let mut longest = Duration::ZERO;
        let mut count = 0;
        let mut start: Option<usize> = None;
        let close = |a: usize, b: usize, longest: &mut Duration, count: &mut usize| {
            let span = w[b].0 - w[a].0;
            if span >= limit {
                *count += 1;
            }
            *longest = (*longest).max(span);
        };
        for i in 0..w.len() {
            let t = w[i].0;
            match start {
                Some(a) if open(t) && w[i].1 - w[a].1 < 64 * 1024 => {}
                Some(a) => {
                    close(a, i - 1, &mut longest, &mut count);
                    start = open(t).then_some(i);
                }
                None => start = open(t).then_some(i),
            }
        }
        if let Some(a) = start {
            close(a, w.len() - 1, &mut longest, &mut count);
        }
        Some((longest, count))
    }

    fn stop(mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct RunResult {
    phase: Duration,
    slowest: Duration,
    failures: usize,
    attempts: usize,
    sdk_phase: Duration,
    max_ack: Duration,
    stalls: Option<(Duration, usize)>,
}

pub async fn run(ctx: &'static Ctx, args: BurstArgs) -> bool {
    let mut t = Table::default();
    if let Err(e) = ctx.ensure_bucket(&args.bucket).await {
        t.check("D0", "create bucket", "200", e, false);
        t.print(&format!("D. Upload burst — {}", ctx.label));
        return false;
    }
    let (path, _) = common::write_synthetic("d-burst.bin", args.size_mib * MIB, 50).await;
    let data = Bytes::from(std::fs::read(&path).expect("read burst source"));
    let md5 = base64_md5(&data);
    let sampler = Sampler::start(args.rx_counter.clone());
    let limit = Duration::from_secs_f64(args.stall_secs);
    let mut results = Vec::new();
    for run in 1..=args.runs {
        let prefix = common::unique(&format!("d{run}"));
        ctx.attempts.take();
        // 1. Object-store client put_file.
        let started = Instant::now();
        let mut tasks = Vec::new();
        for i in 0..args.count {
            let key = format!("{prefix}/client/{i:02}");
            let bucket = args.bucket.clone();
            let path = path.clone();
            tasks.push(tokio::spawn(async move {
                let t0 = Instant::now();
                let r = ctx.store.put_file(&ctx.admin, ctx.target(&bucket, &key), &path).await;
                (t0.elapsed(), Instant::now(), r.err().map(|e| common::short_err(&e.to_string())))
            }));
        }
        let mut slowest = Duration::ZERO;
        let mut failures = 0;
        let mut first_error = None;
        let mut last_end = started;
        for task in tasks {
            let (d, end, err) = task.await.expect("upload task");
            slowest = slowest.max(d);
            last_end = last_end.max(end);
            if let Some(e) = err {
                failures += 1;
                first_error.get_or_insert(e);
            }
        }
        let phase = last_end - started;
        let failed = ctx.attempts.take();
        let attempts = failed.len();
        let stalled_attempts = failed.iter().filter(|a| a.kind == "stalled_stream").count();
        // put_file's body progress is not visible here, so this window also
        // counts time spent waiting for acknowledgements; the client's own
        // stall detection (5 s without body progress) is `stalled_attempts`.
        let waits_client = sampler.stalls(started, last_end, limit, &|_| true);
        let client_failures = failures;
        // Free the store's disk before the second phase.
        delete_prefix(ctx, &args.bucket, &format!("{prefix}/client/")).await;

        // 2. SDK, same request shape, timed body.
        let s3 = ctx.sdk(&ctx.admin);
        let sdk_started = Instant::now();
        let mut tasks = Vec::new();
        let bodies: Arc<Mutex<Vec<Arc<Mutex<BodyTimes>>>>> = Arc::default();
        for i in 0..args.count {
            let key = format!("{prefix}/sdk/{i:02}");
            let times = Arc::new(Mutex::new(BodyTimes::default()));
            bodies.lock().unwrap().push(times.clone());
            let bucket = args.bucket.clone();
            let s3 = s3.clone();
            let md5 = md5.clone();
            let data = data.clone();
            tasks.push(tokio::spawn(async move {
                let body = timed_body(data, times.clone());
                let r = s3.put_object().bucket(bucket).key(key).content_md5(md5).body(body).customize().disable_payload_signing().send().await;
                let done = Instant::now();
                let last = times.lock().unwrap().last;
                (r.map_err(|e| sdk_err(&e)), last.map(|l| done - l))
            }));
        }
        let mut max_ack = Duration::ZERO;
        for task in tasks {
            let (r, ack) = task.await.expect("sdk task");
            if let Err(e) = r {
                failures += 1;
                first_error.get_or_insert(e);
            }
            if let Some(a) = ack {
                max_ack = max_ack.max(a);
            }
        }
        let sdk_phase = sdk_started.elapsed();
        let body_times: Vec<BodyTimes> = bodies.lock().unwrap().iter().map(|b| *b.lock().unwrap()).collect();
        let open = |t: Instant| body_times.iter().any(|b| b.first.is_some_and(|f| f <= t) && b.last.is_none_or(|l| t < l));
        let stalls = sampler.stalls(sdk_started, Instant::now(), limit, &open);
        t.add(
            &format!("D{run}"),
            &format!("run {run}: {} x {} MiB at once", args.count, args.size_mib),
            "",
            format!(
                "client phase {}, slowest PUT {}, {} failed attempts ({stalled_attempts} stalled-stream), {client_failures} failed uploads, longest no-ingress window {}; SDK phase {}, max ACK gap {}, {} failed uploads, longest zero-progress window with a body still sending {}{}",
                common::secs(phase),
                common::secs(slowest),
                attempts,
                waits_client.map(|(d, _)| common::secs(d)).unwrap_or_else(|| "n/a".into()),
                common::secs(sdk_phase),
                common::secs(max_ack),
                failures - client_failures,
                stalls.map(|(d, n)| format!("{} ({n} >= {:.0} s)", common::secs(d), args.stall_secs)).unwrap_or_else(|| "n/a".into()),
                first_error.as_ref().map(|e| format!("; first error: {e}")).unwrap_or_default(),
            ),
            None,
        );
        let stalls = stalls.map(|(d, n)| (d, n + stalled_attempts));
        results.push(RunResult { phase, slowest, failures, attempts, sdk_phase, max_ack, stalls });
        // Free the store's disk before the next run.
        delete_prefix(ctx, &args.bucket, &prefix).await;
    }
    sampler.stop();
    let med = |f: &dyn Fn(&RunResult) -> Duration| {
        let mut v: Vec<_> = results.iter().map(f).collect();
        v.sort();
        (v[v.len() / 2], *v.last().unwrap())
    };
    let (pm, pw) = med(&|r| r.phase);
    let (sm, sw) = med(&|r| r.slowest);
    let (am, aw) = med(&|r| r.max_ack);
    let (qm, qw) = med(&|r| r.sdk_phase);
    let stall_count: usize = results.iter().filter_map(|r| r.stalls.map(|s| s.1)).sum();
    let longest = results.iter().filter_map(|r| r.stalls.map(|s| s.0)).max();
    let failures: usize = results.iter().map(|r| r.failures).sum();
    let attempts: usize = results.iter().map(|r| r.attempts).sum();
    t.add("Dm", "median / worst over the runs", "", format!(
        "client phase {} / {}; slowest PUT {} / {}; SDK phase {} / {}; max ACK gap {} / {}; failed attempts {attempts}; failed uploads {failures}",
        common::secs(pm), common::secs(pw), common::secs(sm), common::secs(sw), common::secs(qm), common::secs(qw), common::secs(am), common::secs(aw)
    ), None);
    t.check("Ds", &format!("zero-progress stalls >= {:.0} s while a body is sending (sampled windows + client stalled-stream detections)", args.stall_secs), "none", match longest {
        Some(l) => format!("{stall_count} (longest window {})", common::secs(l)),
        None => "not measured (no --rx-counter)".into(),
    }, stall_count == 0);
    t.check("Df", "failed uploads", "0", failures.to_string(), failures == 0);
    t.print(&format!("D. Upload burst — {}", ctx.label));
    println!("BURST_SUMMARY label={} phase_median_s={:.3} phase_worst_s={:.3} slowest_put_median_s={:.3} slowest_put_worst_s={:.3} ack_gap_median_s={:.3} ack_gap_worst_s={:.3} stalls={stall_count} failures={failures}",
        ctx.label, pm.as_secs_f64(), pw.as_secs_f64(), sm.as_secs_f64(), sw.as_secs_f64(), am.as_secs_f64(), aw.as_secs_f64());
    t.all_pass()
}

async fn delete_prefix(ctx: &Ctx, bucket: &str, prefix: &str) {
    let s3 = ctx.sdk(&ctx.admin);
    let Ok(out) = s3.list_objects_v2().bucket(bucket).prefix(prefix).send().await else { return };
    let ids: Vec<_> = out.contents().iter().filter_map(|o| o.key()).filter_map(|k| ObjectIdentifier::builder().key(k).build().ok()).collect();
    if ids.is_empty() {
        return;
    }
    let _ = s3.delete_objects().bucket(bucket).delete(Delete::builder().set_objects(Some(ids)).build().unwrap()).send().await;
}

#[derive(Clone, Copy, Default)]
struct BodyTimes {
    first: Option<Instant>,
    last: Option<Instant>,
}

/// A 128 MiB body handed over in 1 MiB frames, recording when its first and
/// last frames were taken by the connection.
struct TimedBody {
    data: Bytes,
    times: Arc<Mutex<BodyTimes>>,
}

impl http_body::Body for TimedBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Result<http_body::Frame<Bytes>, Infallible>>> {
        if self.data.is_empty() {
            return Poll::Ready(None);
        }
        let n = self.data.len().min(MIB);
        let chunk = self.data.split_to(n);
        let now = Instant::now();
        let mut times = self.times.lock().unwrap();
        times.first.get_or_insert(now);
        if self.data.is_empty() {
            times.last = Some(now);
        }
        drop(times);
        Poll::Ready(Some(Ok(http_body::Frame::data(chunk))))
    }

    fn is_end_stream(&self) -> bool {
        self.data.is_empty()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.data.len() as u64)
    }
}

fn timed_body(data: Bytes, times: Arc<Mutex<BodyTimes>>) -> ByteStream {
    // The SDK may build the body more than once (a retry replays it); the
    // times span them all: first frame of any, last frame of the latest.
    ByteStream::new(SdkBody::retryable(move || {
        SdkBody::from_body_1_x(TimedBody { data: data.clone(), times: times.clone() })
    }))
}

pub async fn first_write(ctx: &Ctx, repeats: usize) -> bool {
    let mut t = Table::default();
    let s3 = ctx.sdk(&ctx.admin);
    let mut writes = Vec::new();
    for i in 0..repeats {
        let bucket = common::unique("fw").to_lowercase();
        let t0 = Instant::now();
        let created = s3.create_bucket().bucket(&bucket).send().await;
        let create = t0.elapsed();
        let t1 = Instant::now();
        let put = ctx.store.put_bytes(&ctx.admin, ctx.target(&bucket, "first/object"), Bytes::from(synthetic(4096, 60 + i as u64))).await;
        let write = t1.elapsed();
        t.add(&format!("F{i}"), "CreateBucket, then the first put_bytes (4 KiB, client)", "", format!("create {} ({}), first write {} ({})", common::ms(create), created.as_ref().map(|_| "ok".to_string()).unwrap_or_else(sdk_err), common::ms(write), put.as_ref().map(|_| "ok".to_string()).unwrap_or_else(|e| e.to_string())), None);
        if put.is_ok() {
            writes.push(write);
        }
        let _ = s3.delete_object().bucket(&bucket).key("first/object").send().await;
        let _ = s3.delete_bucket().bucket(&bucket).send().await;
    }
    writes.sort();
    let median = writes.get(writes.len() / 2).copied().unwrap_or_default();
    t.check("Fm", "first write median", "succeeds", format!("{} (max {})", common::ms(median), common::ms(writes.last().copied().unwrap_or_default())), writes.len() == repeats);
    t.print(&format!("D. First write to a new bucket — {}", ctx.label));
    println!("FIRST_WRITE label={} median_ms={:.1}", ctx.label, median.as_secs_f64() * 1000.0);
    t.all_pass()
}
