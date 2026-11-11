//! Authenticated journal transport, shared by both workspace adapters.
use super::*;
use openwebide_core::workspace_transaction::{
    AdvanceWorkspaceTransaction, BeginWorkspaceTransaction, JOURNAL_API_BODY_LIMIT, valid_id,
};

fn address(path: &str) -> Result<(i64, Option<&str>), ApiError> {
    let (prefix, suffix) = path
        .split_once("/workspace-transactions")
        .ok_or_else(|| ApiError::bad_request("Invalid workspace transaction path"))?;
    let project = path_id(prefix, "/api/projects")?;
    let id = match suffix {
        "" => None,
        value => Some(
            value
                .strip_prefix('/')
                .filter(|id| valid_id(id))
                .ok_or_else(|| ApiError::bad_request("Invalid workspace transaction identity"))?,
        ),
    };
    Ok((project, id))
}

pub(crate) async fn activity(
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let prefix = path
        .strip_suffix("/workspace-activity")
        .ok_or_else(|| ApiError::bad_request("Invalid workspace activity path"))?;
    let project = path_id(prefix, "/api/projects")?;
    let mut response = json_response(
        200,
        &state.store.workspace_activity(user.id, project).await?,
    );
    response.headers_mut().insert(
        "cache-control",
        spin_sdk::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

pub(crate) async fn get(
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let (project, id) = address(path)?;
    Ok(json_response(
        200,
        &state
            .store
            .workspace_transaction(user.id, project, id)
            .await?,
    ))
}

pub(crate) async fn begin<B>(
    req: Request<B>,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let (project, id) = address(path)?;
    if id.is_some() {
        return Err(ApiError::bad_request(
            "Unexpected workspace transaction identity",
        ));
    }
    state.store.get_project(project, user.id).await?;
    let request: BeginWorkspaceTransaction =
        parse_json(read_body(req, JOURNAL_API_BODY_LIMIT).await?)?;
    // Fresh language evidence is consumed only by the shared native resource
    // worker. Legacy byte journals retain their existing recovery path.
    if request.language_review.is_some() && request.plan.version != 2 {
        return Err(ApiError::bad_request(
            "Language reviews require a native resource transaction",
        ));
    }
    Ok(json_response(
        200,
        &state
            .store
            .begin_workspace_transaction(user.id, project, &request, now())
            .await?,
    ))
}

pub(crate) async fn delegate<B>(
    req: Request<B>,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let (project, id) = address(
        path.strip_suffix("/delegate")
            .ok_or_else(|| ApiError::bad_request("Invalid workspace delegation path"))?,
    )?;
    let id = id.ok_or_else(|| ApiError::bad_request("A workspace delegation needs its journal"))?;
    state.store.get_project(project, user.id).await?;
    let input: openwebide_core::workspace_transaction::DelegateWorkspaceTransaction =
        parse_json(read_body(req, 64 * 1024).await?)?;
    let grant = state
        .store
        .delegate_workspace_transaction(user.id, project, id, &input, now())
        .await?;
    let mut response = json_response(200, &grant);
    response.headers_mut().insert(
        "cache-control",
        spin_sdk::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

pub(crate) async fn delegated<B>(req: Request<B>, state: &AppState) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if ["origin", "sec-fetch-site", "cookie"]
        .iter()
        .any(|header| req.headers().contains_key(*header))
    {
        return Err(ApiError::new(
            403,
            "Paired workspace callbacks require the native worker transport",
        ));
    }
    let input: openwebide_core::workspace_transaction::DelegatedWorkspaceJournalRequest =
        parse_json(read_body(req, 64 * 1024).await?)?;
    let value = state
        .store
        .delegated_workspace_journal(&input, now())
        .await?;
    let mut response = json_response(200, &value);
    response.headers_mut().insert(
        "cache-control",
        spin_sdk::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

pub(crate) async fn advance<B>(
    req: Request<B>,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let (project, id) = address(path)?;
    let owned = state.store.get_project(project, user.id).await?;
    // Cursor updates never contain snapshots or editor buffers.
    let request: AdvanceWorkspaceTransaction = parse_json(read_body(req, 64 * 1024).await?)?;
    if id != Some(request.id.as_str()) {
        return Err(ApiError::bad_request(
            "Workspace transaction identity changed",
        ));
    }
    if owned.mode != WorkspaceMode::Local
        && request.progress
            != openwebide_core::workspace_transaction::TransactionProgress::prepared_cancellation()
    {
        return Err(ApiError::new(
            409,
            "The native worker owns this operation; use its execution endpoint",
        ));
    }
    Ok(json_response(
        200,
        &state
            .store
            .advance_workspace_transaction(user.id, project, &request, now())
            .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_addresses_are_exact() {
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            address("/api/projects/7/workspace-transactions").unwrap(),
            (7, None)
        );
        assert_eq!(
            address(&format!("/api/projects/7/workspace-transactions/{id}")).unwrap(),
            (7, Some(id))
        );
        for suffix in ["/", "/../x", "/abc", "/0123456789ABCDEF0123456789abcdef"] {
            assert!(address(&format!("/api/projects/7/workspace-transactions{suffix}")).is_err());
        }
    }
}

#[cfg(test)]
mod delegation_tests;

#[cfg(test)]
mod api_contract {
    use super::*;
    use openwebide_core::{
        NewProject, UserRole, WorkspaceMode,
        editor::EditorRecoveryRoot,
        workspace_transaction::{
            EditorRecoveryPatch, EntryState, FileOperation, TransactionPhase, TransactionProgress,
            WorkspacePlan, WorkspaceTransactionAdvanced, WorkspaceTransactionPrepared,
            WorkspaceTransactionState,
        },
    };
    fn request(body: Vec<u8>, advertised_size: Option<usize>) -> Request<FullBody<Bytes>> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("http://localhost/api");
        if let Some(size) = advertised_size {
            builder = builder.header("content-length", size.to_string());
        }
        builder.body(FullBody::new(Bytes::from(body))).unwrap()
    }
    fn retained_review(
        input: &BeginWorkspaceTransaction,
    ) -> openwebide_core::workspace_transaction::LanguageReview {
        let mut prepared = openwebide_core::plugins::testing::receipt();
        prepared.manifest.compatibility.plugin_api = 9;
        prepared.manifest.executable = None;
        prepared.manifest.contributions.tools.clear();
        prepared.manifest.contributions.skills.clear();
        prepared.manifest.contributions.languages = serde_json::from_value(json!([{
            "id":"custom","name":"Custom","extensions":["custom"],
            "features":{"languageServer":{"command":"custom-lsp"}}
        }]))
        .unwrap();
        let result = json!({"changes":[{"kind":"file","operation":input.plan.operations[0]}]});
        openwebide_core::workspace_transaction::LanguageReview {
            prepared,
            binding_revision: 1,
            input: openwebide_core::plugins::languages::intelligence::Request {
                version: openwebide_core::plugins::languages::intelligence::VERSION,
                client: "client".into(),
                document: "document".into(),
                request: 1,
                revision: 1,
                language: "custom".into(),
                path: "original.custom".into(),
                source: String::new(),
                source_digest: openwebide_core::plugins::languages::source_digest(""),
                query: openwebide_core::plugins::languages::intelligence::Query::Rename {
                    position: Default::default(),
                    new_name: "new".into(),
                },
            },
            context: openwebide_core::plugins::languages::intelligence::AnalysisContext {
                identities: vec![],
                revisions: vec![],
                files: vec![],
                server: "original".into(),
                proof: "opaque.signature".into(),
                result_digest: openwebide_core::plugins::content_digest(
                    &serde_json::to_vec(&result).unwrap(),
                ),
            },
            result,
            transaction_digest:
                openwebide_core::workspace_transaction::LanguageReview::transaction_digest(
                    &input.plan,
                    &input.editor,
                )
                .unwrap(),
        }
    }
    #[test]
    fn native_language_review_begins_roundtrip_without_granting_worker_authority_in_both_modes() {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let state = AppState::new().await.unwrap();
                let owner = state
                    .store
                    .insert_user("owner", "hash", UserRole::Admin, 1)
                    .await
                    .unwrap();
                let project = state
                    .store
                    .create_project(
                        &NewProject {
                            name: "reviewed".into(),
                            mode,
                            path: Some("root".into()),
                        },
                        owner.id,
                        1,
                    )
                    .await
                    .unwrap();
                let user = AuthedUser {
                    id: owner.id,
                    role: owner.role,
                };
                let path = format!("/api/projects/{}/workspace-transactions", project.id);
                let mut input = BeginWorkspaceTransaction {
                    id: "a".repeat(32),
                    language_review: None,
                    plan: WorkspacePlan {
                        version: 2,
                        root: "b".repeat(64),
                        operations: vec![FileOperation::Create {
                            path: "new".into(),
                            overwrite: false,
                            ignore_if_exists: false,
                        }],
                        roots: vec!["new".into()],
                        before: [("new".into(), EntryState::Missing)].into(),
                        after: [("new".into(), EntryState::file([]))].into(),
                    },
                    editor: EditorRecoveryPatch {
                        revision: 0,
                        root: EditorRecoveryRoot::for_project(&project),
                        before: vec![],
                        after: vec![],
                        order_before: vec![],
                        order_after: vec![],
                        selected_before: None,
                        selected_after: None,
                    },
                };
                input.language_review = Some(Box::new(retained_review(&input)));
                let response = begin(
                    request(serde_json::to_vec(&input).unwrap(), None),
                    &state,
                    &path,
                    user,
                )
                .await
                .unwrap();
                let prepared: WorkspaceTransactionPrepared = serde_json::from_slice(
                    &response.into_body().collect().await.unwrap().to_bytes(),
                )
                .unwrap();
                let response = get(&state, &format!("{path}/{}", input.id), user)
                    .await
                    .unwrap();
                let stored: Option<WorkspaceTransactionState> = serde_json::from_slice(
                    &response.into_body().collect().await.unwrap().to_bytes(),
                )
                .unwrap();
                assert_eq!(stored.unwrap().language_review, input.language_review);
                let scope = state
                    .store
                    .workspace_transaction_scope(owner.id, project.id, &input.id)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(scope.worker.is_none() && scope.accepted_language_review_digest.is_none());
                let retry = begin(
                    request(serde_json::to_vec(&input).unwrap(), None),
                    &state,
                    &path,
                    user,
                )
                .await
                .unwrap();
                let retry: WorkspaceTransactionPrepared =
                    serde_json::from_slice(&retry.into_body().collect().await.unwrap().to_bytes())
                        .unwrap();
                assert_eq!(retry.journal.generation, prepared.journal.generation);
            }
        });
    }

    #[test]
    fn owned_journal_http_contract_in_both_modes() {
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
                            name: "resources".into(),
                            mode,
                            path: Some("root".into()),
                        },
                        owner.id,
                        1,
                    )
                    .await
                    .unwrap();
                let user = AuthedUser {
                    id: owner.id,
                    role: owner.role,
                };
                let other = AuthedUser {
                    id: other.id,
                    role: other.role,
                };
                let path = format!("/api/projects/{}/workspace-transactions", project.id);
                let activity_path = format!("/api/projects/{}/workspace-activity", project.id);
                assert_eq!(
                    activity(&state, &activity_path, other)
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                    404
                );
                let status = activity(&state, &activity_path, user).await.unwrap();
                assert_eq!(status.headers()["cache-control"], "no-store");
                let body = status.into_body().collect().await.unwrap().to_bytes();
                assert!(body.len() < 64);
                assert_eq!(
                    serde_json::from_slice::<
                        openwebide_core::workspace_transaction::WorkspaceActivity,
                    >(&body)
                    .unwrap(),
                    Default::default()
                );

                assert_eq!(
                    root(&state, &format!("{path}/root"), other)
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                    404
                );
                let inspect_path = format!("{path}/inspect");
                assert_eq!(
                    inspect(
                        request(b"invalid".to_vec(), Some(64 * 1024 + 1)),
                        &state,
                        &inspect_path,
                        other
                    )
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                    404
                );
                if mode == WorkspaceMode::Remote {
                    for (body,size,status) in [
                        (Vec::new(),Some(64*1024+1),413),
                        (serde_json::to_vec(&json!({"root":"a".repeat(64),"action":{"operation":"create","ordinal":0,"path":"source","is_dir":false}})).unwrap(),None,400),
                        (serde_json::to_vec(&json!({"root":"a".repeat(64),"path":"another-user","action":{"operation":"list","path":""}})).unwrap(),None,400),
                    ] {
                        assert_eq!(inspect(request(body,size),&state,&inspect_path,user).await.unwrap_err().into_response().status(),status);
                    }
                } else {
                    assert_eq!(
                        inspect(
                            request(Vec::new(), Some(64 * 1024 + 1)),
                            &state,
                            &inspect_path,
                            user
                        )
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                        400
                    );
                }
                if mode == WorkspaceMode::Local {
                    assert_eq!(
                        root(&state, &format!("{path}/root"), user)
                            .await
                            .unwrap_err()
                            .into_response()
                            .status(),
                        400
                    );
                }
                let id = "0123456789abcdef0123456789abcdef";
                let exact = format!("{path}/{id}");
                // Ownership is checked before reading or disclosing a large body.
                assert_eq!(
                    begin(
                        request(b"invalid".to_vec(), Some(JOURNAL_API_BODY_LIMIT + 1)),
                        &state,
                        &path,
                        other
                    )
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                    404
                );
                assert_eq!(
                    begin(
                        request(Vec::new(), Some(JOURNAL_API_BODY_LIMIT + 1)),
                        &state,
                        &path,
                        user
                    )
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                    413
                );
                assert_eq!(
                    begin(request(b"invalid".to_vec(), None), &state, &path, user)
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                    400
                );
                let input = BeginWorkspaceTransaction {
                    language_review: None,
                    id: id.into(),
                    plan: WorkspacePlan {
                        version: 1,
                        root: "owned-root".into(),
                        operations: vec![FileOperation::Create {
                            path: "new".into(),
                            overwrite: false,
                            ignore_if_exists: false,
                        }],
                        roots: vec!["new".into()],
                        before: [("new".into(), EntryState::Missing)].into(),
                        after: [("new".into(), EntryState::file(Vec::new()))].into(),
                    },
                    editor: EditorRecoveryPatch {
                        revision: 0,
                        root: EditorRecoveryRoot::for_project(&project),
                        before: vec![],
                        after: vec![],
                        order_before: vec![],
                        order_after: vec![],
                        selected_before: None,
                        selected_after: None,
                    },
                };
                let mut with_review = input.clone();
                let review = retained_review(&input);
                review
                    .validate_transaction(&input.plan, &input.editor)
                    .unwrap();
                with_review.language_review = Some(Box::new(review));
                let rejection = begin(
                    request(serde_json::to_vec(&with_review).unwrap(), None),
                    &state,
                    &path,
                    user,
                )
                .await
                .unwrap_err()
                .into_response();
                assert_eq!(rejection.status(), 400);
                let rejection = rejection.into_body().collect().await.unwrap().to_bytes();
                assert!(
                    String::from_utf8(rejection.to_vec())
                        .unwrap()
                        .contains("Language reviews require a native resource transaction")
                );
                assert!(
                    state
                        .store
                        .workspace_transaction(owner.id, project.id, None)
                        .await
                        .unwrap()
                        .is_none()
                );
                let response = begin(
                    request(serde_json::to_vec(&input).unwrap(), None),
                    &state,
                    &path,
                    user,
                )
                .await
                .unwrap();
                let status = activity(&state, &activity_path, user).await.unwrap();
                let body = status.into_body().collect().await.unwrap().to_bytes();
                let status: openwebide_core::workspace_transaction::WorkspaceActivity =
                    serde_json::from_slice(&body).unwrap();
                assert!(status.file_recovery);
                assert!(!status.language_command);
                if mode == WorkspaceMode::Remote {
                    let file_path = format!("/api/projects/{}/files", project.id);
                    for (method, suffix, body) in [
                        ("PUT", "write?path=file", b"new bytes".to_vec()),
                        ("POST", "create?path=file", Vec::new()),
                        ("POST", "copy", br#"{"from":"file","to":"other"}"#.to_vec()),
                        ("DELETE", "delete?path=file", Vec::new()),
                    ] {
                        let request = Request::builder()
                            .method(method)
                            .uri(format!("http://localhost{file_path}/{suffix}"))
                            .body(box_body(FullBody::new(Bytes::from(body))))
                            .unwrap();
                        let route = format!("{file_path}/{}", suffix.split('?').next().unwrap());
                        let result = match method {
                            "PUT" => {
                                super::super::files::files_put(request, &state, &route, user).await
                            }
                            "POST" => {
                                super::super::files::files_post(request, &state, &route, user).await
                            }
                            _ => {
                                super::super::files::files_delete(request, &state, &route, user)
                                    .await
                            }
                        };
                        assert_eq!(
                            result.unwrap_err().into_response().status(),
                            409,
                            "{method} {suffix}"
                        );
                    }
                }
                let saved: WorkspaceTransactionPrepared = serde_json::from_slice(
                    &response.into_body().collect().await.unwrap().to_bytes(),
                )
                .unwrap();
                assert_eq!(saved.journal.id, id);
                assert_eq!(saved.journal.fingerprint, input.plan.fingerprint().unwrap());
                for address in [&path, &exact] {
                    let response = get(&state, address, user).await.unwrap();
                    let loaded: Option<WorkspaceTransactionState> = serde_json::from_slice(
                        &response.into_body().collect().await.unwrap().to_bytes(),
                    )
                    .unwrap();
                    assert_eq!(loaded.unwrap().journal.plan, input.plan);
                    assert_eq!(
                        get(&state, address, other)
                            .await
                            .unwrap_err()
                            .into_response()
                            .status(),
                        404
                    );
                }
                let mut cursor = AdvanceWorkspaceTransaction {
                    id: id.into(),
                    generation: saved.journal.generation.clone(),
                    revision: 1,
                    progress: TransactionProgress {
                        phase: TransactionPhase::Applying,
                        ..TransactionProgress::default()
                    },
                };
                // Neither the browser cancellation route nor the host callback
                // may infer the generation from the current row on a retry.
                for (generation, status) in [
                    (None, 400),
                    (Some("invalid".to_string()), 400),
                    (Some("d".repeat(32)), 409),
                ] {
                    let mut body = json!({
                        "id": id,
                        "revision": 1,
                        "progress": TransactionProgress::prepared_cancellation(),
                    });
                    if let Some(generation) = generation {
                        assert_ne!(generation, saved.journal.generation);
                        body["generation"] = json!(generation);
                    }
                    let response = advance(
                        request(serde_json::to_vec(&body).unwrap(), None),
                        &state,
                        &exact,
                        user,
                    )
                    .await
                    .unwrap_err()
                    .into_response();
                    assert_eq!(response.status(), status);
                    if mode == WorkspaceMode::Remote {
                        body["command"] = json!("advance");
                        let callback = json!({
                            "user": user.id.get(),
                            "project": project.id,
                            "id": id,
                            "operation": body,
                        });
                        // Operation identity belongs to the outer envelope.
                        let mut callback = callback;
                        callback["operation"].as_object_mut().unwrap().remove("id");
                        assert_eq!(
                            service(
                                request(serde_json::to_vec(&callback).unwrap(), None),
                                &state,
                            )
                            .await
                            .unwrap_err()
                            .into_response()
                            .status(),
                            status,
                        );
                    }
                    let unchanged = state
                        .store
                        .workspace_transaction(user.id, project.id, Some(id))
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(unchanged.journal.revision, 1);
                    assert_eq!(unchanged.journal.generation, saved.journal.generation);
                    assert_eq!(unchanged.journal.progress, TransactionProgress::default());
                }
                use openwebide_core::workspace_transaction::{
                    WorkspaceJournalOperation, WorkspaceJournalRequest, WorkspaceJournalResponse,
                };
                let mut callback = WorkspaceJournalRequest {
                    user: user.id.get(),
                    project: project.id,
                    id: id.into(),
                    operation: WorkspaceJournalOperation::Load,
                };
                if mode == WorkspaceMode::Remote {
                    assert_eq!(
                        advance(
                            request(serde_json::to_vec(&cursor).unwrap(), None),
                            &state,
                            &exact,
                            user
                        )
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                        409
                    );
                    let response = service(
                        request(serde_json::to_vec(&callback).unwrap(), None),
                        &state,
                    )
                    .await
                    .unwrap();
                    let loaded: WorkspaceJournalResponse = serde_json::from_slice(
                        &response.into_body().collect().await.unwrap().to_bytes(),
                    )
                    .unwrap();
                    assert!(matches!(
                        loaded,
                        WorkspaceJournalResponse::Loaded { state: Some(_) }
                    ));
                    callback.operation = WorkspaceJournalOperation::Advance {
                        generation: saved.journal.generation.clone(),
                        revision: 1,
                        progress: cursor.progress.clone(),
                    };
                    assert_eq!(
                        service(
                            request(serde_json::to_vec(&callback).unwrap(), None),
                            &state
                        )
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                        409
                    );
                    let worker =
                        openwebide_core::workspace_transaction::WorkspaceTransactionWorker {
                            id: "e".repeat(32),
                            secret: "f".repeat(32),
                        };
                    let scope = state
                        .store
                        .workspace_transaction_scope(user.id, project.id, id)
                        .await
                        .unwrap()
                        .unwrap();
                    callback.operation = WorkspaceJournalOperation::ClaimWorker {
                        review_digest: None,
                        language_grant: None,
                        scope,
                        worker: worker.clone(),
                    };
                    let response = service(
                        request(serde_json::to_vec(&callback).unwrap(), None),
                        &state,
                    )
                    .await
                    .unwrap();
                    let body = response.into_body().collect().await.unwrap().to_bytes();
                    assert!(!String::from_utf8_lossy(&body).contains(&worker.secret));
                    let claim: WorkspaceJournalResponse = serde_json::from_slice(&body).unwrap();
                    assert!(
                        matches!(claim, WorkspaceJournalResponse::WorkerClaimed {scope} if scope.worker==Some(worker.state()))
                    );
                    callback.operation = WorkspaceJournalOperation::AdvanceOwned {
                        worker: worker.clone(),
                        generation: saved.journal.generation.clone(),
                        revision: 1,
                        progress: cursor.progress.clone(),
                    };
                    callback.user = other.id.get();
                    assert_eq!(
                        service(
                            request(serde_json::to_vec(&callback).unwrap(), None),
                            &state
                        )
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                        404
                    );
                    callback.user = user.id.get();
                    let response = service(
                        request(serde_json::to_vec(&callback).unwrap(), None),
                        &state,
                    )
                    .await
                    .unwrap();
                    let advanced: WorkspaceJournalResponse = serde_json::from_slice(
                        &response.into_body().collect().await.unwrap().to_bytes(),
                    )
                    .unwrap();
                    assert!(
                        matches!(advanced, WorkspaceJournalResponse::Advanced { response } if response.revision == 2 && response.generation == saved.journal.generation)
                    );
                    assert_eq!(
                        service(
                            request(serde_json::to_vec(&callback).unwrap(), None),
                            &state
                        )
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                        409
                    );
                    callback.operation = WorkspaceJournalOperation::ReleaseWorker {
                        generation: saved.journal.generation.clone(),
                        worker: worker.clone(),
                    };
                    let response = service(
                        request(serde_json::to_vec(&callback).unwrap(), None),
                        &state,
                    )
                    .await
                    .unwrap();
                    let released: WorkspaceJournalResponse = serde_json::from_slice(
                        &response.into_body().collect().await.unwrap().to_bytes(),
                    )
                    .unwrap();
                    assert!(
                        matches!(released,WorkspaceJournalResponse::WorkerReleased {generation, worker:id} if generation==saved.journal.generation && id==worker.id)
                    );
                    callback.operation = WorkspaceJournalOperation::AdvanceOwned {
                        worker,
                        generation: saved.journal.generation.clone(),
                        revision: 2,
                        progress: cursor.progress.clone(),
                    };
                    assert_eq!(
                        service(
                            request(serde_json::to_vec(&callback).unwrap(), None),
                            &state
                        )
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                        409
                    );
                    assert_eq!(
                        service(request(vec![b' '; 64 * 1024 + 1], None), &state)
                            .await
                            .unwrap_err()
                            .into_response()
                            .status(),
                        413
                    );
                    continue;
                }
                assert_eq!(
                    service(
                        request(serde_json::to_vec(&callback).unwrap(), None),
                        &state
                    )
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                    400
                );
                let response = advance(
                    request(serde_json::to_vec(&cursor).unwrap(), None),
                    &state,
                    &exact,
                    user,
                )
                .await
                .unwrap();
                let saved: WorkspaceTransactionAdvanced = serde_json::from_slice(
                    &response.into_body().collect().await.unwrap().to_bytes(),
                )
                .unwrap();
                assert_eq!(saved.revision, 2);
                assert_eq!(saved.generation, cursor.generation);
                assert!(saved.editor.is_none());
                assert_eq!(
                    advance(
                        request(serde_json::to_vec(&cursor).unwrap(), None),
                        &state,
                        &exact,
                        user
                    )
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                    409
                );
                cursor.id = "abcdef0123456789abcdef0123456789".into();
                assert_eq!(
                    advance(
                        request(serde_json::to_vec(&cursor).unwrap(), None),
                        &state,
                        &exact,
                        user
                    )
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                    400
                );
                assert_eq!(
                    advance(
                        request(vec![b' '; 64 * 1024 + 1], None),
                        &state,
                        &exact,
                        user
                    )
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                    413
                );
            }
        });
    }
}

