//! Embedded languages use the same selected-provider policy in both host modes.
use super::*;

pub(super) const MAX_EMBEDDING_DEPTH: usize = 8;

impl<H: PluginActionHost> LanguageService<H> {
    pub async fn analyze(
        &self,
        document: LanguageDocumentRequest,
        now_ms: u64,
    ) -> Result<LanguageOutput, String> {
        let source = Arc::clone(&document.source);
        let root_document = document.document.clone();
        let path = document.path.clone();
        let revision = document.revision;
        let tab_width = document.tab_width;
        let mut output = self.analyze_document(document, now_ms).await?;
        let LanguageOutput::Analyzed(response) = &mut output else {
            return Ok(output);
        };
        let Some(analysis) = &mut response.analysis else {
            return Ok(output);
        };
        let composition = LanguageComposition::new(&source);
        let registry = LanguageRegistry::from_bindings(&self.bindings);
        let scopes = analysis
            .structure
            .as_ref()
            .map(|structure| structure.scopes.clone())
            .unwrap_or_default();
        let mut pending = Vec::new();
        for (index, scope) in scopes.into_iter().enumerate().rev() {
            if index % 16 == 0 {
                self.host.host.yield_task().await;
                if !self.host.is_current() {
                    return Err("Language service context changed".into());
                }
            }
            composition.replace(analysis, &scope, None)?;
            pending.push((scope, 1, format!("{root_document}/{index}")));
        }
        let mut embedded = EmbeddedDocuments {
            request: response.request,
            ..Default::default()
        };
        let mut bytes = 0_usize;
        let mut count = 0;
        while let Some((mut scope, depth, identity)) = pending.pop() {
            if !self.host.is_current() {
                return Err("Language service context changed".into());
            }
            count += 1;
            if count % 16 == 0 {
                self.host.host.yield_task().await;
            }
            bytes = bytes.saturating_add(scope.range.end - scope.range.start);
            // Regions are already plain. Stop delegating at the shared work
            // budget, preserving the current source and outer language results.
            if depth > MAX_EMBEDDING_DEPTH
                || count > MAX_RECORDS
                || bytes > MAX_EMBEDDING_DEPTH * MAX_SOURCE_BYTES
            {
                continue;
            }
            let LanguageSelection::Selected(provider) =
                registry.select("", "", Some(&scope.language), &self.preferences)
            else {
                continue;
            };
            scope.language = provider.definition.id.clone();
            if !(provider.definition.features.syntax
                || provider.definition.features.folding
                || provider.definition.features.structure)
            {
                // Configuration-only providers can still own embedded editing.
                composition.replace(analysis, &scope, None)?;
                continue;
            }
            let body = &source[scope.range.start..scope.range.end];
            let service = self.for_provider(provider.clone());
            let embedded_document = format!(
                "embedded:{}",
                source_digest(&format!("{identity}/{}", provider.namespace))
            );
            embedded
                .documents
                .insert(embedded_document.clone(), provider.namespace.clone());
            let result = service
                .analyze_document(
                    LanguageDocumentRequest {
                        document: embedded_document,
                        path: path.clone(),
                        revision,
                        source: Arc::new(body.to_string()),
                        tab_width,
                    },
                    now_ms,
                )
                .await;
            if !self.host.is_current() {
                return Err("Language service context changed".into());
            }
            let child = match result {
                Ok(LanguageOutput::Analyzed(response)) => response.analysis,
                _ => None,
            };
            let Some(mut child) = child else { continue };
            let scopes = child
                .structure
                .as_ref()
                .map(|structure| structure.scopes.clone())
                .unwrap_or_default();
            let child_composition = LanguageComposition::new(body);
            for (index, nested) in scopes.iter().enumerate() {
                if index % 16 == 0 {
                    self.host.host.yield_task().await;
                    if !self.host.is_current() {
                        return Err("Language service context changed".into());
                    }
                }
                child_composition.replace(&mut child, nested, None)?;
            }
            composition.replace(analysis, &scope, Some(&child))?;
            for (index, nested) in scopes.into_iter().enumerate().rev() {
                pending.push((
                    LanguageScope {
                        language: nested.language,
                        range: TextRange {
                            start: scope.range.start + nested.range.start,
                            end: scope.range.start + nested.range.end,
                        },
                    },
                    depth + 1,
                    format!("{identity}/{index}"),
                ));
            }
        }
        validate_analysis(analysis, &source, &response.language)?;
        let removed = {
            let mut documents = self.embedded_documents.lock().unwrap();
            let mut removed = documents
                .remove(&root_document)
                .unwrap_or_default()
                .documents;
            removed.retain(|document, _| !embedded.documents.contains_key(document));
            documents.insert(root_document, embedded);
            while documents.len() > 4 {
                let oldest = documents
                    .iter()
                    .min_by_key(|(_, entry)| entry.request)
                    .map(|(name, _)| name.clone());
                if let Some(oldest) = oldest {
                    removed.extend(documents.remove(&oldest).unwrap().documents);
                }
            }
            removed
        };
        // Removed bodies release host stores as well as client-side delta bases.
        let _ = self.close_embedded(removed, now_ms).await;
        if !self.host.is_current() {
            return Err("Language service context changed".into());
        }
        Ok(output)
    }
}
