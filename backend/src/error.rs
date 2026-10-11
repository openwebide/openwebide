//! API error type and JSON response helpers.

use bytes::Bytes;
use serde_json::json;
use spin_sdk::http::{BoxBody, FullBody, Response, box_body};

/// The response type used across the API: a plain `http` response with a
/// type-erased body, so JSON responses and SSE streams share one type.
pub type JsonResp = Response<BoxBody>;

#[derive(Debug)]
pub struct ApiError {
    status: u16,
    message: String,
}

impl ApiError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            message: message.into(),
        }
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: 401,
            message: message.into(),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: 403,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: 404,
            message: message.into(),
        }
    }

    pub fn not_implemented(message: impl Into<String>) -> Self {
        Self {
            status: 501,
            message: message.into(),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: 409,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: 500,
            message: message.into(),
        }
    }

    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            status: 502,
            message: message.into(),
        }
    }

    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            status: 413,
            message: message.into(),
        }
    }

    pub fn too_many_requests(message: impl Into<String>) -> Self {
        Self {
            status: 429,
            message: message.into(),
        }
    }

    pub fn log_for_route(&self, method: &str, path: &str) {
        if self.status == 500 {
            eprintln!("{method} {path}: {self:#}");
        }
    }

    /// Build the JSON error response. A plain method (not an `IntoResponse`
    /// impl) so that `Result<T, ApiError>` handlers can convert it
    /// explicitly.
    pub fn into_response(self) -> JsonResp {
        let message = self.public_message();
        let body = json!({ "error": message }).to_string();
        Response::builder()
            .status(self.status)
            .header("content-type", "application/json")
            .body(box_body(FullBody::new(Bytes::from(body))))
            .expect("valid status and headers")
    }

    pub fn public_message(&self) -> &str {
        if self.status == 500 {
            "internal error"
        } else {
            &self.message
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<openwebide_storage::StorageError> for ApiError {
    fn from(err: openwebide_storage::StorageError) -> Self {
        match err {
            openwebide_storage::StorageError::NotFound(msg) => Self::not_found(msg),
            openwebide_storage::StorageError::InvalidRequest(msg) => Self::bad_request(msg),
            openwebide_storage::StorageError::Conflict(msg) => Self::conflict(msg),
            internal @ (openwebide_storage::StorageError::InvalidValue(_)
            | openwebide_storage::StorageError::Db(_)) => Self::internal(internal.to_string()),
        }
    }
}

impl From<openwebide_llm::ProviderError> for ApiError {
    fn from(err: openwebide_llm::ProviderError) -> Self {
        match err {
            openwebide_llm::ProviderError::NotImplemented(msg) => Self::not_implemented(msg),
            openwebide_llm::ProviderError::Authentication => Self::bad_request(err.to_string()),
            openwebide_llm::ProviderError::NoModel => Self::bad_request(err.to_string()),
            other => Self::internal(other.to_string()),
        }
    }
}

impl From<crate::files::FsError> for ApiError {
    fn from(err: crate::files::FsError) -> Self {
        use crate::files::FsError;
        let status = match &err {
            FsError::Reserved(_) | FsError::PathEscape(_) | FsError::TooLarge { .. } => 400,
            FsError::AlreadyExists(_) => 409,
            FsError::NotFound(_) => 404,
            FsError::PermissionDenied(_) => 403,
            FsError::Io(_) => 500,
        };
        Self::new(status, err.to_string())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        Self::internal(format!("{err:#}"))
    }
}

impl From<crate::git::BridgeError> for ApiError {
    fn from(err: crate::git::BridgeError) -> Self {
        match err {
            crate::git::BridgeError::Unreachable(e) => Self::bad_gateway(format!(
                "bridge daemon unreachable: {e} please start openwebide-bridge"
            )),
            crate::git::BridgeError::Status(status, msg) => {
                if (400..500).contains(&status) {
                    Self::bad_request(msg)
                } else {
                    Self::bad_gateway(msg)
                }
            }
            crate::git::BridgeError::Parse(e) => {
                Self::bad_gateway(format!("failed to parse bridge response: {e}"))
            }
            crate::git::BridgeError::Unauthorized | crate::git::BridgeError::NoSecret => {
                Self::bad_gateway(
                    "the bridge rejected this server (set SPIN_VARIABLE_BRIDGE_SECRET to the bridge's secret; see the bridge's startup output)",
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_status_table() {
        use crate::files::FsError;
        for (error, status) in [
            (FsError::Reserved(".spin".into()), 400),
            (FsError::AlreadyExists("a".into()), 409),
            (FsError::NotFound("a".into()), 404),
            (FsError::PermissionDenied("a".into()), 403),
            (FsError::TooLarge { size: 11, max: 10 }, 400),
            (FsError::PathEscape("../a".into()), 400),
            (FsError::Io("disk failed".into()), 500),
        ] {
            let error: ApiError = error.into();
            assert_eq!(error.status, status);
        }
    }

    #[test]
    fn storage_status_table() {
        use openwebide_storage::StorageError;
        for (error, status) in [
            (StorageError::Db("db failed".into()), 500),
            (StorageError::InvalidValue("invalid row".into()), 500),
            (
                StorageError::InvalidRequest("Project memory is disabled".into()),
                400,
            ),
            (StorageError::NotFound("missing".into()), 404),
            (StorageError::Conflict("duplicate".into()), 409),
        ] {
            let error: ApiError = error.into();
            assert_eq!(error.status, status);
        }
    }

    #[test]
    fn internal_response_hides_detail_retained_for_logging() {
        use http_body_util::BodyExt;
        futures::executor::block_on(async {
            let error = ApiError::internal("database password and query detail");
            assert_eq!(format!("{error:#}"), "database password and query detail");
            error.log_for_route("GET", "/api/settings");
            let response = error.into_response();
            assert_eq!(response.status().as_u16(), 500);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                json!({"error": "internal error"})
            );
        });
    }

    #[test]
    fn already_exists_in_workspace_maps_to_409() {
        let err: ApiError = crate::files::FsError::AlreadyExists("src/main.rs".into()).into();
        let resp = err.into_response();
        assert_eq!(resp.status().as_u16(), 409);
    }

    #[test]
    fn payload_too_large_maps_to_413() {
        let err = ApiError::payload_too_large("request body exceeds 65536 bytes");
        assert_eq!(err.status, 413);
        assert_eq!(err.message, "request body exceeds 65536 bytes");
        assert_eq!(err.into_response().status().as_u16(), 413);
    }

    #[test]
    fn test_bridge_error_to_api_error() {
        let err: ApiError = crate::git::BridgeError::Unreachable("timeout".into()).into();
        assert_eq!(err.status, 502);
        assert!(err.message.contains("unreachable"));

        let err: ApiError = crate::git::BridgeError::Status(400, "bad args".into()).into();
        assert_eq!(err.status, 400);
        assert_eq!(err.message, "bad args");

        let err: ApiError = crate::git::BridgeError::Status(404, "not found".into()).into();
        assert_eq!(err.status, 400);

        let err: ApiError = crate::git::BridgeError::Status(500, "boom".into()).into();
        assert_eq!(err.status, 502);
        assert_eq!(err.message, "boom");

        let err: ApiError = crate::git::BridgeError::Parse("bad json".into()).into();
        assert_eq!(err.status, 502);

        let err: ApiError = crate::git::BridgeError::Unauthorized.into();
        assert_eq!(err.status, 502);
        assert!(err.message.contains("rejected this server"));

        let err: ApiError = crate::git::BridgeError::NoSecret.into();
        assert_eq!(err.status, 502);
        assert!(err.message.contains("rejected this server"));
    }
}
