use pplx_space_db_test_fixtures::{TestDbResult, pg_test_db};

use super::*;

const ORG_A: OrgId = OrgId::from_uuid(uuid::Uuid::from_u128(0xa));
const ORG_B: OrgId = OrgId::from_uuid(uuid::Uuid::from_u128(0xb));

fn name(value: &str) -> TemplateName {
    TemplateName::parse(value).unwrap()
}

fn small_limits() -> TemplateBuildLimits {
    TemplateBuildLimits {
        org_pending_specs: 2,
        global_pending_specs: 3,
        org_active_builds: 1,
        global_active_builds: 2,
        org_uploads_in_flight: 1,
        global_uploads_in_flight: 2,
        org_input_mappings: 8,
        global_input_mappings: 16,
        org_input_bytes: 100,
        global_input_bytes: 1000,
        max_upload_bytes: 60,
        ..TemplateBuildLimits::default()
    }
}

fn rejection(
    result: Result<impl std::fmt::Debug, TemplateBuildAdmissionError>,
) -> AdmissionRejection {
    match result {
        Err(TemplateBuildAdmissionError::Rejected(rejection)) => rejection,
        other => panic!("expected an admission rejection, got {other:?}"),
    }
}

/// Expire every row of `table` for `org_id`, as if its lifetime had passed.
async fn expire(pool: &PgPool, table: &str, org_id: OrgId) -> TestDbResult<()> {
    let statement = match table {
        "template_build_admissions" => sql!(
            "UPDATE template_build_admissions SET expires_at = NOW() - INTERVAL '1 second'
             WHERE org_id = $1"
        ),
        "template_build_input_retention" => sql!(
            "UPDATE template_build_input_retention SET expires_at = NOW() - INTERVAL '1 second'
             WHERE org_id = $1"
        ),
        "template_build_input_charges" => sql!(
            "UPDATE template_build_input_charges SET expires_at = NOW() - INTERVAL '1 second'
             WHERE org_id = $1"
        ),
        other => panic!("no expiry statement for {other}"),
    };
    sqlx::query(statement)
        .bind(org_id.as_uuid())
        .execute(pool)
        .await?;
    Ok(())
}

/// One organization's abandoned specifications stop at its own limit, the
/// cluster limit protects everyone else, and expiry returns the capacity and
/// deletes the abandoned rows.
#[tokio::test]
async fn pending_specs_are_bounded_per_org_and_globally_and_expire() -> TestDbResult<()> {
    let db = pg_test_db().await?;
    let store = TemplateBuildAdmissionStore::new(db.pool.clone(), small_limits());
    let template = name("specs");
    for build in ["a1", "a2"] {
        store
            .admit_build_spec(ORG_A, &template, build, BuildSpec::default())
            .await?;
    }

    let org_limited = store
        .admit_build_spec(ORG_A, &template, "a3", BuildSpec::default())
        .await;
    store
        .admit_build_spec(ORG_B, &template, "b1", BuildSpec::default())
        .await?;
    let globally_limited = store
        .admit_build_spec(ORG_B, &template, "b2", BuildSpec::default())
        .await;

    assert_eq!(
        rejection(org_limited),
        AdmissionRejection {
            limit: AdmissionLimit::PendingSpecs,
            scope: AdmissionScope::Organization,
        }
    );
    assert_eq!(
        rejection(globally_limited),
        AdmissionRejection {
            limit: AdmissionLimit::PendingSpecs,
            scope: AdmissionScope::Global,
        }
    );

    expire(&db.pool, "template_build_admissions", ORG_A).await?;
    store
        .admit_build_spec(ORG_A, &template, "a3", BuildSpec::default())
        .await?;
    let remaining: Vec<String> = sqlx::query_scalar(sql!(
        "SELECT build_id FROM template_build_jobs WHERE template_id = $1 ORDER BY build_id"
    ))
    .bind(template.as_str())
    .fetch_all(&db.pool)
    .await?;
    assert_eq!(
        remaining,
        ["a3", "b1"],
        "expired specifications are deleted"
    );
    Ok(())
}

