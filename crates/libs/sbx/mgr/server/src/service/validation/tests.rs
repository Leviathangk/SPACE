use super::require_environment_acknowledgement;
use pplx_space_common_core::types::{Arch, EnvironmentReference, ExecutionMetadata};

#[test]
fn bound_execution_requires_positive_peer_acknowledgement() {
    let reference = |digit: &str| -> EnvironmentReference {
        format!("sha256:{}", digit.repeat(64)).parse().unwrap()
    };
    let legacy = ExecutionMetadata::initial_firecracker(Arch::X86_64, vec![]);
    let bound = legacy.clone().with_environment(reference("a"));
    let other = legacy.clone().with_environment(reference("b"));
    for (requested, resolved, accepted) in [
        (&legacy, &legacy, true),
        (&legacy, &bound, true),
        (&bound, &bound, true),
        (&bound, &legacy, false),
        (&bound, &other, false),
    ] {
        let result = require_environment_acknowledgement(requested, resolved);
        assert_eq!(result.is_ok(), accepted);
        if let Err(error) = result {
            assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        }
    }
}
