//! Criterion B1: a mismatched Content-MD5 gets 400 and nothing is stored or
//! committed. The object-store client always sends the right digest, so the
//! wrong one is sent with the SDK in the client's request shape
//! (UNSIGNED-PAYLOAD body, Content-MD5 header). Requests are made with the
//! writer credentials (a vended session, for the spike); what was stored is
//! read back with the admin keys, since a session may not list parts.

use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};

use pplx_space_object_store::ObjectStore;

use crate::behaviours::base64_md5;
use crate::common::{self, Ctx, MIB, Table, sdk_err, sha256_hex, synthetic};

pub async fn run(ctx: &Ctx, bucket: &str, key_prefix: &str) -> bool {
    let mut t = Table::default();
    if let Err(e) = ctx.ensure_bucket(bucket).await {
        t.check("B1.0", "create bucket", "200", e, false);
        t.print(&format!("B1. Content-MD5 — {}", ctx.label));
        return false;
    }
    let s3 = ctx.sdk(&ctx.writer);
    let observer = ctx.sdk(&ctx.admin);
    let prefix = if key_prefix.is_empty() { common::unique("b1") } else { format!("{key_prefix}/{}", common::unique("b1")) };
    let body = synthetic(4 * MIB, 11);
    let other = synthetic(4 * MIB, 12);
    let wrong_md5 = base64_md5(&other);

    // New key, wrong digest, unsigned payload.
    let k_new = format!("{prefix}/new");
    let r = s3.put_object().bucket(bucket).key(&k_new).content_md5(&wrong_md5).body(ByteStream::from(body.clone())).customize().disable_payload_signing().send().await;
    let obs = r.as_ref().map(|_| "200 stored".to_string()).unwrap_or_else(sdk_err);
    t.check("B1.1", "PutObject (UNSIGNED-PAYLOAD) with a mismatched Content-MD5", "HTTP 400 BadDigest", obs.clone(), obs.starts_with("HTTP 400"));
    let head = observer.head_object().bucket(bucket).key(&k_new).send().await;
    t.check("B1.2", "the key afterwards", "HTTP 404 (nothing stored)", head.as_ref().map(|o| format!("exists, {} bytes", o.content_length().unwrap_or(-1))).unwrap_or_else(sdk_err), head.is_err());

    // Same with a signed payload (SigV4 hash of the body is correct, digest is not).
    let k_signed = format!("{prefix}/signed");
    let r = s3.put_object().bucket(bucket).key(&k_signed).content_md5(&wrong_md5).body(ByteStream::from(body.clone())).send().await;
    let obs = r.as_ref().map(|_| "200 stored".to_string()).unwrap_or_else(sdk_err);
    t.check("B1.3", "PutObject (signed payload) with a mismatched Content-MD5", "HTTP 400 BadDigest", obs.clone(), obs.starts_with("HTTP 400"));
    let head = observer.head_object().bucket(bucket).key(&k_signed).send().await;
    t.check("B1.4", "the key afterwards", "HTTP 404", head.as_ref().map(|_| "exists".to_string()).unwrap_or_else(sdk_err), head.is_err());

    // Overwrite of an existing object with a bad digest leaves the original.
    let k_over = format!("{prefix}/overwrite");
    let first = ctx.store.put_bytes(&ctx.writer, ctx.target(bucket, &k_over), bytes::Bytes::from(body.clone())).await;
    // `other` under `body`'s digest: the digest of a different body.
    let r = s3.put_object().bucket(bucket).key(&k_over).content_md5(base64_md5(&body)).body(ByteStream::from(other.clone())).customize().disable_payload_signing().send().await;
    let now = ctx.store.get_bytes(&ctx.admin, ctx.target(bucket, &k_over)).await.map(|b| sha256_hex(&b)).unwrap_or_default();
    t.check(
        "B1.5",
        "overwrite an existing object with a mismatched Content-MD5",
        "HTTP 400; original object unchanged",
        format!("first put {first:?}; overwrite {}; object = original: {}", r.as_ref().map(|_| "200".to_string()).unwrap_or_else(sdk_err), now == sha256_hex(&body)),
        first.is_ok() && r.is_err() && now == sha256_hex(&body),
    );

    // UploadPart with a bad digest: refused, no part recorded, Complete fails.
    let k_mp = format!("{prefix}/multipart");
    let observed = async {
        let up = s3.create_multipart_upload().bucket(bucket).key(&k_mp).send().await.map_err(|e| sdk_err(&e))?;
        let id = up.upload_id().unwrap_or_default().to_string();
        let good = s3.upload_part().bucket(bucket).key(&k_mp).upload_id(&id).part_number(1).content_md5(base64_md5(&body[..]))
            .body(ByteStream::from(body.clone())).customize().disable_payload_signing().send().await.map_err(|e| format!("good part: {}", sdk_err(&e)))?;
        let bad = s3.upload_part().bucket(bucket).key(&k_mp).upload_id(&id).part_number(2).content_md5(&wrong_md5)
            .body(ByteStream::from(body.clone())).customize().disable_payload_signing().send().await;
        let bad_obs = bad.as_ref().map(|_| "200 accepted".to_string()).unwrap_or_else(sdk_err);
        let parts = observer.list_parts().bucket(bucket).key(&k_mp).upload_id(&id).send().await;
        let part_numbers: Vec<i32> = parts.as_ref().map(|p| p.parts().iter().filter_map(|p| p.part_number()).collect()).unwrap_or_default();
        // Completing with the bad part's would-be ETag must fail.
        let fake = format!("\"{}\"", hex::encode(md5_raw(&body)));
        let complete = s3.complete_multipart_upload().bucket(bucket).key(&k_mp).upload_id(&id)
            .multipart_upload(CompletedMultipartUpload::builder()
                .parts(CompletedPart::builder().part_number(1).e_tag(good.e_tag().unwrap_or_default()).build())
                .parts(CompletedPart::builder().part_number(2).e_tag(fake).build())
                .build())
            .send().await;
        let complete_obs = complete.as_ref().map(|_| "200 completed".to_string()).unwrap_or_else(sdk_err);
        let head = observer.head_object().bucket(bucket).key(&k_mp).send().await;
        let _ = s3.abort_multipart_upload().bucket(bucket).key(&k_mp).upload_id(&id).send().await;
        Ok::<_, String>((bad_obs, part_numbers, complete_obs, head.is_err()))
    }
    .await;
    match observed {
        Ok((bad, parts, complete, absent)) => {
            t.check("B1.6", "UploadPart (UNSIGNED-PAYLOAD) with a mismatched Content-MD5", "HTTP 400 BadDigest", bad.clone(), bad.starts_with("HTTP 400"));
            t.check("B1.7", "ListParts after the refused part", "only part 1", format!("{parts:?}"), parts == vec![1]);
            t.check("B1.8", "CompleteMultipartUpload naming the refused part", "HTTP 400 InvalidPart; no object", format!("{complete}; object absent = {absent}"), complete.starts_with("HTTP 400") && absent);
        }
        Err(e) => t.check("B1.6", "UploadPart with a mismatched Content-MD5", "HTTP 400", e, false),
    }

    t.print(&format!("B1. Mismatched Content-MD5 — {}", ctx.label));
    t.all_pass()
}

fn md5_raw(data: &[u8]) -> Vec<u8> {
    use md5::Digest as _;
    md5::Md5::digest(data).to_vec()
}
