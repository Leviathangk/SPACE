use super::*;

#[rstest::rstest]
#[case(false)]
#[case(true)]
fn composition_preserves_policy_and_deny_precedence(#[case] exemptions_enabled: bool) {
    let destinations = serde_json::json!([
        {"ip":"192.0.2.10"}, {"ip":"192.0.2.11"}, {"ip":"169.254.169.254"},
        {"domain":"denied.example.test"}, {"domain":"sensor.example.test"},
        {"cidr":"2001:db8:abcd::/48"}, {"wildcard_domain":"*.agent.example.test"}
    ]);
    let egress = NetworkPolicyOverlay::from_config(
        &serde_json::json!({
            "egress_allowances": destinations,
            "tls_interception_exemptions": if exemptions_enabled {
                destinations.clone()
            } else { serde_json::json!([]) }
        })
        .to_string(),
    )
    .unwrap();

    for default_action in [EgressDefaultAction::Deny, EgressDefaultAction::Allow] {
        let base = NetworkPolicyV1 {
            version: Default::default(),
            tls_interception: pplx_space_net_common::TlsInterception::On,
            tls_interception_exemptions: Vec::new(),
            network_credential_rules: Vec::new(),
            egress: pplx_space_net_common::EgressPolicyV1::new(
                default_action,
                vec![EgressDestinationV1::Domain("user.example".parse().unwrap())],
                vec![
                    EgressDestinationV1::Ip("192.0.2.10".parse().unwrap()),
                    EgressDestinationV1::Domain("denied.example.test".parse().unwrap()),
                ],
            )
            .unwrap(),
        };

        let composed = egress.compose(&base);
        assert_eq!(
            serde_json::to_value(&composed)
                .unwrap()
                .get("tls_interception_exemptions")
                .is_some(),
            exemptions_enabled,
            "disabled composition must preserve the legacy serialized policy shape"
        );
        let expected_interception = if exemptions_enabled {
            pplx_space_net_common::TlsInterception::Off
        } else {
            pplx_space_net_common::TlsInterception::On
        };

        assert!(composed.allows_domain("user.example"));
        assert!(composed.allows_domain("sensor.example.test"));
        assert!(
            composed
                .egress_decision_for_ip("192.0.2.11".parse().unwrap())
                .is_allowed()
        );
        assert!(
            !composed
                .egress_decision_for_ip("192.0.2.10".parse().unwrap())
                .is_allowed()
        );
        assert!(
            !composed
                .egress_decision_for_ip("169.254.169.254".parse().unwrap())
                .is_allowed()
        );
        assert!(!composed.allows_domain("denied.example.test"));
        assert_eq!(composed.tls_interception, base.tls_interception);
        assert_eq!(
            composed.tls_interception_for("192.0.2.11".parse().unwrap(), None),
            expected_interception
        );
        assert_eq!(
            composed.tls_interception_for(
                "198.51.100.10".parse().unwrap(),
                Some(&"sensor.example.test".parse().unwrap())
            ),
            expected_interception
        );
        assert_eq!(
            composed.tls_interception_for(
                "198.51.100.10".parse().unwrap(),
                Some(&"user.example".parse().unwrap())
            ),
            pplx_space_net_common::TlsInterception::On
        );
        assert_eq!(
            composed.tls_interception_for("8.8.8.8".parse().unwrap(), None),
            pplx_space_net_common::TlsInterception::On
        );
        assert_eq!(composed.default_action(), default_action);
        assert_eq!(
            composed
                .egress_decision_for_ip("198.51.100.10".parse().unwrap())
                .is_allowed(),
            default_action == EgressDefaultAction::Allow
        );
        assert!(["8.8.8.8", "1.1.1.1"].iter().all(|resolver| {
            composed
                .egress_decision_for_ip(resolver.parse().unwrap())
                .is_allowed()
        }));
        assert_eq!(egress.compose(&composed), composed);
        let disabled = NetworkPolicyOverlay::from_config("{}").unwrap();
        assert_eq!(disabled.compose(&composed), composed);
        assert!(composed.allows_domain("child.agent.example.test"));
        let ipv6 = "2001:db8:abcd::1".parse().unwrap();
        assert!(composed.egress_decision_for_ip(ipv6).is_allowed());
        assert_eq!(
            composed.tls_interception_for(ipv6, None),
            expected_interception
        );
    }
}

#[rstest::rstest]
#[case("")]
#[case("[]")]
#[case("null")]
#[case(r#"{"egress_allowances":[{"ip":"not-an-ip"}]}"#)]
#[case(r#"{"tls_interception_exemptions":[{"domain":"not a domain"}]}"#)]
#[case(r#"{"unknown":[]}"#)]
fn rejects_invalid_configuration(#[case] raw: &str) {
    NetworkPolicyOverlay::from_config(raw).unwrap_err();
}

#[test]
fn create_fallback_preserves_the_requested_overlay() {
    let configured = NetworkPolicyOverlay::from_config(
        r#"{"egress_allowances":[{"domain":"sensor.example.test"}]}"#,
    )
    .unwrap();
    assert!(for_create(false, Some(&configured)).is_none());
    assert!(for_create(true, None).is_none());
    let selected = for_create(true, Some(&configured)).unwrap();
    assert!(std::ptr::eq(selected, &configured));
}

