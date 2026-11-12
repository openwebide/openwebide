//! Retain source inventories without retaining a process or server-issued IDs.
use super::*;

impl Documents {
    pub(in crate::languages::host) fn require_closed_prefetch(
        &self,
        input: &Request,
        path: &Path,
    ) -> Result<(), String> {
        if self.documents.contains_key(&input.document)
            || self
                .documents
                .values()
                .any(|document| document.path == path)
        {
            return Err(
                "Closed-file prefetch cannot replace or close an existing open document".into(),
            );
        }
        Ok(())
    }

    /// Called only while the admitting closed-file query still owns this bank.
    /// Keep its request watermark; retirement is not another client request.
    pub(in crate::languages::host) fn retire_prefetch(
        &mut self,
        document: &str,
    ) -> Result<Option<String>, String> {
        let Some(document) = self.documents.remove(document) else {
            return Ok(None);
        };
        self.overlay_revision = self
            .overlay_revision
            .checked_add(1)
            .ok_or("Language overlay revision exhausted during prefetch retirement")?;
        Ok(Some(document.uri))
    }

    pub(super) fn admit(&mut self, input: &Request) -> Result<(), String> {
        if let Some((request, revision, _)) = self.watermarks.get(&input.document)
            && (input.request <= *request || input.revision < *revision)
        {
            return Err(
                "Language document request is stale or was superseded, including after close"
                    .into(),
            );
        }
        let limit = prefetch::request_limit(input);
        if !self.watermarks.contains_key(&input.document) && self.watermarks.len() >= limit {
            self.watermarks.retain(|id, (_, _, seen)| {
                self.documents.contains_key(id) || seen.elapsed() < 2 * REQUEST_TIMEOUT
            });
            if self.watermarks.len() >= limit {
                return Err("Too many recently closed language documents; retry after current requests expire".into());
            }
        }
        self.watermarks.insert(
            input.document.clone(),
            (input.request, input.revision, Instant::now()),
        );
        Ok(())
    }
    pub(super) fn remove_closed(&mut self, input: &Request) -> Result<Option<Document>, String> {
        if self.documents.get(&input.document).is_some_and(|document| {
            input.request <= document.request || input.revision < document.revision
        }) {
            return Err("Language close request is stale".into());
        }
        let document = self.documents.remove(&input.document);
        if document.is_some() {
            self.overlay_revision = self
                .overlay_revision
                .checked_add(1)
                .ok_or("Language overlay revision exhausted")?;
        }
        Ok(document)
    }
    pub(in crate::languages::host) fn close_retained(
        &mut self,
        input: &Request,
    ) -> Result<(), String> {
        self.admit(input)?;
        self.remove_closed(input)?;
        Ok(())
    }
    pub(in crate::languages::host) fn detach(&mut self) -> (Self, Vec<String>) {
        let version = self.version;
        let mut retained = std::mem::take(self);
        self.version = version;
        let closes = retained
            .documents
            .values_mut()
            .map(|document| {
                document.actions.clear();
                document.completions.clear();
                document.resources.clear();
                document.symbols.clear();
                document.uri.clone()
            })
            .collect();
        (retained, closes)
    }
    pub(in crate::languages::host) fn snapshot(&self) -> Self {
        Self {
            documents: self
                .documents
                .iter()
                .map(|(id, document)| {
                    (
                        id.clone(),
                        Document {
                            path: document.path.clone(),
                            uri: document.uri.clone(),
                            language: document.language.clone(),
                            revision: document.revision,
                            request: document.request,
                            digest: document.digest.clone(),
                            source: document.source.clone(),
                            version: document.version,
                            overlay_revision: document.overlay_revision,
                            actions: BTreeMap::new(),
                            completions: BTreeMap::new(),
                            resources: BTreeMap::new(),
                            symbols: BTreeMap::new(),
                        },
                    )
                })
                .collect(),
            version: self.version,
            overlay_revision: self.overlay_revision,
            analysis_generation: None,
            watermarks: self.watermarks.clone(),
        }
    }
    pub(in crate::languages::host) async fn restore(
        &mut self,
        session: &Session,
    ) -> Result<(), String> {
        // Validate the complete bank before sending any overlay. Path grants
        // and parent links can have changed while its client was detached.
        for document in self.documents.values() {
            tokio::task::yield_now().await;
            let relative = results::relative(&session.root, &document.path)
                .ok_or("A retained language document is outside its project")?;
            if source_path(&session.root, &relative).await? != document.path {
                return Err("A retained language document changed through a symbolic link".into());
            }
        }
        self.version = self.version.max(session.documents.lock().await.version);
        for document in self.documents.values_mut() {
            tokio::task::yield_now().await;
            self.version = self
                .version
                .checked_add(2)
                .ok_or("Language document version exhausted")?;
            document.version = self.version;
            document.overlay_revision = self.overlay_revision;
            // Versions increase across physical handoffs as well as edits.
            // Delayed explicit diagnostics from the previous client cannot fit.
            session.diagnostics.lock().unwrap().insert(
                document.uri.clone(),
                Diagnostics {
                    version: document.version,
                    minimum_version: document.version - 1,
                    raw: vec![],
                    ready: false,
                },
            );
            Self::open_document(session, document).await?;
        }
        Ok(())
    }
}
