use pplx_space_common_svc::bearer_token::{
    SPACE_INTERNAL_CONTROL_TOKEN_ENV, SPACE_INTERNAL_TOKEN_ENV,
    SPACE_NODE_API_INTERNAL_TOKEN_PATH_ENV, SPACE_NODE_API_TOKEN_PATH_ENV,
};

/// Enter before awaiting fixtures: authenticated clients need process credentials.
pub(crate) fn enter_sim_worker_process() -> bool {
    pplx_space_test_env::enter_test_process(&[
        (
            "SPACE_INTERNAL_RPC_INSECURE_DEV",
            Some(std::ffi::OsStr::new("true")),
        ),
        (
            SPACE_INTERNAL_TOKEN_ENV,
            Some(std::ffi::OsStr::new("sim-worker-test-token")),
        ),
        (
            SPACE_INTERNAL_CONTROL_TOKEN_ENV,
            Some(std::ffi::OsStr::new("sim-worker-test-control-token")),
        ),
        (SPACE_NODE_API_TOKEN_PATH_ENV, None),
        (SPACE_NODE_API_INTERNAL_TOKEN_PATH_ENV, None),
    ])
}

/// [`enter_sim_worker_process`], but the node-api token paths name files this
/// test owns instead of being unset. Unset, the token reader falls back to the
/// host's fixed `/var/run/space-auth/*.token` paths and reads them when they
/// exist; these files carry the same tokens the fallback variables do.
///
/// The parent creates the files and keeps them until its child exits; the
/// child's own copy of the directory is unused.
pub(crate) fn enter_sim_worker_process_with_owned_token_files() -> bool {
    let tokens = tempfile::tempdir().expect("create test token directory");
    let node_api = tokens.path().join("node-api.token");
    let node_api_internal = tokens.path().join("node-api-internal.token");
    std::fs::write(&node_api, "sim-worker-test-token").expect("write node-api token");
    std::fs::write(&node_api_internal, "sim-worker-test-control-token")
        .expect("write node-api internal token");
    pplx_space_test_env::enter_test_process(&[
        (
            "SPACE_INTERNAL_RPC_INSECURE_DEV",
            Some(std::ffi::OsStr::new("true")),
        ),
        (
            SPACE_INTERNAL_TOKEN_ENV,
            Some(std::ffi::OsStr::new("sim-worker-test-token")),
        ),
        (
            SPACE_INTERNAL_CONTROL_TOKEN_ENV,
            Some(std::ffi::OsStr::new("sim-worker-test-control-token")),
        ),
        (SPACE_NODE_API_TOKEN_PATH_ENV, Some(node_api.as_os_str())),
        (
            SPACE_NODE_API_INTERNAL_TOKEN_PATH_ENV,
            Some(node_api_internal.as_os_str()),
        ),
    ])
}
