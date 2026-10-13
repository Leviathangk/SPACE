use axum::http::{HeaderMap, HeaderValue};

use super::{api_key, bearer_token};

#[test]
fn extracts_bearer_token_from_authorization_header() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer session-token"),
    );

    assert_eq!(bearer_token(&headers), Some("session-token"));
}

#[test]
fn rejects_non_bearer_authorization_header() {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", HeaderValue::from_static("Basic abc123"));

    assert_eq!(bearer_token(&headers), None);
}

#[test]
fn extracts_api_key_from_x_api_key_header() {
    let mut headers = HeaderMap::new();
    headers.insert("x-api-key", HeaderValue::from_static("e2b_test_key"));

    assert_eq!(api_key(&headers), Some("e2b_test_key"));
}

#[test]
fn rejects_empty_api_key_header() {
    let mut headers = HeaderMap::new();
    headers.insert("x-api-key", HeaderValue::from_static(""));

    assert_eq!(api_key(&headers), None);
}

#[test]
fn a_remembered_service_credential_expires_unless_seen_again() {
    let known = super::KnownServiceCredentials::default();
    let start = std::time::Instant::now();
    known.remember_at("hash".to_string(), start);
    let ttl = super::KNOWN_SERVICE_CREDENTIAL_TTL;
    assert!(known.contains_at("hash", start + ttl / 2));
    assert!(!known.contains_at("hash", start + ttl), "expired");
    assert!(!known.contains_at("other", start));

    known.remember_at("hash".to_string(), start + ttl);
    assert!(known.contains_at("hash", start + ttl + ttl / 2), "renewed");
    known.forget("hash");
    assert!(!known.contains_at("hash", start + ttl));
}

#[test]
fn remembered_service_credentials_are_bounded() {
    let known = super::KnownServiceCredentials::default();
    let start = std::time::Instant::now();
    for index in 0..super::KNOWN_SERVICE_CREDENTIALS_MAX {
        known.remember_at(format!("hash-{index}"), start);
    }
    known.remember_at("one-too-many".to_string(), start);
    assert!(!known.contains_at("one-too-many", start), "the set is full");

    // Expired entries make room.
    let later = start + super::KNOWN_SERVICE_CREDENTIAL_TTL;
    known.remember_at("one-too-many".to_string(), later);
    assert!(known.contains_at("one-too-many", later));
}
