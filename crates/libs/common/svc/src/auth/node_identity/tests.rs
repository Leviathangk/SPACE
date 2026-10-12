use super::*;

#[test]
fn live_identity_rejects_replaced_or_stopped_daemon_and_wrong_peer() {
    let identity = KubernetesPodIdentity {
        namespace: "space".into(),
        service_account: "worker".into(),
        pod_name: "worker-daemon".parse().unwrap(),
        pod_uid: "pod-a".parse().unwrap(),
        node_name: "worker-a".parse().unwrap(),
        node_uid: "node-a".parse().unwrap(),
    };
    let pod: k8s_openapi::api::core::v1::Pod = serde_json::from_value(serde_json::json!({
        "metadata": {"name": "worker-daemon", "uid": "pod-a"},
        "spec": {"nodeName": "worker-a", "serviceAccountName": "worker"},
        "status": {"phase": "Running", "podIPs": [{"ip": "192.0.2.1"}]}
    }))
    .unwrap();
    let peer = "192.0.2.1".parse().unwrap();
    verify_observed_pod(&identity, &pod, peer).unwrap();
    for change in 0..5 {
        let mut changed = pod.clone();
        let mut actual_peer = peer;
        match change {
            0 => changed.metadata.uid = Some("pod-b".into()),
            1 => changed.spec.as_mut().unwrap().node_name = Some("worker-b".into()),
            2 => changed.spec.as_mut().unwrap().service_account_name = Some("other".into()),
            3 => changed.status.as_mut().unwrap().phase = Some("Succeeded".into()),
            4 => actual_peer = "192.0.2.2".parse().unwrap(),
            _ => unreachable!(),
        }
        assert_eq!(
            verify_observed_pod(&identity, &changed, actual_peer)
                .unwrap_err()
                .code(),
            tonic::Code::PermissionDenied
        );
    }
}