/// Concurrent replicas cannot both take the last unit of a limit.
#[tokio::test]
async fn concurrent_spec_admissions_never_exceed_the_limit() -> TestDbResult<()> {
    let db = pg_test_db().await?;
    let store = TemplateBuildAdmissionStore::new(
        db.pool.clone(),
        TemplateBuildLimits {
            org_pending_specs: 3,
            ..TemplateBuildLimits::default()
        },
    );
    let template = name("race");
    let attempts = (0..12).map(|index| {
        let store = store.clone();
        let template = template.clone();
        async move {
            store
                .admit_build_spec(ORG_A, &template, &format!("b{index}"), BuildSpec::default())
                .await
        }
    });

    let results = futures_util::future::join_all(attempts).await;

    let admitted = results.iter().filter(|result| result.is_ok()).count();
    assert_eq!(admitted, 3);
    assert!(results.into_iter().all(|result| match result {
        Ok(()) => true,
        Err(TemplateBuildAdmissionError::Rejected(rejection)) =>
            rejection.scope == AdmissionScope::Organization,
        Err(_) => false,
    }));
    Ok(())
}

/// Active builds: each organization stops at its own limit while another
/// still builds; a dispatched build cannot be dispatched again; completion and
/// a failed dispatch both return capacity; an unobserved build stops counting
/// once its lease lapses.
#[tokio::test]
async fn active_builds_are_isolated_and_capacity_recovers() -> TestDbResult<()> {
    let db = pg_test_db().await?;
    let store = TemplateBuildAdmissionStore::new(db.pool.clone(), small_limits());
    let template = name("builds");
    for (org, build) in [(ORG_A, "a1"), (ORG_A, "a2"), (ORG_B, "b1")] {
        store
            .admit_build_spec(org, &template, build, BuildSpec::default())
            .await?;
    }
    store.admit_build(ORG_A, &template, "a1").await?;

    let org_limited = store.admit_build(ORG_A, &template, "a2").await;
    store.admit_build(ORG_B, &template, "b1").await?;
    let again = store.admit_build(ORG_A, &template, "a1").await;
    let foreign = store.admit_build(ORG_B, &template, "a2").await;

    assert_eq!(
        rejection(org_limited),
        AdmissionRejection {
            limit: AdmissionLimit::ActiveBuilds,
            scope: AdmissionScope::Organization,
        }
    );
    assert!(matches!(
        again,
        Err(TemplateBuildAdmissionError::AlreadyStarted)
    ));
    assert!(matches!(
        foreign,
        Err(TemplateBuildAdmissionError::NoPendingBuild)
    ));

    assert!(store.finish_build(ORG_A, &template, "a1").await?);
    store.admit_build(ORG_A, &template, "a2").await?;
    let finished_again = store.admit_build(ORG_A, &template, "a1").await;
    assert!(matches!(
        finished_again,
        Err(TemplateBuildAdmissionError::AlreadyStarted)
    ));

    store.release_build(ORG_A, &template, "a2").await?;
    store.admit_build(ORG_A, &template, "a2").await?;

    expire(&db.pool, "template_build_admissions", ORG_B).await?;
    store
        .admit_build_spec(ORG_B, &template, "b2", BuildSpec::default())
        .await?;
    store.admit_build(ORG_B, &template, "b2").await?;
    let lapsed: String = sqlx::query_scalar(sql!(
        "SELECT state FROM template_build_admissions WHERE template_id = $1 AND build_id = 'b1'"
    ))
    .bind(template.as_str())
    .fetch_one(&db.pool)
    .await?;
    assert_eq!(lapsed, "finished", "a lapsed build keeps its record");
    Ok(())
}

