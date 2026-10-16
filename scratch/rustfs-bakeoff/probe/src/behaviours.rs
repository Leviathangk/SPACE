//! Criterion A: the S3 behaviours SPACE relies on.

use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart, Delete, ObjectIdentifier};
use bytes::Bytes;
use pplx_space_object_store::{
    ObjectStore, StreamingUploadOptions, UploadBufferBudget, UploadSource,
};
use tokio_util::sync::CancellationToken;

use crate::common::{self, Ctx, MIB, Table, creds, sdk_err, sha256_hex, synthetic};

pub async fn run(ctx: &Ctx, bucket: &str) -> bool {
    let mut t = Table::default();
    if let Err(e) = ctx.ensure_bucket(bucket).await {
        t.check("A0", "create bucket", "200", e, false);
        t.print(&format!("A. S3 behaviours — {}", ctx.label));
        return false;
    }
    let s3 = ctx.sdk(&ctx.admin);
    let prefix = common::unique("a");
    // The crate's content-addressed keys carry `sha256:`; keep one here.
    let key = |name: &str| format!("{prefix}/sha256:{name}");

    // A1 put_bytes + head through the object-store client.
    let small = synthetic(4096, 1);
    let k_small = key("small");
    let r = ctx.store.put_bytes(&ctx.admin, ctx.target(bucket, &k_small), Bytes::from(small.clone())).await;
    let head = ctx.store.head(&ctx.admin, ctx.target(bucket, &k_small)).await;
    match (&r, &head) {
        (Ok(()), Ok(Some(meta))) => t.check("A1", "put_bytes then HeadObject (client)", "size 4096", format!("size {}", meta.size), meta.size == 4096),
        _ => t.check("A1", "put_bytes then HeadObject (client)", "size 4096", format!("{r:?} / {head:?}"), false),
    }
    let missing = ctx.store.head(&ctx.admin, ctx.target(bucket, &key("absent"))).await;
    t.check("A2", "HeadObject on a missing key (client)", "None (404)", format!("{missing:?}"), matches!(missing, Ok(None)));

    // A3 put_file multipart: 40 MiB in 8 MiB parts -> Create, 5 x UploadPart, Complete.
    let (path, sha) = common::write_synthetic("a-multipart.bin", 40 * MIB, 2).await;
    let k_mp = key("multipart");
    let put = ctx.store.put_file(&ctx.admin, ctx.target(bucket, &k_mp), &path).await;
    let head = ctx.store.head(&ctx.admin, ctx.target(bucket, &k_mp)).await;
    let etag = head.as_ref().ok().and_then(|m| m.as_ref()).and_then(|m| m.etag.clone()).unwrap_or_default();
    t.check(
        "A3",
        "put_file 40 MiB, 8 MiB parts: CreateMultipartUpload, UploadPart x5, Complete (client; UNSIGNED-PAYLOAD + Content-MD5)",
        "Ok(41943040), multipart ETag",
        format!("{put:?}, ETag {etag}"),
        matches!(put, Ok(n) if n == 40 * MIB as u64) && etag.contains('-'),
    );

    // A4 ranged If-Match download through the client: 8 MiB chunks, 4 readers.
    let dst = common::scratch_dir().join("a-multipart.down");
    let got = ctx.store.get_file(&ctx.admin, ctx.target(bucket, &k_mp), &dst).await;
    let down = common::sha256_file(&dst).await.map(|(_, s)| s).unwrap_or_default();
    t.check(
        "A4",
        "get_file: ranged GETs with If-Match (client)",
        "same SHA-256 as source",
        format!("{got:?}, sha256 match = {}", down == sha),
        got.is_ok() && down == sha,
    );

    // A5 Range + If-Match directly: a matching ETag returns the range, a stale one 412.
    let ranged = s3.get_object().bucket(bucket).key(&k_mp).range("bytes=100-199").if_match(&etag).send().await;
    match ranged {
        Ok(out) => {
            let len = out.content_length().unwrap_or(-1);
            let body = out.body.collect().await.map(|b| b.into_bytes()).unwrap_or_default();
            let want = std::fs::read(&path).map(|d| d[100..200].to_vec()).unwrap_or_default();
            t.check("A5", "GET Range bytes=100-199 + If-Match current ETag", "206, the 100 source bytes", format!("Content-Length {len}, bytes match = {}", body.as_ref() == want.as_slice()), len == 100 && body.as_ref() == want.as_slice());
        }
        Err(e) => t.check("A5", "GET Range bytes=100-199 + If-Match current ETag", "206", sdk_err(&e), false),
    }
    let stale = s3.get_object().bucket(bucket).key(&k_mp).range("bytes=0-99").if_match("\"0123456789abcdef0123456789abcdef-5\"").send().await;
    let obs = match &stale { Ok(_) => "200/206 (served)".to_string(), Err(e) => sdk_err(e) };
    t.check("A6", "GET Range + If-Match stale ETag", "HTTP 412 PreconditionFailed", obs.clone(), obs.starts_with("HTTP 412"));

    // A7 streaming upload (volume-manager's snapshot push path): 20 MiB in 5 MiB parts.
    let data = synthetic(20 * MIB, 3);
    let want = sha256_hex(&data);
    let k_stream = key("stream");
    let r = put_stream(ctx, bucket, &k_stream, data, 5 * MIB, 4).await;
    let back = ctx.store.get_bytes(&ctx.admin, ctx.target(bucket, &k_stream)).await.map(|b| sha256_hex(&b)).unwrap_or_default();
    t.check("A7", "put_stream 20 MiB in 5 MiB parts (client streaming multipart, UNSIGNED-PAYLOAD + Content-MD5)", "Ok, same SHA-256", format!("{r}, sha256 match = {}", back == want), r.starts_with("Ok") && back == want);
    let tiny = synthetic(100_000, 4);
    let want_tiny = sha256_hex(&tiny);
    let k_tiny = key("stream-small");
    let r = put_stream(ctx, bucket, &k_tiny, tiny, 5 * MIB, 4).await;
    let back = ctx.store.get_bytes(&ctx.admin, ctx.target(bucket, &k_tiny)).await.map(|b| sha256_hex(&b)).unwrap_or_default();
    t.check("A8", "put_stream below one part (client single PUT)", "Ok, same SHA-256", format!("{r}, sha256 match = {}", back == want_tiny), r.starts_with("Ok") && back == want_tiny);

    // A9 explicit abort: Create, UploadPart, Abort, then the upload is gone.
    let k_abort = key("abort");
    let observed = async {
        let up = s3.create_multipart_upload().bucket(bucket).key(&k_abort).send().await.map_err(|e| sdk_err(&e))?;
        let id = up.upload_id().unwrap_or_default().to_string();
        s3.upload_part().bucket(bucket).key(&k_abort).upload_id(&id).part_number(1).body(ByteStream::from(synthetic(5 * MIB, 5))).send().await.map_err(|e| sdk_err(&e))?;
        s3.abort_multipart_upload().bucket(bucket).key(&k_abort).upload_id(&id).send().await.map_err(|e| format!("abort: {}", sdk_err(&e)))?;
        let parts = s3.list_parts().bucket(bucket).key(&k_abort).upload_id(&id).send().await;
        let listed = s3.list_multipart_uploads().bucket(bucket).prefix(&k_abort).send().await.map(|o| o.uploads().len()).unwrap_or(usize::MAX);
        let head = s3.head_object().bucket(bucket).key(&k_abort).send().await;
        Ok::<_, String>(format!(
            "abort ok; ListParts {}; uploads listed {listed}; object {}",
            match parts { Ok(_) => "still answers".to_string(), Err(e) => sdk_err(&e) },
            match head { Ok(_) => "exists".to_string(), Err(e) => sdk_err(&e) },
        ))
    }
    .await;
    let ok = matches!(&observed, Ok(s) if s.contains("ListParts HTTP 404") && s.contains("uploads listed 0") && s.contains("object HTTP 404"));
    t.check("A9", "CreateMultipartUpload, UploadPart, AbortMultipartUpload; upload and object gone", "abort 204; ListParts 404 NoSuchUpload; object 404", observed.unwrap_or_else(|e| e), ok);

    // A10 UploadPartCopy with source ranges (the client's > 4 GiB copy shape,
    // here over the 40 MiB object): two ranges, then Complete.
    let k_copy = key("copy");
    let observed = async {
        let up = s3.create_multipart_upload().bucket(bucket).key(&k_copy).send().await.map_err(|e| sdk_err(&e))?;
        let id = up.upload_id().unwrap_or_default().to_string();
        let source = format!("{bucket}/{}", k_mp.replace(':', "%3A"));
        let split = 24 * MIB - 1;
        let mut parts = Vec::new();
        for (n, range) in [(1, format!("bytes=0-{split}")), (2, format!("bytes={}-{}", split + 1, 40 * MIB - 1))] {
            let r = s3.upload_part_copy().bucket(bucket).key(&k_copy).upload_id(&id).part_number(n).copy_source(&source).copy_source_range(range).send().await.map_err(|e| format!("part {n}: {}", sdk_err(&e)))?;
            let etag = r.copy_part_result().and_then(|c| c.e_tag()).unwrap_or_default().to_string();
            parts.push(CompletedPart::builder().part_number(n).e_tag(etag).build());
        }
        s3.complete_multipart_upload().bucket(bucket).key(&k_copy).upload_id(&id).multipart_upload(CompletedMultipartUpload::builder().set_parts(Some(parts)).build()).send().await.map_err(|e| format!("complete: {}", sdk_err(&e)))?;
        let dst = common::scratch_dir().join("a-copy.down");
        ctx.store.get_file(&ctx.admin, ctx.target(bucket, &k_copy), &dst).await.map_err(|e| e.to_string())?;
        let (n, s) = common::sha256_file(&dst).await.map_err(|e| e.to_string())?;
        Ok::<_, String>((n, s))
    }
    .await;
    match observed {
        Ok((n, s)) => t.check("A10", "UploadPartCopy x2 with x-amz-copy-source-range, Complete", "copy identical to source", format!("{n} bytes, sha256 match = {}", s == sha), s == sha),
        Err(e) => t.check("A10", "UploadPartCopy x2 with x-amz-copy-source-range, Complete", "copy identical to source", e, false),
    }

    // A11 ListObjectsV2 pagination over 25 keys, 10 per page.
    let list_prefix = format!("{prefix}/list/");
    let mut put_err = None;
    for i in 0..25 {
        if let Err(e) = s3.put_object().bucket(bucket).key(format!("{list_prefix}{i:02}")).body(ByteStream::from_static(b"x")).send().await {
            put_err = Some(sdk_err(&e));
        }
    }
    let mut pages = 0;
    let mut seen = Vec::new();
    let mut token: Option<String> = None;
    let mut list_err = None;
    loop {
        let mut req = s3.list_objects_v2().bucket(bucket).prefix(&list_prefix).max_keys(10);
        if let Some(tok) = &token {
            req = req.continuation_token(tok);
        }
        match req.send().await {
            Ok(out) => {
                pages += 1;
                seen.extend(out.contents().iter().filter_map(|o| o.key().map(str::to_string)));
                token = out.next_continuation_token().map(str::to_string);
                if !out.is_truncated().unwrap_or(false) || pages > 10 {
                    break;
                }
            }
            Err(e) => {
                list_err = Some(sdk_err(&e));
                break;
            }
        }
    }
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    t.check(
        "A11",
        "ListObjectsV2 max-keys 10 over 25 keys, continuation tokens",
        "3 pages, 25 distinct keys in order",
        format!("{pages} pages, {} keys, {} distinct, ordered = {}{}", seen.len(), sorted.len(), sorted == seen, list_err.or(put_err).map(|e| format!(", error {e}")).unwrap_or_default()),
        pages == 3 && seen.len() == 25 && sorted == seen,
    );

    // A12 DeleteObjects of those 25 keys in one request.
    let ids: Vec<_> = (0..25).map(|i| ObjectIdentifier::builder().key(format!("{list_prefix}{i:02}")).build().unwrap()).collect();
    let del = s3.delete_objects().bucket(bucket).delete(Delete::builder().set_objects(Some(ids)).build().unwrap()).send().await;
    let left = s3.list_objects_v2().bucket(bucket).prefix(&list_prefix).send().await.map(|o| o.key_count().unwrap_or(-1)).unwrap_or(-1);
    match del {
        Ok(out) => t.check("A12", "DeleteObjects (25 keys, one request)", "25 deleted, 0 errors, 0 left", format!("{} deleted, {} errors, {left} left", out.deleted().len(), out.errors().len()), out.deleted().len() == 25 && out.errors().is_empty() && left == 0),
        Err(e) => t.check("A12", "DeleteObjects (25 keys, one request)", "25 deleted", sdk_err(&e), false),
    }

    // A13-A15 signature checks.
    let bad_secret = creds(&ctx.admin.access_key_id, "not-the-secret-key-at-all", None);
    let r = ctx.store.head(&bad_secret, ctx.target(bucket, &k_small)).await;
    t.check("A13", "HeadObject signed with a wrong secret (client)", "refused", r.as_ref().map(|m| format!("{m:?}")).unwrap_or_else(|e| common::short_err(&e.to_string())), r.is_err());
    let r = ctx.sdk(&bad_secret).put_object().bucket(bucket).key(key("badsig")).body(ByteStream::from_static(b"nope")).send().await;
    let obs = match &r { Ok(_) => "stored".to_string(), Err(e) => sdk_err(e) };
    t.check("A14", "PutObject signed with a wrong secret", "HTTP 403 SignatureDoesNotMatch", obs.clone(), obs.starts_with("HTTP 403"));
    let r = ctx.sdk(&creds("AKIANOSUCHKEY0000000", "x", None)).get_object().bucket(bucket).key(&k_small).send().await;
    let obs = match &r { Ok(_) => "served".to_string(), Err(e) => sdk_err(e) };
    t.check("A15", "GetObject with an unknown access key", "refused (403 InvalidAccessKeyId on S3)", obs.clone(), obs.starts_with("HTTP 4"));
    let present = s3.head_object().bucket(bucket).key(key("badsig")).send().await;
    t.check("A16", "nothing stored by the refused PUT", "HTTP 404", match &present { Ok(_) => "exists".into(), Err(e) => sdk_err(e) }, present.is_err());

    // A17 UNSIGNED-PAYLOAD stated explicitly on a single PUT with Content-MD5.
    let body = synthetic(3 * MIB, 6);
    let md5 = base64_md5(&body);
    let k_unsigned = key("unsigned");
    let r = s3.put_object().bucket(bucket).key(&k_unsigned).content_md5(md5).body(ByteStream::from(body.clone())).customize().disable_payload_signing().send().await;
    let back = ctx.store.get_bytes(&ctx.admin, ctx.target(bucket, &k_unsigned)).await.map(|b| sha256_hex(&b)).unwrap_or_default();
    t.check("A17", "PutObject x-amz-content-sha256: UNSIGNED-PAYLOAD + Content-MD5", "200, same SHA-256", format!("{}, sha256 match = {}", r.as_ref().map(|_| "200".to_string()).unwrap_or_else(sdk_err), back == sha256_hex(&body)), r.is_ok() && back == sha256_hex(&body));

    // A18 aws-chunked STREAMING-UNSIGNED-PAYLOAD-TRAILER with a CRC32 trailer,
    // recorded only: the object-store client keeps checksum trailers off.
    let k_chunked = key("chunked");
    let path3 = common::scratch_dir().join("a-chunked.bin");
    std::fs::write(&path3, &body).unwrap();
    let stream = ByteStream::from_path(&path3).await.unwrap();
    let r = s3.put_object().bucket(bucket).key(&k_chunked).checksum_algorithm(aws_sdk_s3::types::ChecksumAlgorithm::Crc32).body(stream).send().await;
    let back = ctx.store.get_bytes(&ctx.admin, ctx.target(bucket, &k_chunked)).await.map(|b| sha256_hex(&b)).unwrap_or_default();
    t.add("A18", "PutObject STREAMING-UNSIGNED-PAYLOAD-TRAILER body with a CRC32 trailer (not a request SPACE's client sends; it turns trailers off)", "info", format!("{}, sha256 match = {}", r.as_ref().map(|_| "200".to_string()).unwrap_or_else(sdk_err), back == sha256_hex(&body)), None);

    t.print(&format!("A. S3 behaviours SPACE uses — {}", ctx.label));
    t.all_pass()
}

