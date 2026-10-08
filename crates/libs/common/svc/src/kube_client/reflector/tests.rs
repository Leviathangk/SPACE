use super::*;

use axum::body::Body;
use k8s_openapi::api::core::v1::Pod;
use kube::client::ClientBuilder;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

#[tokio::test(start_paused = true)]
async fn synchronization_tracks_watch_failure_relist_and_shutdown() {
    let (requests_tx, mut requests) = mpsc::channel(1);
    let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
        let requests = requests_tx.clone();
        async move {
            let (response_tx, response_rx) = oneshot::channel();
            requests.send((request, response_tx)).await.unwrap();
            Ok::<_, std::io::Error>(response_rx.await.unwrap())
        }
    });
    let client = ClientBuilder::new(service, "default").build();
    let (store, writer) = reflector::store::<Pod>();
    let shutdown = CancellationToken::new();
    let mut health = spawn_reflector(
        "test pods",
        writer,
        Api::default_namespaced(client),
        watcher::Config::default(),
        shutdown.clone(),
    );

    let (request, response) = requests.recv().await.unwrap();
    assert!(!health.is_synced());
    assert!(
        !request
            .uri()
            .query()
            .unwrap_or_default()
            .contains("watch=true")
    );
    response.send(list("Running", "1", None)).unwrap();
    store.wait_until_ready().await.unwrap();
    assert!(health.is_synced());

    // A quiet watch and its normal reconnect do not invalidate the cache.
    let (_, response) = requests.recv().await.unwrap();
    let (events, stream) = mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(1);
    response
        .send(http::Response::new(Body::from_stream(
            tokio_stream::wrappers::ReceiverStream::new(stream),
        )))
        .unwrap();
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    assert!(health.is_synced());
    drop(events);
    let (request, response) = requests.recv().await.unwrap();
    assert!(request.uri().query().unwrap().contains("watch=true"));
    assert!(health.is_synced());
    response
        .send(
            http::Response::builder()
                .status(403)
                .body(Body::from(
                    json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
                "reason": "Forbidden", "message": "watch denied", "code": 403})
                    .to_string(),
                ))
                .unwrap(),
        )
        .unwrap();

    // The old object remains readable, but authorization must not trust it.
    let (request, response) = requests.recv().await.unwrap();
    assert!(
        !request
            .uri()
            .query()
            .unwrap_or_default()
            .contains("watch=true")
    );
    assert_eq!(
        store.state()[0].status.as_ref().unwrap().phase.as_deref(),
        Some("Running")
    );
    assert!(
        !health.is_synced(),
        "a failed watch must invalidate synchronization after initial readiness"
    );
    // A partial relist must not expose either the old or incomplete snapshot as synced.
    response
        .send(list("Failed", "2", Some("next-page")))
        .unwrap();
    let (request, response) = requests.recv().await.unwrap();
    assert!(
        request
            .uri()
            .query()
            .unwrap()
            .contains("continue=next-page")
    );
    assert!(!health.is_synced());
    response
        .send(http::Response::new(Body::from(
            json!({
                "apiVersion": "v1", "kind": "PodList",
                "metadata": {"resourceVersion": "2"}, "items": []
            })
            .to_string(),
        )))
        .unwrap();
    let (_, response) = requests.recv().await.unwrap();
    assert!(health.is_synced());
    assert_eq!(
        store.state()[0].status.as_ref().unwrap().phase.as_deref(),
        Some("Failed")
    );

    // Resource-version expiry also requires a complete replacement snapshot.
    response.send(http::Response::new(Body::from(json!({
        "type": "ERROR", "object": {"kind": "Status", "apiVersion": "v1",
            "status": "Failure", "reason": "Expired", "message": "resource version expired", "code": 410}
    }).to_string() + "\n"))).unwrap();
    let (_, response) = requests.recv().await.unwrap();
    assert!(!health.is_synced());
    response.send(list("Running", "3", None)).unwrap();
    let (_, _response) = requests.recv().await.unwrap();
    assert!(health.is_synced());
    assert_eq!(
        store.state()[0].status.as_ref().unwrap().phase.as_deref(),
        Some("Running")
    );

    shutdown.cancel();
    while health.synced.changed().await.is_ok() {}
    assert!(
        !health.is_synced(),
        "a stopped writer cannot leave a healthy cache"
    );
}

fn list(phase: &str, resource_version: &str, continuation: Option<&str>) -> http::Response<Body> {
    http::Response::new(Body::from(
        json!({
            "apiVersion": "v1", "kind": "PodList",
            "metadata": {"resourceVersion": resource_version, "continue": continuation},
            "items": [{
                "apiVersion": "v1", "kind": "Pod",
                "metadata": {"name": "caller", "namespace": "default", "uid": "pod-uid",
                    "resourceVersion": resource_version},
                "status": {"phase": phase}
            }]
        })
        .to_string(),
    ))
}
