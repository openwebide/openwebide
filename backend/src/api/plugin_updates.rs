//! Bridge-only persistence adapter for shared plugin discovery checks.
use super::*;
use openwebide_core::plugins::updates::{UpdateServiceRequest, UpdateServiceResponse};

pub(crate) async fn service(req: Request, state: &AppState) -> Result<JsonResp, ApiError> {
    let command: UpdateServiceRequest = parse_json(read_body(req, 20 * 1024 * 1024).await?)?;
    let response = match command {
        UpdateServiceRequest::Claim {
            host_id,
            server,
            after,
        } => {
            let lease = format!("{:032x}", rand::random::<u128>());
            let (claims, next) = state
                .store
                .claim_plugin_update_checks(&host_id, server, after, &lease, now())
                .await?;
            UpdateServiceResponse::Claimed { claims, next }
        }
        UpdateServiceRequest::Finish { claim, refresh } => {
            state
                .store
                .finish_plugin_update_check(&claim, &refresh, now())
                .await?;
            UpdateServiceResponse::Finished
        }
    };
    Ok(json_response(200, &response))
}
