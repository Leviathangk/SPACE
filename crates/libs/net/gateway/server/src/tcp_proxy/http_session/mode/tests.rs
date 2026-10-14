use http::{Request, Version, header};
use http_body_util::Empty;

use super::*;

const ALLOWED_HOST: &str = "pypi.org";
const CO_TENANT_HOST: &str = "fiddle.fastly.dev";

#[test]
fn authority_binds_every_http_routing_field_to_the_tls_origin() {
    let host = "allowed.test".parse().unwrap();
    for (uri, headers, version, port, accepted) in [
        ("/", vec!["allowed.test"], Version::HTTP_11, 443, true),
        ("/", vec!["ALLOWED.test.:443"], Version::HTTP_11, 443, true),
        (
            "https://allowed.test/",
            vec!["allowed.test"],
            Version::HTTP_11,
            443,
            true,
        ),
        ("https://allowed.test/", vec![], Version::HTTP_2, 443, true),
        (
            "https://allowed.test:8443/",
            vec!["allowed.test:8443"],
            Version::HTTP_2,
            8443,
            true,
        ),
        ("/", vec!["forbidden.test"], Version::HTTP_11, 443, false),
        (
            "https://forbidden.test/",
            vec!["allowed.test"],
            Version::HTTP_11,
            443,
            false,
        ),
        (
            "https://allowed.test/",
            vec!["forbidden.test"],
            Version::HTTP_2,
            443,
            false,
        ),
        (
            "http://allowed.test/",
            vec!["allowed.test"],
            Version::HTTP_11,
            443,
            false,
        ),
        (
            "https://allowed.test:80/",
            vec![],
            Version::HTTP_2,
            443,
            false,
        ),
        ("/", vec!["allowed.test:8443"], Version::HTTP_11, 443, false),
        (
            "/",
            vec!["allowed.test:65536"],
            Version::HTTP_11,
            443,
            false,
        ),
        ("/", vec!["allowed.test:"], Version::HTTP_11, 443, false),
        ("/", vec!["user@allowed.test"], Version::HTTP_11, 443, false),
        (
            "/",
            vec!["allowed.test", "allowed.test"],
            Version::HTTP_11,
            443,
            false,
        ),
        ("/", vec![], Version::HTTP_11, 443, false),
        ("/", vec!["allowed.test"], Version::HTTP_2, 443, false),
        (
            "https://allowed.test/",
            vec![],
            Version::HTTP_11,
            443,
            false,
        ),
    ] {
        let mut request = Request::builder().uri(uri).version(version);
        for header in &headers {
            request = request.header(header::HOST, *header);
        }
        let request = request.body(()).unwrap();
        assert_eq!(
            authorize_authority(&request.try_into().unwrap(), &host, port).is_ok(),
            accepted,
            "{version:?} {uri} {headers:?} -> {port}"
        );
    }
}

#[test]
fn absolute_form_authority_outranks_an_allowed_host_header() {
    let request = Request::builder()
        .uri(format!("http://{CO_TENANT_HOST}/relay"))
        .header(header::HOST, ALLOWED_HOST)
        .body(Empty::<hyper::body::Bytes>::new())
        .unwrap();

    assert_eq!(
        request_authority(&request.try_into().unwrap())
            .unwrap()
            .host(),
        CO_TENANT_HOST,
        "proxy-form targets must not be authorized by a conflicting Host header"
    );
}

#[test]
fn duplicate_origin_form_hosts_are_invalid() {
    let request = Request::builder()
        .uri("/simple/")
        .header(header::HOST, ALLOWED_HOST)
        .header(header::HOST, CO_TENANT_HOST)
        .body(Empty::<hyper::body::Bytes>::new())
        .unwrap();

    assert!(
        request_authority(&request.try_into().unwrap()).is_err(),
        "authorization must not pick one Host while a CDN routes on another"
    );
}

#[test]
fn canonicalizes_the_validated_authority_and_forwards_origin_form() {
    let request = Request::builder()
        .uri("http://PYPI.ORG.:8080/simple/?q=host")
        .header(header::HOST, CO_TENANT_HOST)
        .body(Empty::<hyper::body::Bytes>::new())
        .unwrap();
    let mut request = SanitizedRequest::try_from(request).unwrap();
    let (_, authority) = validated_request_authority(&request).unwrap();

    normalize_upstream_routing(&mut request, &authority).unwrap();

    assert_eq!(
        request.headers().get(header::HOST).unwrap(),
        "pypi.org:8080",
        "forward the canonical DomainName plus the validated port"
    );
    assert_eq!(
        request.uri(),
        "/simple/?q=host",
        "a direct origin connection must receive origin-form, not an absolute URI"
    );
}
