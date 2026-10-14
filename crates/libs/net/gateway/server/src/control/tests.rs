use crate::egress::test_support::TestNetwork;
// Both existing readiness replies use these tags. A peer that only understands
// this pre-TLS contract must still distinguish ready from pending.
#[derive(Clone, PartialEq, prost::Message)]
struct LegacyReadiness {
    #[prost(int32, tag = "1")]
    state: i32,
    #[prost(string, tag = "2")]
    installed_policy_ref: String,
}

fn root_request<T>(message: T) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    request
        .extensions_mut()
        .insert(pplx_space_common_svc::PeerIdentity::new(
            0,
            0,
            Some(std::process::id().try_into().unwrap()),
        ));
    request
}

#[tokio::test]
async fn control_rpcs_preserve_readiness_and_report_installed_tls_policy() {
    use super::*;
    use crate::egress::PulledNetworkPolicy;
    use pplx_space_net_common::{EffectiveNetworkPolicyRefScope, NetworkPolicyV1, TlsInterception};
    use prost::Message;
    use std::sync::atomic::{AtomicBool, Ordering};

    let policy_ref = EffectiveNetworkPolicyRef::local(
        EffectiveNetworkPolicyRefScope::Sandbox,
        "sandbox-1",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    )
    .unwrap();
    for mode in [TlsInterception::Off, TlsInterception::On] {
        let ca_ready = Arc::new(AtomicBool::new(false));
        let state = TestNetwork::with_gate({
            let ca_ready = ca_ready.clone();
            Arc::new(move || ca_ready.load(Ordering::SeqCst))
        });
        let loader: PolicyLoader = Arc::new({
            let policy_ref = policy_ref.clone();
            move || {
                let policy_ref = policy_ref.clone();
                Box::pin(async move {
                    let mut policy = NetworkPolicyV1::allow_all();
                    policy.tls_interception = mode;
                    Ok(PulledNetworkPolicy {
                        policy,
                        policy_ref,
                        network_credentials: Default::default(),
                    })
                })
            }
        });
        let service = NetGatewayControlService::new(
            Arc::default(),
            state.control(),
            loader,
            GatewayDebugState::default(),
            ServicePortIngressState::default(),
            CancellationToken::new(),
        );
        let reported_mode = service
            .get_stats(root_request(GetStatsRequest {}))
            .await
            .unwrap()
            .into_inner()
            .tls_interception_mode;
        assert_eq!(
            pplx_space_net_common::proto::TlsInterception::try_from(reported_mode).unwrap(),
            pplx_space_net_common::proto::TlsInterception::Off
        );
        let pending = service
            .get_network_state(root_request(GetNetworkStateRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            LegacyReadiness::decode(pending.encode_to_vec().as_slice())
                .unwrap()
                .state,
            2
        );

        for installed in [false, true, false, true] {
            ca_ready.store(installed, Ordering::SeqCst);
            let refreshed = service
                .refresh_network_policy(root_request(RefreshNetworkPolicyRequest {
                    expected_policy_ref: policy_ref.to_string(),
                    reason: "start".to_owned(),
                }))
                .await
                .unwrap()
                .into_inner();
            let observed = service
                .get_network_state(root_request(GetNetworkStateRequest {}))
                .await
                .unwrap()
                .into_inner();
            let reported_mode = service
                .get_stats(root_request(GetStatsRequest {}))
                .await
                .unwrap()
                .into_inner()
                .tls_interception_mode;
            assert_eq!(
                pplx_space_net_common::proto::TlsInterception::try_from(reported_mode).unwrap(),
                match mode {
                    TlsInterception::Off => pplx_space_net_common::proto::TlsInterception::Off,
                    TlsInterception::On => pplx_space_net_common::proto::TlsInterception::On,
                }
            );
            // An installed policy is Ready on the legacy readiness
            // contract regardless of the CA gate. Old sandbox-managers that only
            // decode `state` must never see a CA outage as a failed boot.
            for bytes in [refreshed.encode_to_vec(), observed.encode_to_vec()] {
                let legacy = LegacyReadiness::decode(bytes.as_slice()).unwrap();
                assert_eq!(legacy.state, 3, "{mode:?}, CA installed={installed}");
                assert_eq!(legacy.installed_policy_ref, policy_ref.to_string());
            }
            assert!(observed.last_error.is_empty());
            // The gate is reported on the side, on every readiness reply.
            let expected_readiness = match (mode, installed) {
                (TlsInterception::Off, _) => crate::proto::InterceptionReadiness::NotRequired,
                (TlsInterception::On, true) => crate::proto::InterceptionReadiness::Ready,
                (TlsInterception::On, false) => {
                    crate::proto::InterceptionReadiness::DependenciesNotReady
                }
            };
            let stats = service
                .get_stats(root_request(GetStatsRequest {}))
                .await
                .unwrap()
                .into_inner();
            for (label, reported) in [
                ("refresh", refreshed.interception_readiness),
                ("get_network_state", observed.interception_readiness),
                ("get_stats", stats.interception_readiness),
            ] {
                assert_eq!(
                    crate::proto::InterceptionReadiness::try_from(reported).unwrap(),
                    expected_readiness,
                    "{label}: {mode:?}, CA installed={installed}"
                );
            }
        }

        let next_ref = EffectiveNetworkPolicyRef::local(
            EffectiveNetworkPolicyRefScope::Sandbox,
            "sandbox-1",
            "f".repeat(64),
        )
        .unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = CancellationToken::new();
        let refresh = tokio::spawn({
            let state = service.network_state.clone();
            let started = started.clone();
            let release = release.clone();
            async move {
                let expected = next_ref.clone();
                let loader: PolicyLoader = Arc::new(move || {
                    let started = started.clone();
                    let release = release.clone();
                    let policy_ref = next_ref.clone();
                    Box::pin(async move {
                        started.notify_one();
                        release.cancelled().await;
                        Ok(PulledNetworkPolicy {
                            policy: NetworkPolicyV1::deny_all(),
                            policy_ref,
                            network_credentials: Default::default(),
                        })
                    })
                });
                state
                    .refresh(loader, Some(expected), NetworkStateReason::ControlRefresh)
                    .await
            }
        });
        started.notified().await;
        for expected_state in [
            crate::proto::NetworkState::PendingPolicy,
            crate::proto::NetworkState::Draining,
        ] {
            if expected_state == crate::proto::NetworkState::Draining {
                service
                    .network_state
                    .set_draining(NetworkStateReason::Shutdown)
                    .await;
            }
            let snapshot = service.network_state.snapshot().await;
            assert_eq!(snapshot.mode, expected_state);
            let reported = service
                .get_network_policy(root_request(GetNetworkPolicyRequest {}))
                .await
                .unwrap()
                .into_inner();
            assert_eq!(reported.installed_policy_ref, policy_ref.to_string());
            let reported = NetworkPolicyV1::try_from(reported.policy.as_ref().unwrap()).unwrap();
            assert_eq!(reported.tls_interception, mode);
            assert_eq!(
                reported.default_action(),
                pplx_space_net_common::EgressDefaultAction::Allow
            );
        }
        release.cancel();
        assert_eq!(
            refresh.await.unwrap().mode,
            crate::proto::NetworkState::Draining
        );
    }
}