/// The router authenticates the native daemon before accepting this payload.
/// The same store checks ownership/CAS/editor commit for browser and host writes.
pub(crate) async fn service<B>(req: Request<B>, state: &AppState) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    use openwebide_core::workspace_transaction::{
        WorkspaceJournalOperation, WorkspaceJournalRequest, WorkspaceJournalResponse,
    };
    let input: WorkspaceJournalRequest = parse_json(read_body(req, 64 * 1024).await?)?;
    if input.user <= 0 || input.project <= 0 || !valid_id(&input.id) {
        return Err(ApiError::bad_request("Invalid workspace recovery scope"));
    }
    let user = UserId::new(input.user);
    let project = state.store.get_project(input.project, user).await?;
    if project.mode != WorkspaceMode::Remote {
        return Err(ApiError::bad_request(
            "The native host cannot access this local folder",
        ));
    }
    let response = match input.operation {
        WorkspaceJournalOperation::ClaimWorker {
            scope,
            worker,
            review_digest,
            language_grant,
        } => {
            if scope.id != input.id {
                return Err(ApiError::bad_request("Workspace worker identity changed"));
            }
            let review = openwebide_core::workspace_transaction::language_review_admission(
                review_digest.as_deref(),
                language_grant.as_deref(),
            )
            .map_err(ApiError::bad_request)?;
            WorkspaceJournalResponse::WorkerClaimed {
                scope: state
                    .store
                    .claim_workspace_transaction_worker_reviewed(
                        user,
                        input.project,
                        &scope,
                        &worker,
                        review,
                        now(),
                    )
                    .await?,
            }
        }
        WorkspaceJournalOperation::ReleaseWorker { generation, worker } => {
            state
                .store
                .release_workspace_transaction_worker(
                    user,
                    input.project,
                    &input.id,
                    &generation,
                    &worker,
                    now(),
                )
                .await?;
            WorkspaceJournalResponse::WorkerReleased {
                generation,
                worker: worker.id,
            }
        }
        WorkspaceJournalOperation::Scope => WorkspaceJournalResponse::Scope {
            scope: state
                .store
                .workspace_transaction_scope(user, input.project, &input.id)
                .await?,
        },
        WorkspaceJournalOperation::Load => WorkspaceJournalResponse::Loaded {
            state: state
                .store
                .workspace_transaction(user, input.project, Some(&input.id))
                .await?
                .map(Box::new),
        },
        WorkspaceJournalOperation::Advance {
            generation,
            revision,
            ..
        } => {
            if !valid_id(&generation) || revision == 0 {
                return Err(ApiError::bad_request("Invalid workspace journal revision"));
            }
            return Err(ApiError::new(
                409,
                "Native workspace execution requires an admitted worker",
            ));
        }
        WorkspaceJournalOperation::AdvanceOwned {
            worker,
            generation,
            revision,
            progress,
        } => WorkspaceJournalResponse::Advanced {
            response: state
                .store
                .advance_workspace_transaction_for_worker(
                    user,
                    input.project,
                    &AdvanceWorkspaceTransaction {
                        id: input.id,
                        generation,
                        revision,
                        progress,
                    },
                    Some(&worker),
                    now(),
                )
                .await?,
        },
    };
    Ok(json_response(200, &response))
}

