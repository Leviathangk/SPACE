use axum::http::{HeaderName, HeaderValue, StatusCode};
use pplx_space_common_core::types::{UpstreamIssuer, UpstreamOrgId, UpstreamSubjectId};

use crate::federated_identity::{
    FEDERATED_SERVICE_GRANT_EXPIRES_AT_HEADER, FEDERATED_SERVICE_GRANT_HEADER,
    FEDERATED_SERVICE_GRANT_ISSUED_AT_HEADER, FederatedOwner,
};

use super::*;

const GRANT_EXPIRES_AT_UNIX: i64 = 1_797_120_000;

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        headers.insert(
            HeaderName::try_from(*name).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    headers
}

fn owner() -> FederatedOwner {
    FederatedOwner {
        issuer: UpstreamIssuer::parse("example-issuer").unwrap(),
        org_id: UpstreamOrgId::parse("example-org-1").unwrap(),
        subject_id: UpstreamSubjectId::parse("123").unwrap(),
    }
}

/// Non-federated ECW traffic is untouched by the grant requirement, including
/// when a caller sends grant headers it has no owner for.
#[test]
fn requests_without_a_federated_owner_need_no_grant() {
    assert!(
        request_federated_service_grant(&headers(&[]), None)
            .unwrap()
            .is_none()
    );
    assert!(
        request_federated_service_grant(
            &headers(&[(FEDERATED_SERVICE_GRANT_HEADER, "opaque-grant")]),
            None
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn federated_owner_grant_carries_the_provider_expiry() {
    let grant = request_federated_service_grant(
        &headers(&[
            (FEDERATED_SERVICE_GRANT_HEADER, "opaque-grant"),
            (
                FEDERATED_SERVICE_GRANT_EXPIRES_AT_HEADER,
                &GRANT_EXPIRES_AT_UNIX.to_string(),
            ),
        ]),
        Some(&owner()),
    )
    .unwrap()
    .expect("a federated owner yields a grant");

    assert_eq!(grant.as_bytes(), b"opaque-grant");
    assert_eq!(grant.expires_at_unix(), GRANT_EXPIRES_AT_UNIX);
}

/// The grant and its expiry are one artifact, so half of one is refused the
/// same way a wholly missing grant is: SPACE has no basis for choosing the
/// lifetime of a blob it cannot read.
#[test]
fn federated_owner_without_a_grant_expiry_is_unauthorized() {
    let err = request_federated_service_grant(
        &headers(&[(FEDERATED_SERVICE_GRANT_HEADER, "opaque-grant")]),
        Some(&owner()),
    )
    .unwrap_err();

    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
}

/// Connect's permissive variant: fully absent grant headers are a no-op even
/// for a federated owner — connect must not gain a hard header requirement —
/// while a partial or malformed grant is rejected exactly like refresh.
#[test]
fn connect_grant_is_optional_when_headers_are_absent() {
    // Fully absent: no renewal, no rejection.
    assert!(
        optional_request_federated_service_grant(&headers(&[]), Some(&owner()), false)
            .unwrap()
            .is_none()
    );
    // Non-federated traffic with stray grant headers stays untouched.
    assert!(
        optional_request_federated_service_grant(
            &headers(&[(FEDERATED_SERVICE_GRANT_HEADER, "opaque-grant")]),
            None,
            false,
        )
        .unwrap()
        .is_none()
    );

    // A complete grant renews.
    let grant = optional_request_federated_service_grant(
        &headers(&[
            (FEDERATED_SERVICE_GRANT_HEADER, "opaque-grant"),
            (
                FEDERATED_SERVICE_GRANT_EXPIRES_AT_HEADER,
                &GRANT_EXPIRES_AT_UNIX.to_string(),
            ),
        ]),
        Some(&owner()),
        false,
    )
    .unwrap()
    .expect("a complete grant must be parsed for renewal");
    assert_eq!(grant.as_bytes(), b"opaque-grant");
    assert_eq!(grant.expires_at_unix(), GRANT_EXPIRES_AT_UNIX);
}

/// Half a grant is not "absent": a blob without its expiry (or vice versa)
/// is malformed and must be rejected, never silently skipped.
#[test]
fn connect_grant_with_partial_headers_is_unauthorized() {
    let err = optional_request_federated_service_grant(
        &headers(&[(FEDERATED_SERVICE_GRANT_HEADER, "opaque-grant")]),
        Some(&owner()),
        false,
    )
    .unwrap_err();
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);

    let err = optional_request_federated_service_grant(
        &headers(&[(
            FEDERATED_SERVICE_GRANT_EXPIRES_AT_HEADER,
            &GRANT_EXPIRES_AT_UNIX.to_string(),
        )]),
        Some(&owner()),
        false,
    )
    .unwrap_err();
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);

    let err = optional_request_federated_service_grant(
        &headers(&[
            (FEDERATED_SERVICE_GRANT_HEADER, "opaque-grant"),
            (FEDERATED_SERVICE_GRANT_EXPIRES_AT_HEADER, "not-a-unix-ts"),
        ]),
        Some(&owner()),
        false,
    )
    .unwrap_err();
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);

    // An issued-at with nothing else is a partial envelope, not absence.
    let err = optional_request_federated_service_grant(
        &headers(&[(
            FEDERATED_SERVICE_GRANT_ISSUED_AT_HEADER,
            &GRANT_EXPIRES_AT_UNIX.to_string(),
        )]),
        Some(&owner()),
        false,
    )
    .unwrap_err();
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);

    let err = optional_request_federated_service_grant(
        &headers(&[
            (FEDERATED_SERVICE_GRANT_HEADER, "opaque-grant"),
            (
                FEDERATED_SERVICE_GRANT_EXPIRES_AT_HEADER,
                &GRANT_EXPIRES_AT_UNIX.to_string(),
            ),
            (FEDERATED_SERVICE_GRANT_ISSUED_AT_HEADER, "not-a-unix-ts"),
        ]),
        Some(&owner()),
        false,
    )
    .unwrap_err();
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
}

/// The RFC's end-state contract, flippable without a code change: with
/// the grant requirement on, an absent grant pair on a federated
/// connect is unauthorized exactly like refresh; non-federated traffic is
/// still untouched.
#[test]
fn connect_grant_absence_is_unauthorized_in_strict_mode() {
    let err =
        optional_request_federated_service_grant(&headers(&[]), Some(&owner()), true).unwrap_err();
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);

    assert!(
        optional_request_federated_service_grant(&headers(&[]), None, true)
            .unwrap()
            .is_none()
    );
}