#[test]
fn exemptions_do_not_grant_egress_and_allowances_do_not_exempt_tls() {
    let overlay = NetworkPolicyOverlay::from_config(
        r#"{
        "egress_allowances":[{"domain":"allowed.example.test"}],
        "tls_interception_exemptions":[{"ip":"192.0.2.20"}]
    }"#,
    )
    .unwrap();
    let mut base = NetworkPolicyV1::deny_all();
    base.tls_interception = pplx_space_net_common::TlsInterception::On;
    let composed = overlay.compose(&base);
    assert!(
        !composed
            .egress_decision_for_ip("192.0.2.20".parse().unwrap())
            .is_allowed()
    );
    assert!(composed.allows_domain("allowed.example.test"));
    assert_eq!(
        composed.tls_interception_for(
            "192.0.2.21".parse().unwrap(),
            Some(&"allowed.example.test".parse().unwrap())
        ),
        pplx_space_net_common::TlsInterception::On
    );
}

#[test]
fn accepts_only_federated_overlay_opt_ins() {
    use axum::http::HeaderValue;

    let mut headers = HeaderMap::new();
    assert!(!network_policy_overlay_requested(&headers, true));
    for value in ["true", " TRUE "] {
        headers.insert(OVERLAY_HEADER, HeaderValue::from_static(value));
        assert!(network_policy_overlay_requested(&headers, true), "{value}");
        assert!(
            !network_policy_overlay_requested(&headers, false),
            "{value}"
        );
    }
    for value in ["", "false", "other"] {
        headers.insert(OVERLAY_HEADER, HeaderValue::from_static(value));
        assert!(!network_policy_overlay_requested(&headers, true), "{value}");
    }
    headers.insert(OVERLAY_HEADER, HeaderValue::from_bytes(&[0xff]).unwrap());
    assert!(!network_policy_overlay_requested(&headers, true));
}

#[test]
fn legacy_ecw_opt_in_remains_available_during_migration() {
    let mut headers = HeaderMap::new();
    headers.insert("x-space-falcon", " US2 ".parse().unwrap());
    assert!(ecw_network_policy_overlay_requested(&headers, true));
    assert!(!ecw_network_policy_overlay_requested(&headers, false));
    // The historical header never selected an overlay on native creates.
    assert!(!network_policy_overlay_requested(&headers, true));
    headers.insert(OVERLAY_HEADER, "false".parse().unwrap());
    assert!(!ecw_network_policy_overlay_requested(&headers, true));
    headers.insert(OVERLAY_HEADER, "true".parse().unwrap());
    assert!(ecw_network_policy_overlay_requested(&headers, true));
}

#[test]
fn legacy_configuration_preserves_policy_until_operator_migration() {
    fn load(
        generic: Option<&str>,
        legacy: Option<&str>,
        activation: Option<&str>,
    ) -> Result<Option<NetworkPolicyOverlay>, NetworkPolicyOverlayConfigError> {
        NetworkPolicyOverlay::from_environment(|name| {
            match name {
                ENV => generic,
                "SPACE_FALCON_US2_EGRESS_DESTINATIONS" => legacy,
                "SPACE_FALCON_TLS_INTERCEPTION_EXEMPTIONS_ENABLED" => activation,
                _ => unreachable!(),
            }
            .map(str::to_owned)
            .ok_or(std::env::VarError::NotPresent)
        })
    }
    let destinations = r#"[{"ip":"192.0.2.10"},{"domain":"agent.example.test"}]"#;
    let mut base = NetworkPolicyV1::deny_all();
    base.tls_interception = pplx_space_net_common::TlsInterception::On;
    for (activation, exempt) in [(None, false), (Some("false"), false), (Some("true"), true)] {
        let overlay = load(None, Some(destinations), activation).unwrap().unwrap();
        let policy = overlay.compose(&base);
        assert!(policy.allows_domain("agent.example.test"));
        assert!(
            policy
                .egress_decision_for_ip("192.0.2.10".parse().unwrap())
                .is_allowed()
        );
        assert_eq!(
            policy
                .tls_interception_for("192.0.2.10".parse().unwrap(), None)
                .is_off(),
            exempt
        );
    }
    assert!(load(None, None, None).unwrap().is_none());
    for raw in [
        "[]",
        r#"[{"cidr":"192.0.2.0/24"}]"#,
        r#"[{"wildcard_domain":"*.example.test"}]"#,
        "invalid",
    ] {
        load(None, Some(raw), None).unwrap_err();
    }
    load(None, Some(destinations), Some("invalid")).unwrap_err();
    // Generic configuration wins, including disablement; ignored legacy settings
    // must not prevent startup once the operator has migrated.
    let disabled = load(Some("{}"), Some("invalid"), Some("invalid"))
        .unwrap()
        .unwrap();
    assert_eq!(disabled.compose(&base), base);
    let generic = load(
        Some(r#"{"egress_allowances":[{"domain":"new.example.test"}]}"#),
        Some(destinations),
        Some("true"),
    )
    .unwrap()
    .unwrap()
    .compose(&base);
    assert!(generic.allows_domain("new.example.test"));
    assert!(!generic.allows_domain("agent.example.test"));
    assert!(generic.tls_interception_exemptions.is_empty());
    load(Some("invalid"), Some(destinations), None).unwrap_err();
}
