//! Thin daemon RPC over the shared durable job store and callback execution facade.
use super::*;
use openwebide_core::plugins::jobs::{JobServiceRequest, JobServiceResponse};

pub(crate) async fn service(req: Request, state: &AppState) -> Result<JsonResp, ApiError> {
    let command: JobServiceRequest = parse_json(read_body(req, 2 * 1024 * 1024).await?)?;
    let result = match command {
        JobServiceRequest::Claim { host_id, after } => {
            let lease = format!("{:032x}", rand::random::<u128>());
            JobServiceResponse::Claimed(
                state
                    .store
                    .claim_plugin_jobs(&host_id, after, &lease, now())
                    .await?,
            )
        }
        JobServiceRequest::Renew { delivery } => JobServiceResponse::Renewed(
            state
                .store
                .renew_plugin_job(&delivery.host_id, delivery.id, &delivery.lease, now())
                .await?,
        ),
        JobServiceRequest::Finish {
            delivery,
            success,
            detail,
        } => {
            state
                .store
                .finish_plugin_job(
                    &delivery.host_id,
                    delivery.id,
                    &delivery.lease,
                    success,
                    &detail,
                    now(),
                )
                .await?;
            JobServiceResponse::Finished
        }
        JobServiceRequest::Grant { user_id, delivery } => {
            let grant = format!("{:032x}", rand::random::<u128>());
            let (prepared, context) = state
                .store
                .issue_plugin_job_grant(
                    openwebide_core::UserId::new(user_id),
                    &delivery.host_id,
                    delivery.id,
                    &delivery.lease,
                    &grant,
                    now(),
                )
                .await?;
            JobServiceResponse::Authorized {
                prepared: Box::new(prepared),
                context,
                grant,
            }
        }
        JobServiceRequest::Callback { user_id, request } => JobServiceResponse::Callback(
            super::plugins::execute_host_request(
                &state.store,
                openwebide_core::UserId::new(user_id),
                None,
                &request,
            )
            .await?,
        ),
    };
    Ok(json_response(200, &result))
}
