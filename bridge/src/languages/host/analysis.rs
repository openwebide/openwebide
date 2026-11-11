//! Stateless, bounded proofs for analysis produced by one retained service.
//! A proof is freshness evidence, never filesystem mutation authority.
use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

pub(super) mod affected;

const LIFETIME: u64 = 300;

/// Read-only results remain available when complete observation is unavailable.
/// They carry no proof and cannot be reused as verified analysis or resolved
/// into edits. Speculation, validation and actual edits require full coverage.
pub(super) async fn observe(
    query: &Query,
    sources: Option<&generation::Tracker>,
    documents: &mut documents::Documents,
    root: &std::path::Path,
) -> Result<Option<generation::Observation>, String> {
    let Some(sources) = sources else {
        return Ok(None);
    };
    match sources.observe(root).await {
        Ok(observation) => Ok(Some(observation)),
        Err(error) => {
            documents.invalidate_generation();
            if query.is_prefetch() || query.requires_analysis_context() {
                return Err(error);
            }
            tracing::debug!(%error, "Read-only analysis has no complete project-source context");
            Ok(None)
        }
    }
}
pub(super) struct Analysis {
    key: [u8; 32],
    started: Instant,
}
impl Default for Analysis {
    fn default() -> Self {
        Self {
            key: rand::random(),
            started: Instant::now(),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    version: u32,
    server: String,
    origin: String,
    request: String,
    result: String,
    sources: u64,
    overlays: u64,
    issued: u64,
    affected: String,
}
impl Analysis {
    #[cfg(test)]
    pub fn issue(
        &self,
        server: &str,
        input: &Request,
        sources: generation::Observation,
        overlays: u64,
        result: &Value,
    ) -> Result<protocol::AnalysisContext, String> {
        self.issue_observed(server, input, sources, overlays, result, None)
    }
    pub fn issue_observed(
        &self,
        server: &str,
        input: &Request,
        sources: generation::Observation,
        overlays: u64,
        result: &Value,
        affected: Option<affected::Snapshot>,
    ) -> Result<protocol::AnalysisContext, String> {
        let files = affected
            .as_ref()
            .map_or_else(Vec::new, affected::Snapshot::paths);
        let revisions = affected
            .as_ref()
            .map_or_else(Vec::new, affected::Snapshot::revisions);
        let identities = affected
            .as_ref()
            .map_or_else(Vec::new, affected::Snapshot::identities);
        let affected_digest = if files.is_empty() {
            String::new()
        } else {
            affected.as_ref().unwrap().digest()
        };
        let origin = protocol::actions::origin_fingerprint(input)?;
        let request = request_fingerprint(input, &origin)?;
        let context = Context {
            version: protocol::VERSION,
            server: server.into(),
            origin,
            request,
            result: openwebide_core::plugins::content_digest(
                &serde_json::to_vec(result).map_err(|e| e.to_string())?,
            ),
            sources: sources.revision,
            overlays,
            issued: self.started.elapsed().as_secs(),
            affected: affected_digest,
        };
        let body = serde_json::to_vec(&context).map_err(|e| e.to_string())?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).unwrap();
        mac.update(&body);
        let context = protocol::AnalysisContext {
            files,
            identities,
            revisions,
            server: server.into(),
            result_digest: context.result,
            proof: format!(
                "{}.{}",
                URL_SAFE_NO_PAD.encode(&body),
                URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
            ),
        };
        context.validate()?;
        Ok(context)
    }
    pub fn verify_at(
        &self,
        proof: &protocol::AnalysisContext,
        server: &str,
        input: &Request,
        sources: generation::Observation,
        overlays: u64,
        root: &std::path::Path,
    ) -> Result<affected::Snapshot, String> {
        let context = self.verified(proof, server, input, sources, overlays)?;
        Self::observe_files(proof, &context, root)
    }
    fn observe_files(
        proof: &protocol::AnalysisContext,
        context: &Context,
        root: &std::path::Path,
    ) -> Result<affected::Snapshot, String> {
        if proof.files.is_empty() {
            if !context.affected.is_empty()
                || !proof.identities.is_empty()
                || !proof.revisions.is_empty()
            {
                return Err("The original affected-file path set changed".into());
            }
            return Ok(affected::Snapshot::default());
        }
        affected::Files::verify(
            root,
            &proof.files,
            &proof.identities,
            &proof.revisions,
            &context.affected,
        )
    }
    #[cfg(test)]
    pub fn verify(
        &self,
        proof: &protocol::AnalysisContext,
        server: &str,
        input: &Request,
        sources: generation::Observation,
        overlays: u64,
    ) -> Result<(), String> {
        self.verified(proof, server, input, sources, overlays)
            .map(|_| ())
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn verify_original(
        &self,
        proof: &protocol::AnalysisContext,
        server: &str,
        input: &Request,
        sources: generation::Observation,
        overlays: u64,
        root: &std::path::Path,
    ) -> Result<affected::Snapshot, String> {
        let context = self.verified(proof, server, input, sources, overlays)?;
        if context.request != request_fingerprint(input, &context.origin)? {
            return Err("The original language analysis request changed".into());
        }
        Self::observe_files(proof, &context, root)
    }
    fn verified(
        &self,
        proof: &protocol::AnalysisContext,
        server: &str,
        input: &Request,
        sources: generation::Observation,
        overlays: u64,
    ) -> Result<Context, String> {
        proof.validate()?;
        let stale = || {
            "The language analysis context changed or expired; request the result again".to_string()
        };
        if proof.server != server {
            return Err(stale());
        }
        let (body, signature) = proof.proof.split_once('.').ok_or_else(stale)?;
        let body = URL_SAFE_NO_PAD.decode(body).map_err(|_| stale())?;
        let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| stale())?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).unwrap();
        mac.update(&body);
        mac.verify_slice(&signature).map_err(|_| stale())?;
        let context: Context = serde_json::from_slice(&body).map_err(|_| stale())?;
        let now = self.started.elapsed().as_secs();
        if context.version != protocol::VERSION
            || context.server != server
            || context.result != proof.result_digest
            || context.origin != protocol::actions::origin_fingerprint(input)?
            || context.sources != sources.revision
            || context.overlays != overlays
            || context.issued > now
            || now.saturating_sub(context.issued) > LIFETIME
        {
            return Err(stale());
        }
        Ok(context)
    }
}

fn request_fingerprint(input: &Request, origin: &str) -> Result<String, String> {
    // The authenticated origin already binds source_digest and all document
    // fields. Avoid serializing another escaped copy of the source here.
    Ok(openwebide_core::plugins::content_digest(
        &serde_json::to_vec(&(input.version, input.request, origin, &input.query))
            .map_err(|error| error.to_string())?,
    ))
}

#[cfg(test)]
mod tests;
