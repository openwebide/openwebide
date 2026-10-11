//! Authenticated daemon primitives for durable raw plugin runs.
use super::*;
use openwebide_core::plugins::runs::{RunServiceRequest, RunServiceResponse};
pub(crate) async fn service(req: Request, state: &AppState) -> Result<JsonResp, ApiError> {
    let command: RunServiceRequest = parse_json(read_body(req, 1024 * 1024).await?)?;
    let result = match command {
        RunServiceRequest::Claim { host_id, after } => RunServiceResponse::Claimed(
            state
                .store
                .claim_plugin_runs(
                    &host_id,
                    after,
                    &format!("{:032x}", rand::random::<u128>()),
                    now(),
                )
                .await?,
        ),
        RunServiceRequest::Renew { delivery } => {
            RunServiceResponse::Renewed(state.store.renew_plugin_run(&delivery, now()).await?)
        }
        RunServiceRequest::Report { delivery, report } => {
            state
                .store
                .report_plugin_run(&delivery, &report, now())
                .await?;
            RunServiceResponse::Reported
        }
        RunServiceRequest::Release { delivery } => {
            state.store.release_plugin_run(&delivery, now()).await?;
            RunServiceResponse::Released
        }
    };
    Ok(json_response(200, &result))
}