/// Uploads: concurrency and stored bytes are charged to the uploader, the cap
/// of each upload is what its organization has left, an expired mapping is
/// deleted while its bytes stay charged, and the bytes return when their
/// charge window ends.
#[tokio::test]
async fn upload_quota_is_attributed_retained_and_recovers_after_expiry() -> TestDbResult<()> {
    let db = pg_test_db().await?;
    let store = TemplateBuildAdmissionStore::new(db.pool.clone(), small_limits());
    let template_a = name("inputs-a");
    let template_b = name("inputs-b");

    let first = store.reserve_input_upload(ORG_A).await?;
    let concurrent = store.reserve_input_upload(ORG_A).await;
    let other_org = store.reserve_input_upload(ORG_B).await?;
    assert_eq!(first.max_bytes, 60);
    assert_eq!(
        rejection(concurrent),
        AdmissionRejection {
            limit: AdmissionLimit::UploadsInFlight,
            scope: AdmissionScope::Organization,
        }
    );
    store.release_input_reservation(other_org).await?;

    store
        .record_input(first, ORG_A, &template_a, "one", "digest-one", 50)
        .await?;
    let second = store.reserve_input_upload(ORG_A).await?;
    assert_eq!(
        second.max_bytes, 50,
        "the cap is the organization's remainder"
    );
    store
        .record_input(second, ORG_A, &template_a, "two", "digest-two", 50)
        .await?;
    let exhausted = store.reserve_input_upload(ORG_A).await;
    assert_eq!(
        rejection(exhausted),
        AdmissionRejection {
            limit: AdmissionLimit::InputBytes,
            scope: AdmissionScope::Organization,
        }
    );
    let unaffected = store.reserve_input_upload(ORG_B).await?;
    assert_eq!(unaffected.max_bytes, 60);
    store
        .record_input(unaffected, ORG_B, &template_b, "one", "digest-b", 10)
        .await?;

    expire(&db.pool, "template_build_input_retention", ORG_A).await?;
    let still_stored = store.reserve_input_upload(ORG_A).await;
    assert_eq!(
        rejection(still_stored),
        AdmissionRejection {
            limit: AdmissionLimit::InputBytes,
            scope: AdmissionScope::Organization,
        },
        "an expired mapping's object can still exist, so its bytes stay charged"
    );
    let mappings: Vec<(String, String)> = sqlx::query_as(sql!(
        "SELECT template_id, files_hash FROM template_build_blobs ORDER BY template_id"
    ))
    .fetch_all(&db.pool)
    .await?;
    assert_eq!(
        mappings,
        [("inputs-b".to_string(), "one".to_string())],
        "expired mappings are deleted; the other organization's is retained"
    );

    expire(&db.pool, "template_build_input_charges", ORG_A).await?;
    let recovered = store.reserve_input_upload(ORG_A).await?;
    assert_eq!(recovered.max_bytes, 60);
    let open: i64 = sqlx::query_scalar(sql!(
        "SELECT COUNT(*) FROM template_build_input_charges WHERE state = 'reserved'"
    ))
    .fetch_one(&db.pool)
    .await?;
    assert_eq!(open, 1, "recorded and released uploads hold no reservation");
    Ok(())
}

/// Simulate an upload that is still running after its in-flight window.
async fn age_in_flight_upload(pool: &PgPool) -> TestDbResult<()> {
    sqlx::query(sql!(
        "UPDATE template_build_input_charges SET created_at = NOW() - INTERVAL '2 hours'"
    ))
    .execute(pool)
    .await?;
    Ok(())
}

/// Every upload stores an object, including one that replaces an existing
/// mapping, so uploading again under one file hash keeps charging until
/// those objects can have expired.
#[tokio::test]
async fn reuploading_one_mapping_charges_every_stored_object() -> TestDbResult<()> {
    let db = pg_test_db().await?;
    let store = TemplateBuildAdmissionStore::new(db.pool.clone(), small_limits());
    let template = name("reupload");
    for digest in ["digest-one", "digest-two"] {
        let reservation = store.reserve_input_upload(ORG_A).await?;
        store
            .record_input(reservation, ORG_A, &template, "files", digest, 50)
            .await?;
    }

    let third = store.reserve_input_upload(ORG_A).await;

    assert_eq!(
        rejection(third),
        AdmissionRejection {
            limit: AdmissionLimit::InputBytes,
            scope: AdmissionScope::Organization,
        },
        "the replaced mapping's object is still stored and still charged"
    );
    Ok(())
}

