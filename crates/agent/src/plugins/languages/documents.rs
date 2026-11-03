//! Full/delta synchronization, one resync and bounded acknowledged sources.
use super::*;

pub struct LanguageDocumentRequest {
    pub document: String,
    pub path: String,
    pub revision: u64,
    pub source: Arc<String>,
    pub tab_width: usize,
}
#[derive(Clone)]
pub(super) struct ClientDocument {
    source: Arc<String>,
    revision: u64,
    request: u64,
    path: String,
    tab_width: usize,
}
impl<H: PluginActionHost> LanguageService<H> {
    pub fn provider(&self) -> &RegisteredLanguage {
        &self.provider
    }
    pub fn definitions(&self) -> &[LanguageDefinition] {
        &self
            .bindings
            .iter()
            .find(|binding| binding.prepared.storage_namespace() == self.provider.plugin_namespace)
            .expect("selected binding")
            .prepared
            .manifest
            .contributions
            .languages
    }

    pub async fn analyze(
        &self,
        document: LanguageDocumentRequest,
        now_ms: u64,
    ) -> Result<LanguageOutput, String> {
        let LanguageDocumentRequest {
            document,
            path,
            revision,
            source,
            tab_width,
        } = document;
        if source.len() > MAX_SOURCE_BYTES {
            return Err("Language source exceeds its byte limit".into());
        }
        let request = self
            .sequence
            .fetch_add(1, Ordering::Relaxed)
            .checked_add(1)
            .ok_or("Language request sequence exhausted")?;
        let previous = self
            .host
            .state
            .lock()
            .unwrap()
            .documents
            .get(&document)
            .cloned()
            .filter(|previous| {
                previous.path == path
                    && previous.tab_width == tab_width
                    && previous.revision < revision
            });
        let change = if let Some(previous) = &previous {
            let mut change = openwebide_core::editor::TextChangePreparation::default();
            while !change.is_complete() {
                if !self.host.is_current() {
                    return Err("Language service context changed".into());
                }
                change.advance(&previous.source, &source, 16 * 1024);
                if !change.is_complete() {
                    self.host.host.yield_task().await;
                }
            }
            let edit = change.change();
            SourceChange::Replace {
                base_revision: previous.revision,
                start: edit.map_or(0, |edit| edit.range.start),
                end: edit.map_or(0, |edit| edit.range.end),
                text: edit.map_or_else(String::new, |edit| {
                    source[edit.range.start..edit.new_end].to_string()
                }),
            }
        } else {
            SourceChange::Full {
                text: source.as_ref().clone(),
            }
        };
        let mut input = LanguageInput::Analyze(AnalysisRequest {
            version: VERSION,
            document: document.clone(),
            request,
            revision,
            language: self.provider.definition.id.clone(),
            path: path.clone(),
            source_digest: source_digest(&source),
            source: change,
            tab_width,
        });
        let mut output = self.request(&input, Some(&source), now_ms).await?;
        if matches!(&output, LanguageOutput::Analyzed(response) if response.status == AnalysisStatus::NeedsSource)
        {
            let LanguageInput::Analyze(request) = &mut input else {
                unreachable!()
            };
            request.source = SourceChange::Full {
                text: source.as_ref().clone(),
            };
            output = self.request(&input, Some(&source), now_ms).await?;
            if matches!(&output, LanguageOutput::Analyzed(response) if response.status == AnalysisStatus::NeedsSource)
            {
                self.host
                    .state
                    .lock()
                    .unwrap()
                    .failed(self.host.host.now_ms().unwrap_or(now_ms), false);
                return Err("Language provider could not synchronize its document source".into());
            }
        }
        if matches!(&output, LanguageOutput::Analyzed(response) if matches!(response.status, AnalysisStatus::Ready { .. }))
        {
            let mut state = self.host.state.lock().unwrap();
            if state
                .documents
                .get(&document)
                .is_none_or(|previous| previous.request < request)
            {
                state.documents.insert(
                    document,
                    ClientDocument {
                        source,
                        revision,
                        request,
                        path,
                        tab_width,
                    },
                );
            }
            while state.documents.len() > 4
                || state
                    .documents
                    .values()
                    .map(|document| document.source.len())
                    .sum::<usize>()
                    > 8 * 1024 * 1024
            {
                let oldest = state
                    .documents
                    .iter()
                    .min_by_key(|(_, document)| document.request)
                    .map(|(id, _)| id.clone());
                if let Some(oldest) = oldest {
                    state.documents.remove(&oldest);
                } else {
                    break;
                }
            }
        }
        Ok(output)
    }

    pub async fn close(&self, document: &str, now_ms: u64) -> Result<(), String> {
        self.host.state.lock().unwrap().documents.remove(document);
        self.request(
            &LanguageInput::Close {
                version: VERSION,
                document: document.into(),
            },
            None,
            now_ms,
        )
        .await
        .map(|_| ())
    }
}
