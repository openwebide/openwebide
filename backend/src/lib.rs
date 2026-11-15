//! Open WebIDE backend: a Spin HTTP component exposing the REST API over
//! Spin's `sqlite` capability.

mod agent;
mod api;
mod auth;
mod bridge;
mod bridge_client;
mod error;
mod files;
mod git;
mod http_client;
mod mime;
mod push;
mod recovery_authority;
mod router;
mod sse;
mod state;
mod url;
mod web;

use spin_sdk::http::{IntoResponse, Request};
use spin_sdk::http_service;

// Generate bindings for the extra component dependencies declared in
// `spin-dependencies.wit` (the WASI filesystem, used for remote-mode projects).
spin_sdk::dependencies!();

#[http_service]
async fn handle(req: Request) -> impl IntoResponse {
    router::route(req).await
}
