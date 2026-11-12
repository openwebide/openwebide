use super::*;
// -- project files (remote mode) ---------------------------------------------------

/// Fresh dispatch preflight, shared by all direct filesystem mutations. The
/// native physical-folder worker must additionally serialize accepted writes;
/// an idle observation is not an execution lease.
async fn ensure_idle(state: &AppState, user: UserId, project: i64) -> Result<(), ApiError> {
    state
        .store
        .workspace_activity(user, project)
        .await?
        .ensure_idle()
        .map_err(ApiError::conflict)
}

/// Load a remote project and join its base with a project-relative path.
pub(crate) async fn remote_project_path(
    state: &AppState,
    user_id: UserId,
    id: i64,
    rel: &str,
) -> Result<(String, String), ApiError> {
    let project = state.store.get_project(id, user_id).await?;
    if project.mode != openwebide_core::WorkspaceMode::Remote {
        return Err(ApiError::bad_request(
            "file access is only available for remote-mode projects",
        ));
    }
    let base = project
        .path
        .ok_or_else(|| ApiError::bad_request("project folder is not configured"))?;
    let base = openwebide_core::vfs::workspace_path(&base)
        .map_err(|_| ApiError::bad_request("project path escapes the workspace root"))?;
    let rel = openwebide_core::vfs::workspace_path(rel)
        .map_err(|_| ApiError::bad_request("path escapes the project folder or is reserved"))?;
    let full = if base.is_empty() {
        rel.clone()
    } else if rel.is_empty() {
        base.clone()
    } else {
        format!("{base}/{rel}")
    };
    Ok((full, base))
}

/// Strip the project's base-path prefix from each entry's path so the
/// returned paths are project-relative — matching what the read, write,
/// create, and search endpoints expect in `?path=`.
pub(super) fn strip_base(base: &str, entries: Vec<FileEntry>) -> Vec<FileEntry> {
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        return entries;
    }
    let prefix = format!("{base}/");
    entries
        .into_iter()
        .map(|e| {
            let path = e
                .path
                .strip_prefix(prefix.as_str())
                .unwrap_or(e.path.as_str())
                .to_string();
            FileEntry { path, ..e }
        })
        .collect()
}

/// Strip the project's base-path prefix from each search hit's path so the
/// returned paths are project-relative.
pub(super) fn strip_base_hits(base: &str, hits: Vec<SearchHit>) -> Vec<SearchHit> {
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        return hits;
    }
    let prefix = format!("{base}/");
    hits.into_iter()
        .map(|h| {
            let path = h
                .path
                .strip_prefix(prefix.as_str())
                .unwrap_or(h.path.as_str())
                .to_string();
            SearchHit { path, ..h }
        })
        .collect()
}

pub(super) fn search_query(value: Option<String>) -> Result<String, ApiError> {
    let query = value.ok_or_else(|| ApiError::bad_request("missing ?q="))?;
    if query.trim().is_empty() {
        return Err(ApiError::bad_request("query must not be empty"));
    }
    Ok(query)
}

pub(super) fn is_include_ignored(value: Option<String>) -> bool {
    matches!(value.as_deref(), Some("1" | "true"))
}