pub fn base64_md5(data: &[u8]) -> String {
    use base64::Engine as _;
    use md5::Digest as _;
    base64::engine::general_purpose::STANDARD.encode(md5::Md5::digest(data))
}

/// Uploads `data` through the client's streaming path. The producer is a
/// task writing into a pipe, as volume-manager's encoder does.
pub async fn put_stream(ctx: &Ctx, bucket: &str, key: &str, data: Vec<u8>, part: usize, concurrency: usize) -> String {
    put_stream_with(ctx, &ctx.admin, bucket, key, data, part, concurrency).await
}

pub async fn put_stream_with(
    ctx: &Ctx,
    c: &pplx_space_object_store::ObjectCredentials,
    bucket: &str,
    key: &str,
    data: Vec<u8>,
    part: usize,
    concurrency: usize,
) -> String {
    use tokio::io::AsyncWriteExt as _;
    let Some(uploader) = ctx.store.streaming_upload() else {
        return "no streaming capability".into();
    };
    let max = data.len() as u64 * 2 + MIB as u64;
    let options = match StreamingUploadOptions::new(part, concurrency, max) {
        Ok(o) => o,
        Err(e) => return format!("options: {e}"),
    };
    let budget = UploadBufferBudget::new(part * concurrency).expect("budget");
    let (reader, mut writer) = tokio::io::duplex(MIB);
    let cancel = CancellationToken::new();
    let producer = tokio::spawn(async move {
        for chunk in data.chunks(MIB) {
            writer.write_all(chunk).await?;
        }
        writer.shutdown().await
    });
    let finished = async move { producer.await.map_err(std::io::Error::other)? };
    let source = UploadSource::new(reader, finished, cancel);
    match uploader.put_stream(c, ctx.target(bucket, key), source, budget, options).await {
        Ok(n) => format!("Ok({n})"),
        Err(e) => format!("Err({e})"),
    }
}
