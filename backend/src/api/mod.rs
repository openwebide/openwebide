//! API handlers.

pub(crate) mod assistance;
pub(crate) mod completion;
pub(crate) mod host_admin;
pub(crate) mod naming;
pub(crate) mod plugin_jobs;
pub(crate) mod plugin_runs;
pub(crate) mod scheduled;
pub(crate) mod session_search;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use http_body_util::BodyExt;
use openwebide_agent::AgentConfig;
use openwebide_core::{
    ChatRequest, EditorContext, FileDiff, FileEntry, GitCheckoutRequest, GitCommitRequest,
    GitSyncRequest, Health, NewConnection, NewProject, Role, RunKind, RunPlan, SearchHit,
    SystemPrompt, TurnTelemetry, WorkspaceMode, vfs::SearchOptions, with_temporal_context,
};
use openwebide_llm::{LlmProvider, ToolStreamMemo, registry::Provider};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use spin_sdk::http::{FullBody, Request, Response, box_body};

use crate::agent::{CancelFlag, PermissionPoller, agent_stream, workspace_tools};
use crate::auth::AuthedUser;
use crate::error::{ApiError, JsonResp};
use crate::http_client::SpinHttpClient;
use crate::sse::{SseBody, message_stream};
use crate::state::{AppState, now, now_ms};
use openwebide_core::UserId;

fn json_response(status: u16, value: &impl serde::Serialize) -> JsonResp {
    let body = serde_json::to_string(value).unwrap_or_else(|_| "{}".into());
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(box_body(FullBody::new(Bytes::from(body))))
        .expect("valid status and headers")
}

/// Per-route request-body size caps. Auth and JSON bodies stay small; chat
/// endpoints carry whole transcripts (local-mode `chat-tools` resends the
/// full history); file writes mirror the file-read cap.
const AUTH_BODY_LIMIT: usize = 64 * 1024;
const CHAT_BODY_LIMIT: usize = 16 * 1024 * 1024;
#[allow(clippy::cast_possible_truncation)] // The 10 MiB read limit fits usize on every supported target.
const FILE_BODY_LIMIT: usize = crate::files::MAX_READ_BYTES as usize;
const JSON_BODY_LIMIT: usize = 1024 * 1024;
// 200 × 2,000 characters can expand to seven bytes each in double-encoded JSON.
const SETTINGS_BODY_LIMIT: usize = 4 * 1024 * 1024;

/// Collect a body already wrapped in [`http_body_util::Limited`], mapping a
/// length-limit violation to 413 and any other body error to 400.
async fn read_limited_bytes<B>(
    body: http_body_util::Limited<B>,
    limit: usize,
) -> Result<Bytes, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let collected = body.collect().await.map_err(|e| {
        if e.downcast_ref::<http_body_util::LengthLimitError>()
            .is_some()
        {
            ApiError::payload_too_large(format!("request body exceeds {limit} bytes"))
        } else {
            ApiError::bad_request(format!("read request body: {e}"))
        }
    })?;
    Ok(collected.to_bytes())
}

/// Read the raw request body, refusing bodies over `limit` bytes with
/// a 413 — first from a lying `content-length`, then from the actual byte
/// count via the `Limited` wrapper.
async fn read_bytes_body(req: Request, limit: usize) -> Result<Bytes, ApiError> {
    if let Some(len) = req
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        && len > limit
    {
        return Err(ApiError::payload_too_large(format!(
            "request body exceeds {limit} bytes"
        )));
    }
    read_limited_bytes(http_body_util::Limited::new(req.into_body(), limit), limit).await
}

#[cfg(test)]
async fn read_limited<B>(body: http_body_util::Limited<B>, limit: usize) -> Result<String, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let bytes = read_limited_bytes(body, limit).await?;
    String::from_utf8(bytes.to_vec())
        .map_err(|_| ApiError::bad_request("request body is not valid UTF-8"))
}

async fn read_body(req: Request, limit: usize) -> Result<String, ApiError> {
    let bytes = read_bytes_body(req, limit).await?;
    String::from_utf8(bytes.to_vec())
        .map_err(|_| ApiError::bad_request("request body is not valid UTF-8"))
}

fn parse_json<T: DeserializeOwned>(body: String) -> Result<T, ApiError> {
    serde_json::from_str(&body).map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))
}

fn path_id(path: &str, prefix: &str) -> Result<i64, ApiError> {
    path.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('/'))
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or_else(|| ApiError::bad_request(format!("expected a numeric id after {prefix}/")))
}

pub(super) mod auth;
pub(super) mod bridge;
pub(super) mod chat;
pub(super) mod connections;
pub(super) mod editor_recovery;
pub(super) mod files;
pub(super) mod git;
pub(crate) mod memories;
pub(crate) mod model_operations;
pub(crate) mod model_setup;
mod paths;
pub(crate) mod plugins;
pub(super) mod projects;
pub(super) mod prompts;
pub(super) mod push;
mod query;
pub(super) mod reviews;
pub(super) mod sessions;
pub(super) mod settings;
pub(crate) mod skills;
pub(super) mod web;
use paths::*;
use query::query;

#[cfg(test)]
mod tests;

pub(crate) mod approvals;

pub(crate) mod questions;