pub(super) fn raw_headers(rel: &str) -> Vec<(&'static str, String)> {
    let mut headers = vec![
        ("x-content-type-options", "nosniff".to_string()),
        ("content-security-policy", "sandbox".to_string()),
    ];
    let mime = crate::mime::mime_type_from_path(rel);
    if mime.starts_with("image/") {
        headers.push(("content-type", mime.to_string()));
    } else {
        headers.push(("content-type", "application/octet-stream".to_string()));
        headers.push(("content-disposition", "attachment".to_string()));
    }
    headers
}
/// Percent-decode a query parameter value (e.g. `src%2Fhello.rs` ->
/// `src/hello.rs`). The frontend percent-encodes every non-unreserved byte in
/// a path, including `/`, so the backend must decode before using it.
pub(crate) async fn files_get(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let params = query(&req);
    let user_id = user.id;
    let (id, sub) = project_files_path(path)?;
    match sub {
        "files/canonical" => {
            use openwebide_core::Vfs;
            let rel = params
                .get("path")
                .cloned()
                .ok_or_else(|| ApiError::bad_request("missing ?path="))?;
            let (_, base) = remote_project_path(state, user_id, id, &rel).await?;
            let canonical = crate::files::HostFsVfs::new(base)
                .canonicalize(&rel)
                .await
                .map_err(crate::files::FsError::from)?;
            Ok(json_response(200, &json!({"path": canonical})))
        }
        "files/context" => {
            let (base, _) = remote_project_path(state, user_id, id, "").await?;
            let project = state.store.get_project(id, user_id).await?;
            let mut tools = if params.get("tools").map(String::as_str) == Some("false") {
                Vec::new()
            } else {
                crate::agent::workspace_tools()
            };
            if let Some(connection) = params.get("connection").filter(|value| !value.is_empty()) {
                let connection: i64 = connection
                    .parse()
                    .map_err(|_| ApiError::bad_request("invalid connection"))?;
                state
                    .store
                    .get_connection(connection)
                    .await?
                    .tool_selection
                    .apply(&mut tools);
            }
            let environment = openwebide_core::RunEnvironment {
                project_id: Some(project.id),
                browser_preferences: params
                    .get("browser_preferences")
                    .and_then(|value| serde_json::from_str(value).ok())
                    .flatten(),
                project_name: Some(project.name.clone()),
                project_root: openwebide_core::run::execution_root(&project),
                mode: Some(project.mode),
                timestamp: now(),
            };
            let content = openwebide_agent::context::RunContext::new(environment)
                .startup(
                    &crate::files::HostFsVfs::new(base.clone()),
                    &crate::bridge_client::SpinBridgeClient::for_project(&state.store, base),
                    &tools,
                )
                .await;
            Ok(json_response(200, &json!({"content":content})))
        }

        "files" => {
            let rel = params.get("path").cloned().unwrap_or_default();
            let (full, base) = remote_project_path(state, user_id, id, &rel).await?;
            let entries = crate::files::list(&full).await?;
            Ok(json_response(200, &strip_base(&base, entries)))
        }
        "files/read" => {
            let rel = params
                .get("path")
                .cloned()
                .ok_or_else(|| ApiError::bad_request("missing ?path="))?;
            let (full, _) = remote_project_path(state, user_id, id, &rel).await?;
            let content = String::from_utf8(crate::files::read_bytes(&full).await?)
                .map_err(|_| ApiError::bad_request("file is not valid UTF-8"))?;
            Ok(json_response(
                200,
                &json!({ "path": rel, "content": content }),
            ))
        }
        "files/raw" | "files/bounded" => {
            let rel = params
                .get("path")
                .cloned()
                .ok_or_else(|| ApiError::bad_request("missing ?path="))?;
            let (full, _) = remote_project_path(state, user_id, id, &rel).await?;
            let bounded = sub == "files/bounded";
            let bytes = if bounded {
                let max = params
                    .get("max_bytes")
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|max| *max <= crate::files::MAX_READ_BYTES)
                    .ok_or_else(|| ApiError::bad_request("invalid ?max_bytes="))?;
                crate::files::read_bytes_with_limit(&full, max).await?
            } else {
                crate::files::read_bytes(&full).await?
            };

            let mut builder = Response::builder().status(200);
            for (k, v) in raw_headers(&rel) {
                builder = builder.header(k, v);
            }
            let resp = builder
                .header(
                    "cache-control",
                    if bounded {
                        "no-store"
                    } else {
                        "private, max-age=300"
                    },
                )
                .body(box_body(FullBody::new(Bytes::from(bytes))))
                .map_err(|e| ApiError::internal(e.to_string()))?;
            Ok(resp)
        }
        "files/search" => {
            let q = search_query(params.get("q").cloned())?;
            let rel = params.get("path").cloned().unwrap_or_default();
            let opts = SearchOptions {
                include_ignored: is_include_ignored(params.get("include_ignored").cloned()),
            };
            let (full, base) = remote_project_path(state, user_id, id, &rel).await?;
            let entries = crate::files::search(&full, &q, opts).await?;
            Ok(json_response(200, &strip_base(&base, entries)))
        }
        "files/content-search" => {
            let q = search_query(params.get("q").cloned())?;
            let rel = params.get("path").cloned().unwrap_or_default();
            let opts = SearchOptions {
                include_ignored: is_include_ignored(params.get("include_ignored").cloned()),
            };
            let (full, base) = remote_project_path(state, user_id, id, &rel).await?;
            let hits = crate::files::full_text_search(&full, &q, opts).await?;
            Ok(json_response(200, &strip_base_hits(&base, hits)))
        }
        other => Err(ApiError::not_found(format!("no file route for {other}"))),
    }
}