pub(crate) async fn execute<B>(
    req: Request<B>,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    use openwebide_core::workspace_transaction::{
        ExecuteWorkspaceTransaction, NativeWorkspaceTransaction, WorkspaceTransactionAction,
        WorkspaceTransactionOutcome,
    };
    let path = path
        .strip_suffix("/execute")
        .ok_or_else(|| ApiError::bad_request("Invalid workspace execution path"))?;
    let (project, id) = address(path)?;
    let owned = state.store.get_project(project, user.id).await?;
    if owned.mode != WorkspaceMode::Remote {
        return Err(ApiError::bad_request(
            "Use the selected browser folder for this operation",
        ));
    }
    let request: ExecuteWorkspaceTransaction = parse_json(read_body(req, 64 * 1024).await?)?;
    if id != Some(request.id.as_str()) || !valid_id(&request.generation) {
        return Err(ApiError::bad_request(
            "Workspace transaction identity changed",
        ));
    }
    let pending = state
        .store
        .workspace_transaction(user.id, project, id)
        .await?
        .ok_or_else(|| ApiError::bad_request("The reviewed operation is unavailable"))?;
    if pending.journal.generation != request.generation {
        return Err(ApiError::new(
            409,
            "The reviewed operation was replaced; reload before continuing",
        ));
    }
    if request.action == WorkspaceTransactionAction::Apply
        && pending.editor.root != openwebide_core::editor::EditorRecoveryRoot::for_project(&owned)
    {
        return Err(ApiError::bad_request("The reviewed project folder changed"));
    }
    let generation = request.generation.clone();
    let payload = NativeWorkspaceTransaction {
        user: user.id.get(),
        project,
        request,
    };
    let (status, bytes) = crate::bridge::send(
        &state.store,
        "/workspace/execute",
        serde_json::to_string(&payload).map_err(|e| ApiError::bad_request(e.to_string()))?,
    )
    .await?;
    if status != 200 {
        let error = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| value["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| "The execution host could not apply the reviewed operation".into());
        return Err(ApiError::bad_request(error));
    }
    let outcome: WorkspaceTransactionOutcome = serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::bad_request("The execution host returned an invalid workspace result")
    })?;
    outcome
        .validate(&generation)
        .map_err(ApiError::bad_request)?;
    Ok(json_response(200, &outcome))
}

