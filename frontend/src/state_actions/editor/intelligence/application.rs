//! Retained analysis owns its original provider, source and normalized result.
//! Applying an edit never refreshes an old result through new synchronization.
use super::*;

pub(super) struct AnalysisResult {
    pub snapshot: Snapshot,
    analysis: openwebide_agent::plugins::languages::LanguageAnalysis,
    workspace_files:
        Option<Result<Arc<crate::workspace_transactions::review::WorkspaceReviewFiles>, String>>,
}

pub(super) struct PreparedEdit {
    pub edits: Vec<TextEdit>,
    pub snippet: Option<openwebide_core::editor::snippet::Snippet>,
    pub suggest: bool,
}

impl AnalysisResult {
    pub fn capture(
        snapshot: &Snapshot,
        query: &Query,
        client: String,
        document: LanguageDocumentRequest,
        service: ProjectLanguageService,
        response: &Response,
        workspace_files: Option<
            Result<Arc<crate::workspace_transactions::review::WorkspaceReviewFiles>, String>,
        >,
    ) -> Result<Option<Arc<Self>>, String> {
        // Opaque completion/action lists are resolved on a fresh request before
        // applying. Read-only bodies need no additional retained result copy.
        if !query.requires_analysis_context() || matches!(query, Query::ValidateAnalysis { .. }) {
            return Ok(None);
        }
        service
            .retain_analysis(&client, document, query.clone(), response)?
            .map(|analysis| {
                Self::from_analysis(snapshot.clone(), analysis, workspace_files).map(Arc::new)
            })
            .transpose()
    }

    pub fn from_analysis(
        snapshot: Snapshot,
        analysis: openwebide_agent::plugins::languages::LanguageAnalysis,
        workspace_files: Option<
            Result<Arc<crate::workspace_transactions::review::WorkspaceReviewFiles>, String>,
        >,
    ) -> Result<Self, String> {
        let document = analysis.document();
        if document.path != snapshot.presentation.path
            || document.revision != snapshot.revision
            || document.source.as_str() != snapshot.source.as_str()
        {
            return Err("The analysis no longer matches the editor document".into());
        }
        Ok(Self {
            snapshot,
            analysis,
            workspace_files,
        })
    }

    pub fn language_review(
        &self,
        plan: &openwebide_core::workspace_transaction::WorkspacePlan,
        editor: &openwebide_core::workspace_transaction::EditorRecoveryPatch,
    ) -> Result<openwebide_core::workspace_transaction::LanguageReview, String> {
        self.analysis.language_review(plan, editor)
    }

    pub async fn validate(&self) -> Result<(), String> {
        self.analysis.validate().await
    }
    pub fn workspace_files(
        &self,
    ) -> Result<Arc<crate::workspace_transactions::review::WorkspaceReviewFiles>, String> {
        self.workspace_files.clone().unwrap_or_else(|| {
            Err("Request fresh code actions before reviewing file operations".into())
        })
    }

    pub fn action(
        &self,
        review: &protocol::actions::ActionReview,
    ) -> Result<(protocol::Request, RegisteredLanguage), String> {
        if !matches!(self.analysis.query(), Query::ReviewAction { .. })
            || serde_json::to_value(review).map_err(|error| error.to_string())?
                != *self.analysis.result()
        {
            return Err("The action review changed. Request it again.".into());
        }
        Ok((self.analysis.origin(), self.analysis.provider().clone()))
    }

    pub fn workspace_edit(&self, edit: &results::WorkspaceEdit) -> Result<(), String> {
        let expected = match self.analysis.query() {
            Query::Rename { .. } | Query::ResolveAction { .. } => {
                serde_json::from_value::<results::WorkspaceEdit>(self.analysis.result().clone())
                    .map_err(|_| "Invalid retained workspace edit")?
            }
            Query::ReviewAction { .. } => {
                let review = serde_json::from_value::<protocol::actions::ActionReview>(
                    self.analysis.result().clone(),
                )
                .map_err(|_| "Invalid retained action review")?;
                if review.command.is_some() {
                    return Err("Review the complete action with its server command".into());
                }
                review.edit
            }
            _ => return Err("Request a fresh workspace review before applying these edits".into()),
        };
        if &expected != edit {
            return Err("The workspace edit changed. Request it again.".into());
        }
        Ok(())
    }

    pub fn formatting(&self, edits: Vec<TextEdit>) -> Result<PreparedEdit, String> {
        if !matches!(
            self.analysis.query(),
            Query::Formatting { .. } | Query::RangeFormatting { .. }
        ) {
            return Err("The retained result is not a formatting edit".into());
        }
        let result = if self.analysis.result().is_null() && edits.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::to_value(&edits).map_err(|error| error.to_string())?
        };
        if &result != self.analysis.result() {
            return Err("The formatting result changed. Request it again.".into());
        }
        Ok(PreparedEdit {
            edits,
            snippet: None,
            suggest: false,
        })
    }

    pub fn completion(&self, item: &serde_json::Value) -> Result<PreparedEdit, String> {
        if !matches!(self.analysis.query(), Query::ResolveCompletion { .. })
            || item != self.analysis.result()
        {
            return Err("The completion result changed. Request it again.".into());
        }
        let completion = prepare_completion(&self.snapshot, item)?;
        Ok(PreparedEdit {
            edits: completion.edits,
            snippet: completion.snippet,
            suggest: completion.suggest,
        })
    }
}
