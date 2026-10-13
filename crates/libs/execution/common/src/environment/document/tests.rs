use super::*;

const E0_DOCUMENT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../proto/execution/v1/fixtures/e0.json"
));

#[test]
fn golden_document_reloads_with_the_same_identity_regardless_of_object_order() {
    let definition = EnvironmentDefinition::from_document(E0_DOCUMENT).unwrap();
    assert_eq!(
        definition.reference().to_string(),
        "sha256:3168c96b5719ce62543ae53e6641a4eb80416a9f01e5585f6f76c3c70111fa0b"
    );
    assert_eq!(
        EnvironmentDefinition::from_document(&definition.to_document().unwrap()).unwrap(),
        definition
    );
    // Value sorts object keys; serialization removes whitespace too. Neither
    // storage representation is the input to the environment digest.
    let sorted: serde_json::Value = serde_json::from_str(E0_DOCUMENT).unwrap();
    assert_eq!(
        EnvironmentDefinition::from_document(&sorted.to_string()).unwrap(),
        definition
    );
}

#[test]
fn dropped_nested_fields_and_duplicate_keys_are_not_accepted() {
    let base: serde_json::Value = serde_json::from_str(E0_DOCUMENT).unwrap();
    for pointer in [
        "",
        "/descriptor",
        "/descriptor/guest",
        "/descriptor/runtime/release/runtime",
        "/descriptor/boot/kernel",
        "/descriptor/compatibility/state_format",
    ] {
        let mut document = base.clone();
        document
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("future_semantics".to_owned(), true.into());
        assert!(
            EnvironmentDefinition::from_document(&document.to_string()).is_err(),
            "{pointer}"
        );
    }
    for key in [
        "schema_version",
        "reference",
        "host",
        "guest",
        "os",
        "architecture",
        "provider",
        "profile",
        "kernel",
        "state_format",
        "version",
        "digest",
    ] {
        // Duplicate every occurrence, with the same value: ambiguity is an
        // error even if a last-value-wins parser would produce the same tree.
        let source = serde_json::to_string(&base).unwrap();
        let needle = format!("\"{key}\":");
        let start = source.find(&needle).unwrap() + needle.len();
        let mut stream = serde_json::Deserializer::from_str(source.get(start..).unwrap())
            .into_iter::<serde_json::Value>();
        let value = stream.next().unwrap().unwrap();
        let duplicate = format!("\"{key}\":{value},");
        let mut text = source.clone();
        text.insert_str(start - needle.len(), &duplicate);
        assert!(
            EnvironmentDefinition::from_document(&text).is_err(),
            "{key}"
        );
    }
}

#[test]
fn unsupported_profiles_versions_and_changed_identities_fail_closed() {
    let base: serde_json::Value = serde_json::from_str(E0_DOCUMENT).unwrap();
    let cases: &[(&str, serde_json::Value)] = &[
        ("/schema_version", 2.into()),
        ("/descriptor/runtime/provider", "host_facility".into()),
        ("/descriptor/boot/profile", "firmware_disk_v1".into()),
        ("/descriptor/compatibility/profile", "future_v1".into()),
        ("/descriptor/isolation", "future_isolation".into()),
        (
            "/descriptor/runtime/release/runtime/version",
            "1.17.0".into(),
        ),
        ("/descriptor/runtime/release/runtime/kind", "fc".into()),
        ("/reference", format!("sha256:{}", "0".repeat(64)).into()),
    ];
    for (pointer, replacement) in cases {
        let mut document = base.clone();
        *document.pointer_mut(pointer).unwrap() = replacement.clone();
        assert!(
            EnvironmentDefinition::from_document(&document.to_string()).is_err(),
            "{pointer}"
        );
    }
}
