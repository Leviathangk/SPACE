use super::*;

#[test]
fn network_policy_loader_requires_v1_and_a_valid_reference() {
    let v1 = NetworkPolicyV1::deny_all();
    let v1_proto = NetworkPolicyProto::from(&v1);

    assert_eq!(
        network_policy_from_host_control_proto(Some(&v1_proto)).unwrap(),
        v1.clone(),
        "the V1 host-control field hydrates the gateway"
    );

    assert!(matches!(
        network_policy_from_host_control_proto(None),
        Err(NetworkPolicyFromHostControlError::MissingV1Policy)
    ));

    for (reference, expected) in [
        ("", EffectiveNetworkPolicyRefParseError::Empty),
        (
            "invalid",
            EffectiveNetworkPolicyRefParseError::InvalidFormat,
        ),
    ] {
        let error = InitialNetworkState {
            policy: v1.clone(),
            network_credentials: Default::default(),
            policy_ref: reference.to_owned(),
            network_tx_bytes: 0,
            network_rx_bytes: 0,
        }
        .into_policy()
        .unwrap_err();
        assert_eq!(
            std::error::Error::source(&error)
                .unwrap()
                .downcast_ref::<EffectiveNetworkPolicyRefParseError>(),
            Some(&expected),
        );
        assert!(matches!(
            error,
            HydrateNetworkStateError::InvalidReference { reference: raw, source }
                if raw == reference && source == expected
        ));
    }
}