pub(crate) async fn inspect<B>(
    req: Request<B>,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    use openwebide_core::workspace_transaction::companion::{
        FileValue, FolderInspection, NativeFolderInspection, REQUEST_LIMIT,
    };
    let path = path
        .strip_suffix("/inspect")
        .ok_or_else(|| ApiError::bad_request("Invalid workspace inspection path"))?;
    let (project, id) = address(path)?;
    if id.is_some() {
        return Err(ApiError::bad_request(
            "Unexpected workspace inspection identity",
        ));
    }
    let owned = state.store.get_project(project, user.id).await?;
    if owned.mode != WorkspaceMode::Remote {
        return Err(ApiError::bad_request(
            "Use the selected browser folder for this operation",
        ));
    }
    let inspection: FolderInspection = parse_json(read_body(req, REQUEST_LIMIT).await?)?;
    inspection.validate().map_err(ApiError::bad_request)?;
    let request = NativeFolderInspection {
        user: user.id.get(),
        project,
        path: owned
            .path
            .ok_or_else(|| ApiError::bad_request("The project folder is not configured"))?,
        inspection,
    };
    request.validate().map_err(ApiError::bad_request)?;
    let (status, bytes) = crate::bridge::send(
        &state.store,
        "/workspace/inspect",
        serde_json::to_string(&request)
            .map_err(|error| ApiError::bad_request(error.to_string()))?,
    )
    .await?;
    if status != 200 {
        let error = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| value["error"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "The execution host could not inspect the project folder".into());
        return Err(ApiError::bad_request(error));
    }
    let value: FileValue = serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::bad_request("The execution host returned an invalid workspace inspection")
    })?;
    Ok(json_response(200, &value))
}

