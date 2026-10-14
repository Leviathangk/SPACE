use super::*;

#[test]
fn experiment_configuration_rejects_typos_without_dropping_retention() {
    use std::ffi::OsStr;
    assert_eq!(
        ExperimentPolicy::from_setting(None).unwrap(),
        ExperimentPolicy::Disabled
    );
    for policy in [
        ExperimentPolicy::Disabled,
        ExperimentPolicy::Admit,
        ExperimentPolicy::Retain,
    ] {
        assert_eq!(
            ExperimentPolicy::from_setting(Some(OsStr::new(policy.as_str()))).unwrap(),
            policy
        );
    }
    for invalid in ["retain:fc-1.17-linux-6.18-typo", "enabled", " "] {
        ExperimentPolicy::from_setting(Some(OsStr::new(invalid))).unwrap_err();
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        ExperimentPolicy::from_setting(Some(OsStr::from_bytes(&[0xff]))).unwrap_err();
    }
}

#[test]
fn invalid_experiment_setting_refuses_startup() {
    if !pplx_space_test_env::enter_test_process(&[(
        "SPACE_EXECUTION_EXPERIMENTS",
        Some(std::ffi::OsStr::new("retain:fc-1.17-linux-6.18-typo")),
    )]) {
        return;
    }
    assert!(matches!(
        ServiceSettings::from_env(),
        Err(ExperimentPolicyError::Unknown(_))
    ));
}
