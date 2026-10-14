use super::*;

#[test]
fn route_identity_generations_preserve_unsigned_peer_encoding_and_reject_out_of_domain_values() {
    use prost::Message;
    // The previous peer encoded field 3 as uint64; positive generations must
    // keep the same bytes while values outside the durable signed domain fail.
    #[derive(prost::Message)]
    struct UnsignedIdentity {
        #[prost(uint64, optional, tag = "3")]
        execution_generation: Option<u64>,
    }
    for value in [0, 1, i64::MAX as u64, (i64::MAX as u64) + 1, u64::MAX] {
        let bytes = UnsignedIdentity {
            execution_generation: Some(value),
        }
        .encode_to_vec();
        let wire = ProtoSpaceApiRouteIdentity::decode(bytes.as_slice()).unwrap();
        let parsed = route_identity_from_proto(Some(wire));
        if value > 0 && value <= i64::MAX as u64 {
            let identity = parsed.unwrap().unwrap();
            assert_eq!(
                identity.execution_generation.unwrap().get().unsigned_abs(),
                value
            );
            assert_eq!(
                route_identity_to_proto(&identity).unwrap().encode_to_vec(),
                bytes
            );
        } else {
            assert!(matches!(
                parsed,
                Err(RouteIdentityParseError::ExecutionGeneration)
            ));
        }
    }
}

fn test_activation_attempt_id() -> SandboxActivationAttemptId {
    "018f1e9d-7b7a-7d3a-8000-000000000001"
        .parse()
        .expect("activation attempt id parses")
}

fn route_identity_for(
    route_generation: u64,
    node_id: NodeId,
    execution_generation: Option<i64>,
    pod_uid: Option<PodUid>,
) -> SpaceApiRouteIdentity {
    SpaceApiRouteIdentity {
        sandbox_id: None,
        route_generation: Some(
            RouteGeneration::new(route_generation).expect("route generation parses"),
        ),
        current_node_id: Some(node_id),
        substrate_kind: None,
        execution_generation: execution_generation
            .map(|value| ExecutionGeneration::new(value).expect("execution generation parses")),
        pod_uid,
        activation_attempt_id: None,
    }
}

#[test]
fn service_port_ingress_identity_updates_are_monotonic() {
    let node_id = NodeId::new();
    let state = ExecutionIdentity::new(None);
    let identity = route_identity_for(7, node_id, Some(11), None);

    assert!(matches!(
        state.update_identity(identity.clone()),
        Ok(RouteIdentityUpdate::Updated)
    ));
    assert_eq!(state.identity(), Some(identity.clone()));
    assert!(matches!(
        state.update_identity(identity.clone()),
        Ok(RouteIdentityUpdate::Unchanged)
    ));

    let stale = route_identity_for(6, node_id, Some(11), None);
    let error = state
        .update_identity(stale)
        .expect_err("stale route generation rejects");
    assert!(matches!(
        error,
        RouteIdentityUpdateError::StaleRouteGeneration { .. }
    ));
    assert_eq!(state.identity(), Some(identity.clone()));

    let newer = route_identity_for(8, node_id, Some(12), None);
    assert!(matches!(
        state.update_identity(newer.clone()),
        Ok(RouteIdentityUpdate::Updated)
    ));
    assert_eq!(state.identity(), Some(newer));
}

#[test]
fn newer_service_port_identity_cancels_only_the_previous_stream_epoch() {
    let node_id = NodeId::new();
    let current = route_identity_for(7, node_id, Some(11), None);
    let state = ExecutionIdentity::new(Some(current.clone()));
    let old_stream = state.identity_with_epoch().unwrap().1;

    assert!(matches!(
        state.update_identity(current),
        Ok(RouteIdentityUpdate::Unchanged)
    ));
    assert!(!old_stream.is_cancelled());

    let replacement = route_identity_for(8, node_id, Some(12), None);
    assert!(matches!(
        state.update_identity(replacement.clone()),
        Ok(RouteIdentityUpdate::Updated)
    ));
    assert!(old_stream.is_cancelled());

    let new_stream = state.identity_with_epoch().unwrap().1;
    let conflict = route_identity_for(8, NodeId::new(), Some(12), None);
    assert!(matches!(
        state.update_identity(conflict),
        Err(RouteIdentityUpdateError::Conflict { .. })
    ));
    assert!(!new_stream.is_cancelled());
    assert_eq!(state.identity(), Some(replacement));
}