pub(crate) async fn root(
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    use openwebide_core::workspace_transaction::WorkspaceRootIdentity;
    let path = path
        .strip_suffix("/root")
        .ok_or_else(|| ApiError::bad_request("Invalid workspace root path"))?;
    let (project, id) = address(path)?;
    if id.is_some() {
        return Err(ApiError::bad_request("Unexpected workspace root identity"));
    }
    let owned = state.store.get_project(project, user.id).await?;
    if owned.mode != WorkspaceMode::Remote {
        return Err(ApiError::bad_request(
            "Use the selected browser folder for this operation",
        ));
    }
    let path = owned
        .path
        .ok_or_else(|| ApiError::bad_request("The project folder is not configured"))?;
    let (status, bytes) = crate::bridge::send(
        &state.store,
        "/workspace/root",
        json!({"user":user.id.get(),"project":project,"path":path}).to_string(),
    )
    .await?;
    if status != 200 {
        return Err(ApiError::bad_request(
            "The execution host could not identify the project folder",
        ));
    }
    let root: WorkspaceRootIdentity = serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::bad_request("The execution host returned an invalid folder identity")
    })?;
    Ok(json_response(200, &root))
}

#[cfg(test)]
mod cancellation_contract {
    use super::*;
    use openwebide_core::{
        NewProject, UserRole,
        editor::EditorRecoveryRoot,
        workspace_transaction::{
            EditorRecoveryPatch, EntryState, FileOperation, TransactionProgress, WorkspacePlan,
        },
    };
    #[test]
    fn unstarted_operations_can_cancel_without_filesystem_access_in_both_modes() {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let state = AppState::new().await.unwrap();
                let owner = state
                    .store
                    .insert_user("owner", "hash", UserRole::Admin, 1)
                    .await
                    .unwrap();
                let project = state
                    .store
                    .create_project(
                        &NewProject {
                            name: "cancel".into(),
                            mode,
                            path: Some("missing-folder".into()),
                        },
                        owner.id,
                        1,
                    )
                    .await
                    .unwrap();
                let user = AuthedUser {
                    id: owner.id,
                    role: owner.role,
                };
                let id = "0123456789abcdef0123456789abcdef";
                let prepared = state
                    .store
                    .begin_workspace_transaction(
                        user.id,
                        project.id,
                        &BeginWorkspaceTransaction {
                            language_review: None,
                            id: id.into(),
                            plan: WorkspacePlan {
                                version: 1,
                                root: "approved-root".into(),
                                operations: vec![FileOperation::Create {
                                    path: "new".into(),
                                    overwrite: false,
                                    ignore_if_exists: false,
                                }],
                                roots: vec!["new".into()],
                                before: [("new".into(), EntryState::Missing)].into(),
                                after: [("new".into(), EntryState::file(Vec::new()))].into(),
                            },
                            editor: EditorRecoveryPatch {
                                revision: 0,
                                root: EditorRecoveryRoot::for_project(&project),
                                before: vec![],
                                after: vec![],
                                order_before: vec![],
                                order_after: vec![],
                                selected_before: None,
                                selected_after: None,
                            },
                        },
                        1,
                    )
                    .await
                    .unwrap();
                let input = AdvanceWorkspaceTransaction {
                    id: id.into(),
                    generation: prepared.journal.generation,
                    revision: 1,
                    progress: TransactionProgress::prepared_cancellation(),
                };
                let request = Request::builder()
                    .body(FullBody::new(Bytes::from(
                        serde_json::to_vec(&input).unwrap(),
                    )))
                    .unwrap();
                let response = advance(
                    request,
                    &state,
                    &format!("/api/projects/{}/workspace-transactions/{id}", project.id),
                    user,
                )
                .await
                .unwrap();
                assert_eq!(response.status(), 200);
                assert_eq!(
                    state
                        .store
                        .workspace_transaction(user.id, project.id, Some(id))
                        .await
                        .unwrap()
                        .unwrap()
                        .journal
                        .progress,
                    TransactionProgress::prepared_cancellation()
                );
                assert_eq!(
                    state
                        .store
                        .editor_recovery(user.id, project.id)
                        .await
                        .unwrap()
                        .revision,
                    0
                );
                // The absent physical folder was never resolved or written.
                state
                    .store
                    .delete_project(project.id, user.id)
                    .await
                    .unwrap();
            }
        });
    }
}
