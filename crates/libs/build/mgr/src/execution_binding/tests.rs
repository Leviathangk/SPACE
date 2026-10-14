use super::*;
use pplx_space_common_core::types::*;

fn fixture() -> ResolveExecutionEnvironmentResponse {
    let definition = EnvironmentDefinition::from_document(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../proto/execution/v1/fixtures/e0.json"
    )))
    .unwrap();
    let spec = definition.descriptor().spec();
    let RuntimeBinding::ManagedRelease(runtime) = &spec.runtime;
    let BootProfile::LinuxDirectV1 { kernel } = &spec.boot;
    let ExecutionCompatibilityProfile::FirecrackerSnapshotV1 { state_format } = &spec.compatibility;
    let execution = definition
        .bind_execution(
            ExecutionMetadata::new(
                runtime.runtime.clone(),
                spec.guest,
                Some(state_format.clone()),
                vec![ArtifactComponentMetadata {
                    kind: kernel.kind,
                    role: Some("boot".parse().unwrap()),
                    version: Some(kernel.version.clone()),
                    digest: Some(kernel.digest),
                    format: None,
                }],
            )
            .unwrap(),
        )
        .unwrap();
    ResolveExecutionEnvironmentResponse {
        definition: Some((&definition).into()),
        execution: Some((&execution).into()),
    }
}

#[test]
fn resolution_requires_complete_matching_support_evidence() {
    let binding = BuildExecutionBinding::from_response(None, fixture()).unwrap();
    BuildExecutionBinding::from_response(Some(binding.definition.reference()), fixture()).unwrap();
    assert!(matches!(
        BuildExecutionBinding::from_response(
            Some(EnvironmentReference::new(Digest::sha256([0; 32]))),
            fixture()
        ),
        Err(BuildExecutionBindingError::Reference)
    ));
    type Mutation = fn(&mut ResolveExecutionEnvironmentResponse);
    for mutate in [
        (|r: &mut ResolveExecutionEnvironmentResponse| r.definition = None) as Mutation,
        |r| r.execution = None,
        |r| r.execution.as_mut().unwrap().environment = None,
        |r| {
            r.execution
                .as_mut()
                .unwrap()
                .runtime
                .as_mut()
                .unwrap()
                .version = "1.17.0".into()
        },
        |r| r.execution.as_mut().unwrap().components.clear(),
    ] {
        let mut response = fixture();
        mutate(&mut response);
        BuildExecutionBinding::from_response(None, response).unwrap_err();
    }
}

#[test]
fn scratch_inputs_replace_inherited_selectors_and_reject_conflicting_recipe_pins() {
    let binding = BuildExecutionBinding::from_response(None, fixture()).unwrap();
    let mut command = tokio::process::Command::new("true");
    for key in [
        "SPACE_SEED_RUNTIME",
        "SPACE_SEED_RUNTIME_VERSION",
        "SPACE_SEED_STATE_FORMAT_VERSION",
        "SPACE_SEED_KERNEL_VERSION",
        "SPACE_SEED_KERNEL_DIGEST",
    ] {
        command.env(key, "untrusted-inherited-selector");
    }
    binding.configure_scratch(&mut command, None).unwrap();
    let env: std::collections::HashMap<_, _> = command.as_std().get_envs().collect();
    for (key, expected) in [
        ("SPACE_SEED_RUNTIME", "firecracker"),
        ("SPACE_SEED_RUNTIME_VERSION", "1.15.1"),
        ("SPACE_SEED_STATE_FORMAT_VERSION", "9.0.0"),
        ("SPACE_SEED_KERNEL_VERSION", "6.1.155"),
    ] {
        assert_eq!(
            env[std::ffi::OsStr::new(key)],
            Some(std::ffi::OsStr::new(expected))
        );
    }
    let BootProfile::LinuxDirectV1 { kernel } = &binding.definition.descriptor().spec().boot;
    let mut pin = super::super::GuestKernelPin {
        version: kernel.version.clone(),
        digest: kernel.digest,
    };
    binding.configure_scratch(&mut command, Some(&pin)).unwrap();
    pin.digest = Digest::sha256([0; 32]);
    assert!(matches!(
        binding.configure_scratch(&mut command, Some(&pin)),
        Err(BuildExecutionBindingError::KernelPin)
    ));
}

#[test]
fn seed_metadata_requires_evidence_and_becomes_a_versioned_bound_document() {
    let binding = BuildExecutionBinding::from_response(None, fixture()).unwrap();
    let mut metadata = TemplateMetadata::default();
    binding.bind_template(&mut metadata).unwrap_err();
    let mut wire = fixture().execution.unwrap();
    wire.environment = None;
    metadata.set_execution_metadata(ExecutionMetadata::try_from(&wire).unwrap());
    assert_eq!(
        metadata.schema_version,
        Some(TEMPLATE_METADATA_SCHEMA_VERSION)
    );
    binding.bind_template(&mut metadata).unwrap();
    assert_eq!(
        metadata.schema_version,
        Some(BOUND_TEMPLATE_METADATA_SCHEMA_VERSION)
    );
    let restored: TemplateMetadata =
        serde_json::from_str(&serde_json::to_string(&metadata).unwrap()).unwrap();
    assert_eq!(
        restored
            .execution_metadata()
            .unwrap()
            .unwrap()
            .environment(),
        Some(binding.definition.reference())
    );
}
