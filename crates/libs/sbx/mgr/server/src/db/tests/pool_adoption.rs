use super::*;
use crate::db::pool_adoption::*;
use pplx_space_common_core::types::{OrgId, ROOT_SANDBOX_OWNER};

#[test]
fn failed_pool_registration_rolls_back_create_and_cleanup_intent() {
    let conn = test_conn();
    conn.execute_batch("CREATE TRIGGER refuse_pool_member BEFORE INSERT ON sandbox_pool_adoptions BEGIN SELECT RAISE(ABORT, 'injected registration failure'); END;").unwrap();
    let id = SandboxId::new().to_string();
    let params = TestSandboxInsert::new(&id, "atomic01", SandboxState::Creating);

    let result = insert_sandbox_with_cleanup_episode_and_route_evidence(
        &conn,
        params.insert_params(
            &SandboxRuntimeLabel::parse(params.uuid8.clone()).unwrap(),
            &ROOT_SANDBOX_OWNER,
        ),
        CleanupEpisodeSpec {
            id: SandboxCleanupEpisodeId::new(),
            completion: CleanupCompletion::DeleteRow,
            keep_filesystem: false,
            entered_from_state: None,
            activation_state: SandboxState::Creating,
            execution_generation: None,
            activation_attempt_id: None,
        },
        None,
        None,
        true,
    );

    assert!(result.is_err());
    assert!(get_sandbox(&conn, &id).unwrap().is_none());
    assert!(!pending(&conn, &id).unwrap());
}

#[test]
fn encrypted_pool_claim_is_fenced_until_completed_and_replays_only_for_its_owner() {
    for volume_encryption in [false, true] {
        let conn = test_conn();
        let id = SandboxId::new().to_string();
        insert_test_sandbox(
            &conn,
            TestSandboxInsert {
                user: ROOT_SANDBOX_OWNER.to_string(),
                ..TestSandboxInsert::new(&id, "encpool1", SandboxState::Paused)
            },
        );
        let claim = PoolClaim {
            id: uuid::Uuid::now_v7(),
            org_id: OrgId::new(),
            volume_encryption,
        };
        let owner = adopter_owner();
        let env = BTreeMap::from([("EXAMPLE".to_string(), "adopted".to_string())]);

        assert!(!begin(&conn, &id, claim, &owner, &env).unwrap());
        assert!(!pending(&conn, &id).unwrap());
        register(&conn, &id).unwrap();
        assert!(pending(&conn, &id).unwrap());
        assert!(begin(&conn, &id, claim, &owner, &env).unwrap());

        assert!(
            !begin(
                &conn,
                &id,
                PoolClaim {
                    volume_encryption: !claim.volume_encryption,
                    ..claim
                },
                &owner,
                &env
            )
            .unwrap()
        );
        assert!(incomplete(&conn, &id).unwrap());
        assert!(pending(&conn, &id).unwrap());
        assert!(!get_sandbox(&conn, &id).unwrap().unwrap().volume_encryption);
        assert!(
            !begin(
                &conn,
                &id,
                PoolClaim {
                    id: uuid::Uuid::now_v7(),
                    ..claim
                },
                &owner,
                &env
            )
            .unwrap()
        );
        assert!(
            !begin(
                &conn,
                &id,
                PoolClaim {
                    org_id: OrgId::new(),
                    ..claim
                },
                &owner,
                &env
            )
            .unwrap()
        );
        assert!(!begin(&conn, &id, claim, &owner, &BTreeMap::new()).unwrap());
        assert!(
            !complete(
                &conn,
                &id,
                PoolClaim {
                    id: uuid::Uuid::now_v7(),
                    ..claim
                }
            )
            .unwrap()
        );
        assert!(begin(&conn, &id, claim, &owner, &env).unwrap());
        assert!(complete(&conn, &id, claim).unwrap());
        assert!(complete(&conn, &id, claim).unwrap());
        assert!(!incomplete(&conn, &id).unwrap());
        assert!(!pending(&conn, &id).unwrap());
        let adopted = get_sandbox(&conn, &id).unwrap().unwrap();
        assert_eq!(adopted.volume_encryption, volume_encryption);
        assert_eq!(adopted.user, owner);
        assert_eq!(adopted.process_env, env);
        assert_eq!(adopted.state, SandboxState::Paused);
    }
}

#[test]
fn encrypted_pool_claim_cannot_complete_while_the_member_is_running() {
    let conn = test_conn();
    let id = SandboxId::new().to_string();
    insert_test_sandbox(
        &conn,
        TestSandboxInsert {
            user: ROOT_SANDBOX_OWNER.to_string(),
            ..TestSandboxInsert::new(&id, "encpool2", SandboxState::Started)
        },
    );
    register(&conn, &id).unwrap();
    let claim = PoolClaim {
        id: uuid::Uuid::now_v7(),
        org_id: OrgId::new(),
        volume_encryption: true,
    };

    assert!(begin(&conn, &id, claim, &adopter_owner(), &BTreeMap::new()).unwrap());
    assert!(!complete(&conn, &id, claim).unwrap());

    assert!(incomplete(&conn, &id).unwrap());
    assert!(!get_sandbox(&conn, &id).unwrap().unwrap().volume_encryption);
}

#[test]
fn legacy_owner_swap_completes_only_an_unclaimed_pool_member() {
    for claimed in [false, true] {
        let conn = test_conn();
        let id = SandboxId::new().to_string();
        insert_test_sandbox(
            &conn,
            TestSandboxInsert {
                user: ROOT_SANDBOX_OWNER.to_string(),
                ..TestSandboxInsert::new(&id, "legacy1", SandboxState::Started)
            },
        );
        register(&conn, &id).unwrap();
        let owner = adopter_owner();
        if claimed {
            let claim = PoolClaim {
                id: uuid::Uuid::now_v7(),
                org_id: OrgId::new(),
                volume_encryption: true,
            };
            assert!(begin(&conn, &id, claim, &owner, &BTreeMap::new()).unwrap());
        }

        assert!(
            set_sandbox_owner_if_current(&conn, &id, &ROOT_SANDBOX_OWNER, &owner)
                .unwrap()
                .is_some()
        );

        assert_eq!(pending(&conn, &id).unwrap(), claimed);
        assert_eq!(incomplete(&conn, &id).unwrap(), claimed);
    }
}

#[test]
fn deleting_a_pool_member_removes_its_adoption_receipt() {
    let conn = test_conn();
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    let id = SandboxId::new().to_string();
    insert_test_sandbox(
        &conn,
        TestSandboxInsert::new(&id, "delete1", SandboxState::Stopped),
    );
    register(&conn, &id).unwrap();

    delete_sandbox(&conn, &id).unwrap();

    assert!(!pending(&conn, &id).unwrap());
}
