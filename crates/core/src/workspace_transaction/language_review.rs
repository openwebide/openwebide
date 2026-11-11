//! Retained language evidence belongs to the exact durable transaction. It is
//! not a grant: a native worker must validate it on its original live service.
use super::{EditorRecoveryPatch, WorkspacePlan};
use crate::plugins::{
    PreparedPlugin,
    languages::{
        MAX_REQUEST_ENVELOPE_BYTES,
        intelligence::{
            self as protocol, AnalysisContext, Query, Request,
            actions::ActionReview,
            results::{WorkspaceChange, WorkspaceEdit},
        },
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Preserve the existing source/package envelope and normalized-result budgets,
/// plus bounded proof, revision and transaction framing. This is not scope data.
pub const MAX_LANGUAGE_REVIEW_BYTES: usize =
    MAX_REQUEST_ENVELOPE_BYTES + protocol::MAX_RESULT_BYTES + 64 * 1024;

/// Compact framing only, not native authentication or current authority.
pub fn language_review_admission<'a>(
    digest: Option<&'a str>,
    grant: Option<&'a str>,
) -> Result<Option<(&'a str, &'a str)>, String> {
    match (digest, grant) {
        (None, None) => Ok(None),
        (Some(digest), Some(grant))
            if super::companion::fingerprint(digest) && super::valid_id(grant) =>
        {
            Ok(Some((digest, grant)))
        }
        _ => Err("Invalid or incomplete language review admission".into()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageReview {
    pub prepared: PreparedPlugin,
    pub binding_revision: i64,
    pub input: Request,
    pub context: AnalysisContext,
    pub result: serde_json::Value,
    /// Both resource snapshots and the complete editor patch are bound here.
    pub transaction_digest: String,
}
impl LanguageReview {
    /// Identity acknowledged by the trusted native verifier. This includes the
    /// original proof and complete result, not just the client's plan digest.
    pub fn fingerprint(&self) -> Result<String, String> {
        digest(self)
    }
    /// Reproduce the normalized result using the same planner as editor review.
    /// The caller supplies original-folder reads and holds native exclusion.
    pub async fn reproduce(
        &self,
        files: &impl crate::workspace_entries::WorkspaceEntries,
        plan: &WorkspacePlan,
        editor: &EditorRecoveryPatch,
        current: impl Fn() -> bool,
    ) -> Result<(), String> {
        use crate::{
            editor::{EditorRecovery, EditorRecoveryFile, EditorRecoveryRecord, RecoveryScroll},
            plugins::languages::intelligence::workspace_edit::{WorkspaceDocument, WorkspaceDraft},
        };
        self.validate_transaction(plan, editor)?;
        let before = EditorRecoveryRecord {
            revision: editor.revision,
            state: EditorRecovery {
                format: 1,
                root: Some(editor.root.clone()),
                selected: editor.selected_before.clone(),
                files: editor
                    .order_before
                    .iter()
                    .map(|path| {
                        editor
                            .before
                            .iter()
                            .find(|file| file.path == *path)
                            .cloned()
                            .unwrap_or_else(|| EditorRecoveryFile {
                                path: path.clone(),
                                document: None,
                                scroll: RecoveryScroll::default(),
                                read_only: false,
                            })
                    })
                    .collect(),
            },
        };
        before.state.validate()?;
        let mut documents = std::collections::BTreeMap::new();
        for file in &editor.before {
            if let Some(source) = &file.document {
                let mut document = WorkspaceDocument::new(&file.path, source.restore()?);
                document.read_only = file.read_only;
                documents.insert(file.path.clone(), document);
            }
        }
        let mut draft = if plan.version == 2 {
            WorkspaceDraft::prepare_native(
                files,
                plan.root.clone(),
                plan.operations.clone(),
                documents,
                editor.order_before.clone(),
                editor.selected_before.clone(),
                &current,
            )
            .await?
        } else {
            WorkspaceDraft::prepare(
                files,
                plan.root.clone(),
                plan.operations.clone(),
                documents,
                editor.order_before.clone(),
                editor.selected_before.clone(),
                &current,
            )
            .await?
        };
        if draft.plan != *plan {
            return Err("The original folder differs from the reviewed resource plan".into());
        }
        for change in self.workspace_edit()?.ordered()? {
            if !current() {
                return Err("The original language review changed during verification".into());
            }
            match change {
                WorkspaceChange::File { operation } => draft.resource(operation)?,
                WorkspaceChange::Text { file } => {
                    draft.document(files, &file.path, &current).await?;
                    draft.text(file)?;
                }
            }
        }
        let after = draft.recovery(&before.state)?;
        if draft.recovery_patch(&before, &after)? != *editor || !current() {
            return Err("The editor patch differs from the original language result".into());
        }
        Ok(())
    }
    pub fn validate_provider(
        &self,
        registry: &crate::plugins::languages::LanguageRegistry,
        preferences: &crate::plugins::languages::LanguagePreferences,
    ) -> Result<(), String> {
        self.validate()?;
        self.input.validate()?;
        protocol::validate_review_provider(
            &self.prepared,
            self.binding_revision,
            &self.input,
            registry,
            preferences,
        )
    }
    pub fn validate(&self) -> Result<(), String> {
        self.prepared
            .validate()
            .map_err(|error| error.to_string())?;
        self.input.validate_archived()?;
        protocol::declared_server(&self.prepared, &self.input.language)?;
        self.context.validate_result(&self.result)?;
        if self.binding_revision <= 0
            || !super::companion::fingerprint(&self.transaction_digest)
            || serde_json::to_vec(&self.result)
                .map_err(|error| error.to_string())?
                .len()
                > protocol::MAX_RESULT_BYTES
            || serde_json::to_vec(self)
                .map_err(|error| error.to_string())?
                .len()
                > MAX_LANGUAGE_REVIEW_BYTES
        {
            return Err("Invalid or oversized retained language review".into());
        }
        self.workspace_edit()?;
        Ok(())
    }
    /// Keep the complete action body, including command arguments, in the
    /// authenticated result. Consumers cannot substitute its edit-only form.
    pub fn workspace_edit(&self) -> Result<WorkspaceEdit, String> {
        let edit = match &self.input.query {
            Query::Rename { .. } | Query::ResolveAction { .. } => {
                serde_json::from_value(self.result.clone())
                    .map_err(|_| "Invalid retained workspace edit")?
            }
            Query::ReviewAction { id } => {
                let review: ActionReview = serde_json::from_value(self.result.clone())
                    .map_err(|_| "Invalid retained action review")?;
                review.validate()?;
                if review.command.as_ref().is_some_and(|command| {
                    command.id != *id || command.server != self.context.server
                }) {
                    return Err(
                        "The retained command changed its original service or action".into(),
                    );
                }
                review.edit
            }
            _ => return Err("This language result is not a workspace review".into()),
        };
        edit.validate()?;
        Ok(edit)
    }
    /// Check durable format and consistency, not native freshness or permission.
    /// Admission must also verify the original service proof and reproduce the
    /// normalized edits against the transaction's captured before state.
    pub fn validate_transaction(
        &self,
        plan: &WorkspacePlan,
        editor: &EditorRecoveryPatch,
    ) -> Result<(), String> {
        self.validate()?;
        if self.transaction_digest != Self::transaction_digest(plan, editor)? {
            return Err("The language review belongs to a different workspace transaction".into());
        }
        let operations = self
            .workspace_edit()?
            .ordered()?
            .into_iter()
            .filter_map(|change| match change {
                WorkspaceChange::File { operation } => Some(operation),
                WorkspaceChange::Text { .. } => None,
            })
            .collect::<Vec<_>>();
        if operations != plan.operations {
            return Err("The resource plan differs from the original language result".into());
        }
        Ok(())
    }
    /// Stream the existing bounded structures rather than allocating another
    /// serialized copy of their potentially large snapshots and drafts.
    pub fn transaction_digest(
        plan: &WorkspacePlan,
        editor: &EditorRecoveryPatch,
    ) -> Result<String, String> {
        plan.validate()?;
        editor.validate()?;
        digest(&(plan, editor))
    }
}
fn digest(value: &impl Serialize) -> Result<String, String> {
    struct Writer(Sha256);
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer(Sha256::new());
    serde_json::to_writer(&mut writer, value).map_err(|error| error.to_string())?;
    Ok(writer
        .0
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests;
