use super::*;

#[test]
fn callback_hints_require_the_owned_remote_project_before_reading_the_body() {
    futures::executor::block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let state = AppState::new().await.unwrap();
            let owner = state
                .store
                .insert_user("owner", "hash", UserRole::Admin, 1)
                .await
                .unwrap();
            let other = state
                .store
                .insert_user("other", "hash", UserRole::User, 1)
                .await
                .unwrap();
            let project = state
                .store
                .create_project(
                    &NewProject {
                        name: "callback".into(),
                        mode,
                        path: Some("project".into()),
                    },
                    owner.id,
                    1,
                )
                .await
                .unwrap();
            let path = format!("/api/projects/{}/language-commands/reconnect", project.id);

            assert_eq!(
                reconnect(
                    request("", 8193),
                    &state,
                    &path,
                    AuthedUser {
                        id: other.id,
                        role: other.role
                    }
                )
                .await
                .unwrap_err()
                .into_response()
                .status(),
                404
            );
            let owner = AuthedUser {
                id: owner.id,
                role: owner.role,
            };
            assert_eq!(
                reconnect(request("", 8193), &state, &path, owner)
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                if mode == WorkspaceMode::Local {
                    400
                } else {
                    413
                }
            );
            for body in [
                json!({"project":project.id, "callback":"file:///private"}),
                json!({"project":project.id, "callback":"https://user:secret@example.test/api"}),
                json!({"project":project.id, "callback":"https://example.test/api?secret=1"}),
                json!({"project":project.id + 1, "callback":"https://example.test/api"}),
                json!({"user":0,"project":project.id, "callback":"https://example.test/api"}),
                json!({"project":project.id, "callback":"https://example.test/api", "authority":"forged"}),
            ] {
                let body = body.to_string();
                assert_eq!(
                    reconnect(request(&body, body.len()), &state, &path, owner)
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                    400
                );
            }
            assert!(
                state
                    .store
                    .get_setting("workspace_recovery_secret_v1")
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    });
}