/// The builder stored the bytes before registration found that another
/// organization owns the template. Those bytes stay charged, and the upload
/// route's failure cleanup must not hand them back.
#[tokio::test]
async fn stored_upload_refused_at_registration_stays_charged() -> TestDbResult<()> {
    let db = pg_test_db().await?;
    let store = TemplateBuildAdmissionStore::new(db.pool.clone(), small_limits());
    let template = name("foreign");
    sqlx::query(sql!(
        "INSERT INTO templates (id, name, org_id) VALUES ($1, $2, $3)"
    ))
    .bind(uuid::Uuid::now_v7())
    .bind(template.as_str())
    .bind(pplx_space_db_test_fixtures::SEED_ORG_ID)
    .execute(&db.pool)
    .await?;
    let reservation = store.reserve_input_upload(ORG_A).await?;

    let refused = store
        .record_input(reservation, ORG_A, &template, "files", "digest", 60)
        .await;
    store.release_input_reservation(reservation).await?;

    assert!(matches!(refused, Err(RecordInputError::Conflict)));
    let next = store.reserve_input_upload(ORG_A).await?;
    assert_eq!(
        next.max_bytes, 40,
        "the refused upload's 60 bytes stay charged"
    );
    Ok(())
}

/// An upload still running past its in-flight window gives up its
/// concurrency slot but not its bytes, and still settles when it finishes.
#[tokio::test]
async fn upload_outliving_its_in_flight_window_keeps_its_bytes_charged() -> TestDbResult<()> {
    let db = pg_test_db().await?;
    let store = TemplateBuildAdmissionStore::new(db.pool.clone(), small_limits());
    let template = name("slow");
    let slow = store.reserve_input_upload(ORG_A).await?;
    age_in_flight_upload(&db.pool).await?;

    let next = store.reserve_input_upload(ORG_A).await?;
    assert_eq!(
        next.max_bytes, 40,
        "the slow upload's reservation still holds its bytes"
    );
    store
        .record_input(slow, ORG_A, &template, "one", "digest-one", 60)
        .await?;
    store
        .record_input(next, ORG_A, &template, "two", "digest-two", 40)
        .await?;

    let exhausted = store.reserve_input_upload(ORG_A).await;
    assert_eq!(
        rejection(exhausted),
        AdmissionRejection {
            limit: AdmissionLimit::InputBytes,
            scope: AdmissionScope::Organization,
        }
    );
    Ok(())
}

/// Backends of this database waiting on a lock, other than the caller.
async fn lock_waiters(pool: &PgPool) -> TestDbResult<i64> {
    Ok(sqlx::query_scalar(sql!(
        "SELECT COUNT(*) FROM pg_stat_activity
         WHERE datname = current_database()
           AND wait_event_type = 'Lock'
           AND pid <> pg_backend_pid()"
    ))
    .fetch_one(pool)
    .await?)
}

/// Settling an upload's bytes and recording its mapping are one admission
/// step: while a registration is in progress, its mapping slot cannot be
/// handed to another upload.
#[tokio::test]
async fn registration_keeps_its_mapping_slot_until_the_mapping_is_recorded() -> TestDbResult<()> {
    // The blocker, the lock probe, and both store calls each hold a connection.
    let db = pplx_space_db_test_fixtures::pg_test_db_with_max_connections(8).await?;
    let store = TemplateBuildAdmissionStore::new(
        db.pool.clone(),
        TemplateBuildLimits {
            org_input_mappings: 1,
            ..small_limits()
        },
    );
    let template = name("slot");
    let registering = store.reserve_input_upload(ORG_A).await?;
    // Hold the mapping row so registration stops just before inserting it.
    let mut blocker = db.pool.begin().await?;
    sqlx::query(sql!(
        "INSERT INTO template_build_blobs (template_id, files_hash, blob_sha256, org_id)
         VALUES ($1, 'files', 'blocker', $2)"
    ))
    .bind(template.as_str())
    .bind(ORG_A.as_uuid())
    .execute(&mut *blocker)
    .await?;
    let record = tokio::spawn({
        let store = store.clone();
        let template = template.clone();
        async move {
            store
                .record_input(registering, ORG_A, &template, "files", "digest", 10)
                .await
        }
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while lock_waiters(&db.pool).await? < 1 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "registration never blocked"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let competing = tokio::spawn({
        let store = store.clone();
        async move { store.reserve_input_upload(ORG_A).await }
    });
    while !competing.is_finished() && lock_waiters(&db.pool).await? < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "competing upload neither finished nor waited"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    blocker.rollback().await?;

    record.await??;
    assert_eq!(
        rejection(competing.await?),
        AdmissionRejection {
            limit: AdmissionLimit::InputMappings,
            scope: AdmissionScope::Organization,
        },
        "the registering upload's mapping slot was never free"
    );
    Ok(())
}