pub(crate) async fn files_put<B>(
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
    let params = query(&req);
    let user_id = user.id;
    let (id, sub) = project_files_path(path)?;
    if sub != "files/write" {
        return Err(ApiError::not_found(format!("no file route for {sub}")));
    }
    let rel = params
        .get("path")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("missing ?path="))?;
    let (full, _) = remote_project_path(state, user_id, id, &rel).await?;
    let binary = req
        .headers()
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/octet-stream"))
        });
    let content = read_bytes_body(req, FILE_BODY_LIMIT).await?;
    if !binary && std::str::from_utf8(&content).is_err() {
        return Err(ApiError::bad_request("request body is not valid UTF-8"));
    }
    ensure_idle(state, user_id, id).await?;
    crate::files::write(&full, &content).await?;
    Ok(json_response(200, &json!({ "path": rel })))
}

#[derive(Deserialize)]
pub(super) struct CopyBody {
    from: String,
    to: String,
}

pub(crate) async fn files_post<B>(
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
    let params = query(&req);
    let user_id = user.id;
    let (id, sub) = project_files_path(path)?;
    match sub {
        "files/create" => {
            let rel = params
                .get("path")
                .cloned()
                .ok_or_else(|| ApiError::bad_request("missing ?path="))?;
            let kind = params.get("type").cloned().unwrap_or_else(|| "file".into());
            let is_dir = kind == "dir";
            let (full, _) = remote_project_path(state, user_id, id, &rel).await?;
            ensure_idle(state, user_id, id).await?;
            crate::files::create(&full, is_dir).await?;
            Ok(json_response(
                201,
                &json!({ "path": rel, "is_dir": is_dir }),
            ))
        }
        "files/copy" => {
            let body = read_body(req, JSON_BODY_LIMIT).await?;
            let args: CopyBody = parse_json(body)?;
            let (full_from, _) = remote_project_path(state, user_id, id, &args.from).await?;
            let (full_to, _) = remote_project_path(state, user_id, id, &args.to).await?;
            ensure_idle(state, user_id, id).await?;
            crate::files::copy(&full_from, &full_to).await?;
            Ok(json_response(
                200,
                &json!({ "from": args.from, "to": args.to }),
            ))
        }
        _ => Err(ApiError::not_found(format!("no file route for {sub}"))),
    }
}

pub(crate) async fn files_delete<B>(
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
    let params = query(&req);
    let user_id = user.id;
    let (id, sub) = project_files_path(path)?;
    if sub != "files/delete" {
        return Err(ApiError::not_found(format!("no file route for {sub}")));
    }
    let rel = params
        .get("path")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("missing ?path="))?;
    let (full, _) = remote_project_path(state, user_id, id, &rel).await?;
    ensure_idle(state, user_id, id).await?;
    crate::files::delete(&full).await?;
    Ok(json_response(200, &json!({ "path": rel })))
}
