use super::*;

fn declaration() -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1, "command": ["/bin/sh", "-lc", "exec app"],
        "user": "root", "working_directory": "/", "env": {"TOKEN": "secret"},
        "readiness": {"type": "exec", "command": ["/bin/true"]},
        "startup_timeout_secs": 30
    })
}

#[test]
fn round_trip_preserves_intent_without_exposing_command_or_environment() {
    let runtime: TemplateRuntime = serde_json::from_value(declaration()).unwrap();
    assert_eq!(
        TemplateRuntime::from_bytes(&runtime.canonical_bytes().unwrap()).unwrap(),
        runtime
    );
    let debug = format!("{runtime:?}");
    assert!(!debug.contains("secret"));
    assert!(!debug.contains("exec app"));
}

#[test]
fn rejects_ambiguous_or_unusable_declarations() {
    for (field, value) in [
        ("schema_version", serde_json::json!(2)),
        ("command", serde_json::json!([])),
        ("command", serde_json::json!(["a\u{0}b"])),
        ("working_directory", serde_json::json!("relative")),
        ("startup_timeout_secs", serde_json::json!(0)),
        ("startup_timeout_secs", serde_json::json!(61)),
        ("env", serde_json::json!({"BAD-KEY": "value"})),
        (
            "readiness",
            serde_json::json!({"type": "exec", "command": []}),
        ),
        ("unexpected", serde_json::json!(true)),
    ] {
        let mut input = declaration();
        input[field] = value;
        assert!(
            serde_json::from_value::<TemplateRuntime>(input).is_err(),
            "{field}"
        );
    }
}
