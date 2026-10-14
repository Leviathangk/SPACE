use super::*;

#[test]
fn deployment_admission_defaults_to_production() {
    use std::ffi::OsStr;
    assert!(deployment_is_production(None));
    for value in ["prod", "production", "", " ", "unknown", "dev-typo"] {
        assert!(deployment_is_production(Some(OsStr::new(value))), "{value}");
    }
    for value in [
        "dev",
        "development",
        "local",
        "kind-local",
        "test",
        "testing",
        "staging",
        " TESTING ",
    ] {
        assert!(
            !deployment_is_production(Some(OsStr::new(value))),
            "{value}"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        assert!(deployment_is_production(Some(OsStr::from_bytes(&[0xff]))));
    }
}

#[test]
fn startup_uses_deployment_identity_for_admission() {
    if !pplx_space_test_env::enter_test_process(&[
        ("SPACE_ENV", Some(std::ffi::OsStr::new("testing"))),
        ("DD_ENV", Some(std::ffi::OsStr::new("prod"))),
    ]) {
        return;
    }
    assert!(!ServiceSettings::from_env().production);
}