#[test]
fn service_port_ingress_identity_rejects_same_generation_conflicts() {
    let node_id = NodeId::new();
    let state = ExecutionIdentity::new(Some(route_identity_for(7, node_id, Some(11), None)));

    let conflicting_node = route_identity_for(7, NodeId::new(), Some(11), None);
    let error = state
        .update_identity(conflicting_node)
        .expect_err("same-generation node conflict rejects");
    assert!(matches!(error, RouteIdentityUpdateError::Conflict { .. }));

    let missing_execution = route_identity_for(7, node_id, None, None);
    let error = state
        .update_identity(missing_execution)
        .expect_err("same-generation identity cannot drop execution generation");
    assert!(matches!(error, RouteIdentityUpdateError::Conflict { .. }));
}

#[test]
fn service_port_ingress_identity_allows_same_generation_enrichment() {
    let node_id = NodeId::new();
    let pod_uid = PodUid::parse("pod-uid").expect("pod uid parses");
    let state = ExecutionIdentity::new(Some(route_identity_for(7, node_id, None, None)));
    let incomplete_identity_stream = state.identity_with_epoch().unwrap().1;
    let enriched = route_identity_for(7, node_id, Some(11), Some(pod_uid));

    assert!(matches!(
        state.update_identity(enriched.clone()),
        Ok(RouteIdentityUpdate::Updated)
    ));
    assert!(
        incomplete_identity_stream.is_cancelled(),
        "a stream authorized against incomplete identity must not survive enrichment"
    );
    assert_eq!(state.identity(), Some(enriched));
}

fn rotated_activation_attempt_id() -> SandboxActivationAttemptId {
    "018f1e9d-7b7a-7d3a-8000-000000000002"
        .parse()
        .expect("activation attempt id parses")
}

#[test]
fn service_port_ingress_identity_allows_same_generation_attempt_rotation() {
    let node_id = NodeId::new();
    let mut current = route_identity_for(7, node_id, Some(11), None);
    current.activation_attempt_id = Some(test_activation_attempt_id());
    let state = ExecutionIdentity::new(Some(current.clone()));
    let live_stream = state.identity_with_epoch().unwrap().1;

    let mut rotated = current.clone();
    rotated.activation_attempt_id = Some(rotated_activation_attempt_id());
    assert!(matches!(
        state.update_identity(rotated.clone()),
        Ok(RouteIdentityUpdate::Updated)
    ));
    assert_eq!(state.identity(), Some(rotated));
    assert!(
        !live_stream.is_cancelled(),
        "an attempt rotation stays within one execution; established streams remain valid"
    );
}

#[test]
fn same_generation_attempt_rotation_does_not_relax_other_dimensions() {
    let mut current = route_identity_for(7, NodeId::new(), Some(11), None);
    current.activation_attempt_id = Some(test_activation_attempt_id());
    let state = ExecutionIdentity::new(Some(current.clone()));

    let mut moved = route_identity_for(7, NodeId::new(), Some(11), None);
    moved.activation_attempt_id = Some(rotated_activation_attempt_id());
    let error = state
        .update_identity(moved)
        .expect_err("a rotated attempt cannot smuggle a node move");
    assert!(matches!(error, RouteIdentityUpdateError::Conflict { .. }));
    assert_eq!(state.identity(), Some(current));
}

#[test]
fn same_generation_identity_cannot_drop_activation_attempt() {
    let mut current = route_identity_for(7, NodeId::new(), Some(11), None);
    current.activation_attempt_id = Some(test_activation_attempt_id());
    let state = ExecutionIdentity::new(Some(current.clone()));

    let mut dropped = current.clone();
    dropped.activation_attempt_id = None;
    let error = state
        .update_identity(dropped)
        .expect_err("same-generation identity cannot drop the activation attempt");
    assert!(matches!(error, RouteIdentityUpdateError::Conflict { .. }));
    assert_eq!(state.identity(), Some(current));
}

#[test]
fn service_port_ingress_identity_update_requires_route_generation() {
    let state = ExecutionIdentity::new(None);
    let error = state
        .update_identity(SpaceApiRouteIdentity {
            sandbox_id: None,
            route_generation: None,
            current_node_id: Some(NodeId::new()),
            substrate_kind: None,
            execution_generation: None,
            pod_uid: None,
            activation_attempt_id: None,
        })
        .expect_err("missing route generation rejects");
    assert!(matches!(
        error,
        RouteIdentityUpdateError::MissingRouteGeneration
    ));
}
