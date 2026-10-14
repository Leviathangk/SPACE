use pplx_space_execution_common::environment::canonical_descriptor;

use super::*;

#[test]
fn compiled_pins_produce_the_e0_golden_definition() {
    let initial = InitialEnvironment::for_architecture(Arch::X86_64).unwrap();
    let definition = initial.definition().unwrap();
    assert_eq!(
        canonical_descriptor(definition.descriptor()),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../../proto/execution/v1/fixtures/e0.canonical"
        ))
    );
    assert_eq!(
        definition.reference().to_string(),
        "sha256:3168c96b5719ce62543ae53e6641a4eb80416a9f01e5585f6f76c3c70111fa0b"
    );
    assert_eq!(
        initial.resolve_definition(definition.reference()).unwrap(),
        definition
    );
    for digest in [
        initial.release().digest(),
        initial.release().archive_sha256,
        initial.kernel().sha256,
    ] {
        assert!(matches!(
            initial.resolve_definition(EnvironmentReference::new(digest)),
            Err(ResolveEnvironmentError::UnknownReference(_))
        ));
    }
}

#[test]
fn valid_but_unqualified_candidates_cannot_resolve() {
    let initial = InitialEnvironment::for_architecture(Arch::X86_64).unwrap();
    let mut spec = initial.definition().unwrap().descriptor().spec().clone();
    let RuntimeBinding::ManagedRelease(release) = &mut spec.runtime;
    let catalog = crate::catalog::RuntimeCatalog::compiled().unwrap();
    let candidate = catalog
        .releases(RuntimeKind::Firecracker, Arch::X86_64)
        .find(|r| r.release.version.as_str() == "1.17.0")
        .unwrap();
    release.runtime = candidate.release.clone();
    release.digest = candidate.digest();
    let definition = EnvironmentDefinition::new(spec.try_into().unwrap());
    assert!(matches!(
        initial.resolve_definition(definition.reference()),
        Err(ResolveEnvironmentError::UnknownReference(_))
    ));
    // The old aarch64 pins still exist; they do not implicitly qualify a new
    // descriptor on an architecture outside this milestone's qualification.
    let arm = InitialEnvironment::for_architecture(Arch::Aarch64).unwrap();
    assert!(matches!(
        arm.definition(),
        Err(CompiledEnvironmentDefinitionError::UnsupportedArchitecture(
            Arch::Aarch64
        ))
    ));
}

#[test]
fn compiled_benchmark_pair_has_a_distinct_complete_identity() {
    let experiment = BenchmarkEnvironment::for_architecture(Arch::X86_64)
        .unwrap()
        .unwrap();
    let definition = experiment.definition().unwrap();
    let requirements = definition.execution_requirements().unwrap();
    assert_eq!(requirements.runtime().version.as_str(), "1.17.0");
    assert_eq!(
        requirements.state_format().unwrap().version.as_str(),
        "12.0.0"
    );
    assert_eq!(
        requirements.components()[0]
            .version
            .as_ref()
            .unwrap()
            .as_str(),
        "6.18.44"
    );
    assert_eq!(
        requirements.components()[0].digest,
        Some(experiment.kernel().sha256)
    );
    assert_ne!(
        definition.reference(),
        InitialEnvironment::for_architecture(Arch::X86_64)
            .unwrap()
            .definition()
            .unwrap()
            .reference()
    );
    assert!(
        BenchmarkEnvironment::for_architecture(Arch::Aarch64)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        definition.reference().to_string(),
        "sha256:959f1181ac69f0dde7125fbc81be6fcd75b64e9e06e7a393f067b96eb6fb2ac4"
    );
}
