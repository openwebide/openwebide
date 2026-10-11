//! Shared publication/coalescing policy above the thin worker transport.
use super::EditorActions;
use crate::state::workspace::{EditorFallbackPaint, EditorSyntaxScope, PreparedEditorSyntax};
use leptos::prelude::*;
use openwebide_core::editor::{
    SyntaxAnalysis, SyntaxReply, SyntaxReplyReception, SyntaxRequest, SyntaxRequestPublication,
    SyntaxStatus,
};
use std::{cell::Cell, rc::Rc};

pub(super) async fn publish_syntax_request(
    mut publication: SyntaxRequestPublication,
    mut current: impl FnMut() -> bool,
) -> Option<String> {
    loop {
        if !current() {
            return None;
        }
        if publication.advance(openwebide_core::editor::SYNTAX_REQUEST_BATCH_BYTES) {
            return current().then(|| publication.finish()).flatten();
        }
        crate::util::yield_task().await;
    }
}

async fn receive_syntax_reply(
    mut reception: SyntaxReplyReception<'_>,
    mut current: impl FnMut() -> bool,
) -> Option<(SyntaxStatus, Option<std::sync::Arc<SyntaxAnalysis>>)> {
    loop {
        if !current() {
            return None;
        }
        if reception.advance() {
            return current().then(|| reception.finish()).flatten();
        }
        crate::util::yield_task().await;
    }
}

#[derive(Clone)]
struct PublishedSyntax {
    scope: EditorSyntaxScope,
    ticket: u32,
    analysis: std::sync::Arc<openwebide_core::editor::SyntaxAnalysis>,
}

impl EditorActions {
    pub(super) fn syntax_scope(self) -> Option<EditorSyntaxScope> {
        let Some(key) = self.key() else {
            self.workspace.editor_syntax_scope.set(None);
            return None;
        };
        let epoch = self.workspace.pending_epoch.get_untracked();
        let read_revision = self.workspace.editor_read_revision.get_untracked();
        let account_generation = self.auth.map_or(0, |auth| auth.generation.get_untracked());
        let tab_width = self.rules_untracked().indentation.tab_width();
        let source_revision = self.workspace.editor_source_revision.get_untracked();
        let source = self.workspace.content.with_untracked(|source| {
            self.workspace.editor_syntax_scope.with_untracked(|cached| {
                cached
                    .as_ref()
                    .filter(|cached| {
                        cached.key == key
                            && cached.epoch == epoch
                            && cached.read_revision == read_revision
                            && cached.account_generation == account_generation
                            && (cached.source_revision == source_revision
                                || cached.source.as_ref() == source.as_str())
                    })
                    .map_or_else(|| source.shared(), |cached| cached.source.clone())
            })
        });
        let scope = EditorSyntaxScope {
            key,
            source,
            source_revision,
            epoch,
            read_revision,
            account_generation,
            tab_width,
        };
        self.workspace.editor_syntax_scope.set(Some(scope.clone()));
        Some(scope)
    }
    pub(super) fn syntax_scope_current(self, scope: &EditorSyntaxScope) -> bool {
        !self.workspace.content.is_disposed()
            && self.key().as_ref() == Some(&scope.key)
            && self.workspace.pending_epoch.get_untracked() == scope.epoch
            && self.workspace.editor_read_revision.get_untracked() == scope.read_revision
            && self.auth.map_or(0, |auth| auth.generation.get_untracked())
                == scope.account_generation
            && self.rules_untracked().indentation.tab_width() == scope.tab_width
            && self.workspace.editor_source_revision.get_untracked() == scope.source_revision
            && (self.syntax_source_retained(scope)
                || self
                    .workspace
                    .content
                    .with_untracked(|source| source.as_str() == scope.source.as_ref()))
    }

    /// Only facade-minted immutable source allocations can skip byte validation.
    fn syntax_source_retained(self, scope: &EditorSyntaxScope) -> bool {
        self.workspace.editor_syntax_scope.with_untracked(|cached| {
            cached.as_ref().is_some_and(|cached| {
                cached.key == scope.key
                    && cached.epoch == scope.epoch
                    && cached.read_revision == scope.read_revision
                    && cached.account_generation == scope.account_generation
                    && cached.source_revision == scope.source_revision
                    && std::sync::Arc::ptr_eq(&cached.source, &scope.source)
            })
        })
    }

    /// Pending worker results do not require a second full-file lexical pass.
    fn worker_syntax_pending(self) -> bool {
        self.workspace.editor_worker_active.get_untracked()
            && self.key().is_some()
            && self
                .workspace
                .editor_preparation
                .with_untracked(|prepared| {
                    prepared
                        .as_ref()
                        .is_none_or(|prepared| !self.syntax_scope_current(&prepared.scope))
                })
    }

    fn fallback_paint(
        self,
    ) -> Option<(bool, std::sync::Arc<openwebide_core::highlight::TokenRows>)> {
        self.workspace
            .editor_fallback_paint
            .with_untracked(|paint| {
                paint
                    .as_ref()
                    .filter(|paint| {
                        self.syntax_scope_current(&paint.scope)
                            && (paint.prepared_source
                                || paint.lexical.is_some()
                                || self.source_line_count().is_some())
                    })
                    .map(|paint| (paint.prepared_source, paint.tokens.clone()))
            })
    }
    /// Reactive progress for the current file, independent of transport choice.
    pub fn syntax_preparation_pending(self) -> bool {
        self.workspace.content.track();
        let scope = self.presentation_scope();
        self.preparation_revision();
        scope.is_some() && self.syntax_is_pending()
    }

    pub fn syntax_is_pending(self) -> bool {
        if self.worker_syntax_pending() {
            return true;
        }
        self.workspace.editor_fallback_active.get_untracked()
            && self.fallback_paint().is_none()
            && (!self.workspace.editor_worker_active.get_untracked()
                || self.syntax_highlights().is_none())
    }

    /// Full-row probes require current source ownership: a completed document
    /// index or cooperative fallback paint. Pending workers retain their bounded
    /// visible-source contract.
    pub fn full_row_paint_ready(self) -> bool {
        !self.workspace.editor_fallback_active.get_untracked()
            || self.worker_syntax_pending()
            || !self.syntax_is_pending()
    }

    /// Completed source ownership can come from token rows or a current index.
    /// Pending borrowed paint remains distinct so its first frame can be immediate.
    pub fn source_paint_complete(self, has_token_rows: bool) -> bool {
        has_token_rows || (!self.syntax_is_pending() && self.source_line_count().is_some())
    }

    /// Keep the previous frame only when current source cannot yet be painted.
    /// Owned retained grammar rows can repaint now: matching rows keep colors,
    /// changed rows borrow current text. Terminal fallback retains its index gate.
    pub fn retain_pending_paint(self, prepared_source: bool, has_rows: bool) -> bool {
        self.syntax_is_pending()
            && self.retained_paint().is_none()
            && (prepared_source
                || (has_rows
                    && (!self.full_row_paint_ready()
                        || (self.key().is_some_and(|key| {
                            openwebide_core::highlight::language_from_path(&key.1)
                                != openwebide_core::highlight::Language::Plain
                        }) && !self.pending_plain_after_rejection()))))
    }

    /// A rejected parser result establishes neutral paint for subsequent edits in
    /// the same document ownership. Retry grammar analysis in the background;
    /// never carry this policy across a read, project, account or rule change.
    fn pending_plain_after_rejection(self) -> bool {
        if !self.worker_syntax_pending() {
            return false;
        }
        self.workspace
            .editor_preparation
            .with_untracked(|prepared| {
                prepared.as_ref().is_some_and(|prepared| {
                    let scope = &prepared.scope;
                    prepared.status == openwebide_core::editor::SyntaxStatus::TooLarge
                        && self.key().as_ref() == Some(&scope.key)
                        && self.workspace.pending_epoch.get_untracked() == scope.epoch
                        && self.workspace.editor_read_revision.get_untracked()
                            == scope.read_revision
                        && self.account_generation() == scope.account_generation
                        && self.rules_untracked().indentation.tab_width() == scope.tab_width
                        && self.workspace.editor_source_revision.get_untracked()
                            > scope.source_revision
                })
            })
    }

    /// Initial neutral viewport paint is independent of whole-file fallback tokens.
    /// Wrapped/nonuniform rows and large paint still require complete preparation.
    pub fn viewport_paint_ready(self, source_rows: &[usize]) -> bool {
        let indexed_neutral = self
            .workspace
            .editor_fallback_paint
            .with_untracked(|paint| {
                paint.as_ref().is_some_and(|paint| {
                    !paint.prepared_source
                        && paint.lexical.is_none()
                        && self.syntax_scope_current(&paint.scope)
                })
            });
        if indexed_neutral && self.measured_rows().is_none() {
            // Logical rows permit measurement, not arbitrary native paint. Keep
            // cold oversized/unsupported rows on complete native input until
            // their source-owned geometry is actually available.
            if source_rows.is_empty()
                || source_rows.len() > openwebide_core::editor::MAX_MEASURE_ROWS
            {
                return false;
            }
            return self.projection().is_some_and(|projection| {
                Self::bounded_source_paint_ready(&projection, source_rows)
            }) || self.measured_prefix().is_some_and(|prefix| {
                source_rows.iter().all(|source| {
                    prefix
                        .paint
                        .projection
                        .lines()
                        .binary_search_by_key(source, |line| line.source_line)
                        .is_ok_and(|row| row < prefix.rows.len())
                })
            }) || self.paragraph_coverage().is_some_and(|coverage| {
                coverage
                    .paint
                    .projection
                    .lines()
                    .first()
                    .is_some_and(|line| source_rows == [line.source_line])
            });
        }
        self.full_row_paint_ready() || self.bounded_viewport_paint_ready(source_rows)
    }

    /// Publish an initial neutral frame without awaiting a browser frame callback.
    pub fn initial_viewport_paint_ready(
        self,
        source_rows: &[usize],
        paint: (bool, &openwebide_core::highlight::TokenRows),
    ) -> bool {
        !paint.0 && paint.1.is_empty() && self.bounded_viewport_paint_ready(source_rows)
    }

    /// Current native text can publish retained grammar spans before the next
    /// animation frame when the complete visible paint has a bounded source proof.
    pub fn pending_viewport_paint_ready(
        self,
        source_rows: &[usize],
        paint: &(bool, std::sync::Arc<openwebide_core::highlight::TokenRows>),
    ) -> bool {
        !paint.0
            && self
                .retained_paint()
                .is_some_and(|retained| std::sync::Arc::ptr_eq(&retained, &paint.1))
            && self.bounded_viewport_paint_ready(source_rows)
    }

    fn bounded_viewport_paint_ready(self, source_rows: &[usize]) -> bool {
        if self.is_composing() || self.preferences().word_wrap {
            return false;
        }
        let Some(projection) = self
            .projection()
            .filter(openwebide_core::editor::FoldProjection::has_uniform_rows)
        else {
            return false;
        };
        Self::bounded_source_paint_ready(&projection, source_rows)
    }

    /// Bounded DOM paint can discover wrapped/folded geometry once logical rows
    /// are indexed. It does not publish a native mapping or completed extents.
    fn bounded_source_paint_ready(
        projection: &openwebide_core::editor::FoldProjection,
        source_rows: &[usize],
    ) -> bool {
        if source_rows.is_empty() || source_rows.len() > openwebide_core::editor::MAX_MEASURE_ROWS {
            return false;
        }
        source_rows
            .iter()
            .try_fold(0_usize, |bytes, source| {
                let row = projection
                    .lines()
                    .binary_search_by_key(source, |line| line.source_line)
                    .ok()?;
                bytes
                    .checked_add(projection.line_body(row)?.len())
                    .filter(|bytes| *bytes <= openwebide_core::editor::MAX_MEASURE_BYTES)
            })
            .is_some()
    }

    /// Restore complete native text when a cold viewport cannot be painted yet.
    /// Active composition retains its installed native mapping until commit/cancel.
    pub fn defer_viewport_paint(self, source_rows: &[usize]) -> bool {
        if self.is_composing() {
            return false;
        }
        let retain_native = self.current_native_context().is_some_and(|context| {
            // An empty viewport is a geometry wait, not evidence that the current
            // source-owned native mapping is unusable. Keep it through edits.
            source_rows.is_empty()
                || (self.native_geometry_pending()
                    && source_rows.iter().any(|source| {
                        context
                            .projection()
                            .lines()
                            .binary_search_by_key(source, |line| line.source_line)
                            .is_ok()
                    }))
        });
        // An initial pending window remains usable at its own source rows. Moving
        // to a cold unpaintable viewport outside it requires complete native input.
        if !retain_native {
            self.release_native_context();
        }
        true
    }

    /// Empty pending tokens borrow row bodies from the immutable projection.
    /// Terminal analysis fallback retains the existing contextual lexer.
    pub fn syntax_paint(self) -> (bool, std::sync::Arc<openwebide_core::highlight::TokenRows>) {
        if !self.workspace.editor_worker_active.get_untracked()
            && self.workspace.editor_fallback_active.get_untracked()
        {
            return self
                .fallback_paint()
                .unwrap_or_else(|| (false, std::sync::Arc::new(Vec::new())));
        }
        if let Some(tokens) = self.syntax_highlights() {
            (true, tokens)
        } else if let Some(paint) = self.fallback_paint() {
            paint
        } else if let Some(tokens) = self.retained_paint() {
            (false, tokens)
        } else if self.syntax_is_pending() {
            (false, std::sync::Arc::new(Vec::new()))
        } else {
            (
                false,
                std::sync::Arc::new(self.workspace.content.with_untracked(|source| {
                    openwebide_core::highlight::share_token_rows(
                        openwebide_core::highlight::highlight_lines(
                            &source.replace("\r\n", "\n"),
                            openwebide_core::highlight::Language::Plain,
                        ),
                    )
                })),
            )
        }
    }

    /// Retain grammar colors within the same document ownership. Consumers must
    /// validate each row against current source before using these older tokens.
    fn retained_paint(self) -> Option<std::sync::Arc<openwebide_core::highlight::TokenRows>> {
        if !self.workspace.editor_worker_active.get_untracked() {
            return None;
        }
        self.workspace
            .editor_preparation
            .with_untracked(|prepared| {
                let prepared = prepared.as_ref()?;
                let scope = &prepared.scope;
                (self.key().as_ref() == Some(&scope.key)
                    && self.workspace.pending_epoch.get_untracked() == scope.epoch
                    && self.workspace.editor_read_revision.get_untracked() == scope.read_revision
                    && self.account_generation() == scope.account_generation
                    && self.rules_untracked().indentation.tab_width() == scope.tab_width)
                    .then(|| prepared.analysis.as_ref()?.highlights().cloned())
                    .flatten()
            })
    }

    /// Empty neutral rows borrow the immutable projection. Retained grammar
    /// rows stay provisional and are validated against current source by paint.
    pub fn render_paint(
        self,
        paint: (bool, std::sync::Arc<openwebide_core::highlight::TokenRows>),
    ) -> (bool, std::sync::Arc<openwebide_core::highlight::TokenRows>) {
        if paint.0
            || self
                .retained_paint()
                .is_some_and(|rows| std::sync::Arc::ptr_eq(&rows, &paint.1))
        {
            paint
        } else {
            (false, Self::neutral_tokens())
        }
    }

    fn neutral_tokens() -> std::sync::Arc<openwebide_core::highlight::TokenRows> {
        static NEUTRAL: std::sync::LazyLock<std::sync::Arc<openwebide_core::highlight::TokenRows>> =
            std::sync::LazyLock::new(|| std::sync::Arc::new(Vec::new()));
        NEUTRAL.clone()
    }

    pub fn install_syntax_worker(self) {
        self.install_fallback_paint();
        if !crate::editor_worker::enabled() {
            return;
        }
        let Ok(client) = crate::editor_worker::WorkerClient::new() else {
            return;
        };
        self.install_syntax_transport(Rc::new(client));
    }

    /// Shared cooperative fallback policy above browser task/frame primitives.
    fn install_fallback_paint(self) {
        self.workspace.editor_fallback_active.set(true);
        let ticket = StoredValue::new(0_u64);
        on_cleanup(move || {
            if !self.workspace.editor_fallback_active.is_disposed() {
                self.workspace.editor_fallback_active.set(false);
                self.workspace.editor_fallback_paint.set(None);
            }
        });
        Effect::new(move || {
            self.workspace.content.track();
            self.workspace.open_file.track();
            self.workspace.active_project.track();
            self.workspace.pending_epoch.track();
            self.workspace.editor_read_revision.track();
            self.workspace.editor_worker_active.track();
            self.workspace.editor_preparation_revision.track();
            self.workspace.editor_documents.track();
            if let Some(auth) = self.auth {
                auth.generation.track();
            }
            self.rules();
            ticket.update_value(|value| *value = value.wrapping_add(1));
            let next = ticket.get_value();
            if self.worker_syntax_pending() || self.fallback_paint().is_some() {
                return;
            }
            let scope = self
                .workspace
                .editor_preparation
                .with_untracked(|prepared| {
                    prepared
                        .as_ref()
                        .filter(|prepared| self.syntax_scope_current(&prepared.scope))
                        .map(|prepared| prepared.scope.clone())
                })
                .or_else(|| self.syntax_scope());
            let Some(scope) = scope else {
                return;
            };
            if let Some(tokens) = self.syntax_highlights() {
                if !self.workspace.editor_worker_active.get_untracked() {
                    self.workspace
                        .editor_fallback_paint
                        .set(Some(EditorFallbackPaint {
                            scope,
                            prepared_source: true,
                            tokens,
                            lexical: None,
                        }));
                }
                return;
            }
            // Neutral rendering borrows source through the projection. An already
            // complete current index proves readiness without allocating a second
            // full-file table of plain tokens. Never build an index here: callers
            // without one retain cooperative fallback and its cancellation checks.
            if self.source_line_count().is_some() {
                self.workspace
                    .editor_fallback_paint
                    .set(Some(EditorFallbackPaint {
                        scope,
                        prepared_source: false,
                        tokens: Self::neutral_tokens(),
                        lexical: None,
                    }));
                return;
            }
            let mut lexical = openwebide_core::highlight::LexicalPreparation::for_textarea(
                scope.source.clone(),
                openwebide_core::highlight::Language::Plain,
            );
            if let Some(previous) = self
                .workspace
                .editor_fallback_paint
                .with_untracked(|paint| {
                    paint
                        .as_ref()
                        .filter(|paint| {
                            paint.scope.key == scope.key
                                && paint.scope.account_generation == scope.account_generation
                                && paint.scope.read_revision == scope.read_revision
                                && paint.scope.tab_width == scope.tab_width
                        })
                        .and_then(|paint| paint.lexical.clone())
                })
            {
                lexical = lexical.reuse_cooperative(previous);
            }
            // A small file's initial scan and copy fit in one bounded batch.
            // Larger jobs retain their context and use the ordinary async budget.
            lexical.advance_bounded(
                openwebide_core::highlight::LEXICAL_BATCH_ROWS,
                openwebide_core::highlight::LEXICAL_BATCH_BYTES * 2,
            );
            if lexical.is_complete() {
                let lexical =
                    std::sync::Arc::new(lexical.finish_snapshot().expect("completed lexical job"));
                self.workspace
                    .editor_fallback_paint
                    .set(Some(EditorFallbackPaint {
                        scope,
                        prepared_source: false,
                        tokens: lexical.tokens().clone(),
                        lexical: Some(lexical),
                    }));
                return;
            }
            leptos::task::spawn_local(async move {
                // Let rapid source/read/account changes coalesce before more token allocation.
                crate::util::yield_task().await;
                let current = || {
                    !ticket.is_disposed()
                        && ticket.get_value() == next
                        && self.syntax_scope_current(&scope)
                        && !self.worker_syntax_pending()
                };
                if !current() {
                    return;
                }
                let mut batches = 0_usize;
                let clock = web_sys::window().and_then(|window| window.performance());
                let now = || clock.as_ref().map_or(f64::NAN, web_sys::Performance::now);
                let mut frame_started = now();
                while !lexical.is_complete() {
                    if !current() {
                        return;
                    }
                    lexical.advance_bounded(
                        openwebide_core::highlight::LEXICAL_BATCH_ROWS,
                        openwebide_core::highlight::LEXICAL_BATCH_BYTES,
                    );
                    batches += 1;
                    if !lexical.is_complete() {
                        if openwebide_core::highlight::lexical_frame_due(
                            batches,
                            now() - frame_started,
                        ) {
                            crate::util::yield_frame().await;
                            batches = 0;
                            frame_started = now();
                        } else {
                            crate::util::yield_task().await;
                        }
                    }
                }
                if !current() {
                    return;
                }
                let lexical =
                    std::sync::Arc::new(lexical.finish_snapshot().expect("completed lexical job"));
                self.workspace
                    .editor_fallback_paint
                    .set(Some(EditorFallbackPaint {
                        scope,
                        prepared_source: false,
                        tokens: lexical.tokens().clone(),
                        lexical: Some(lexical),
                    }));
            });
        });
    }

    /// Transport boundary; browser contracts inject deferred replies into the same policy.
    pub fn install_syntax_transport(self, client: Rc<dyn crate::editor_worker::SyntaxTransport>) {
        self.workspace.editor_worker_active.set(true);
        let pending = StoredValue::new(None::<(u32, EditorSyntaxScope)>);
        let ticket = StoredValue::new(0_u32);
        let published = StoredValue::new(None::<PublishedSyntax>);
        let running = Rc::new(Cell::new(false));
        let cleanup = send_wrapper::SendWrapper::new(client.clone());
        on_cleanup(move || {
            cleanup.stop();
            if !self.workspace.editor_worker_active.is_disposed() {
                self.workspace.editor_worker_active.set(false);
                self.workspace.editor_preparation.set(None);
            }
        });
        Effect::new(move || {
            self.workspace.content.track();
            self.workspace.open_file.track();
            self.workspace.active_project.track();
            self.workspace.pending_epoch.track();
            self.workspace.editor_read_revision.track();
            self.workspace.editor_worker_active.track();
            if let Some(auth) = self.auth {
                auth.generation.track();
            }
            self.rules();
            if !self.workspace.editor_worker_active.get_untracked() {
                return;
            }
            ticket.update_value(|value| *value = value.wrapping_add(1));
            let next = ticket.get_value();
            pending.set_value(self.syntax_scope().map(|scope| (next, scope)));
            if running.replace(true) {
                return;
            }
            let client = client.clone();
            let running = running.clone();
            leptos::task::spawn_local(async move {
                while !pending.is_disposed() {
                    let Some((request_ticket, scope)) =
                        pending.try_update_value(Option::take).flatten()
                    else {
                        break;
                    };
                    let mut admission =
                        openwebide_core::editor::SyntaxAdmission::new(scope.source.clone());
                    while admission.status()
                        == openwebide_core::editor::SyntaxAdmissionStatus::Pending
                    {
                        if pending.is_disposed()
                            || ticket.get_value() != request_ticket
                            || !self.syntax_scope_current(&scope)
                        {
                            break;
                        }
                        admission.advance(openwebide_core::highlight::LEXICAL_BATCH_BYTES);
                        if admission.status()
                            == openwebide_core::editor::SyntaxAdmissionStatus::Pending
                        {
                            crate::util::yield_task().await;
                        }
                    }
                    if pending.is_disposed() {
                        break;
                    }
                    if ticket.get_value() != request_ticket || !self.syntax_scope_current(&scope) {
                        continue;
                    }
                    if admission.status()
                        == openwebide_core::editor::SyntaxAdmissionStatus::TooLarge
                    {
                        if ticket.get_value() == request_ticket && self.syntax_scope_current(&scope)
                        {
                            self.workspace
                                .editor_preparation
                                .set(Some(PreparedEditorSyntax {
                                    scope,
                                    status: openwebide_core::editor::SyntaxStatus::TooLarge,
                                    analysis: None,
                                }));
                            self.workspace
                                .editor_preparation_revision
                                .update(|value| *value = value.wrapping_add(1));
                        }
                        continue;
                    }
                    let previous = published.get_value().filter(|previous| {
                        previous.scope.key == scope.key
                            && previous.scope.account_generation == scope.account_generation
                            && previous.scope.read_revision == scope.read_revision
                            && previous.scope.tab_width == scope.tab_width
                    });
                    if previous.is_none() {
                        published.set_value(None);
                    }
                    let document = format!(
                        "{}:{}:{}:{}",
                        scope.account_generation, scope.read_revision, scope.key.0, scope.key.1
                    );
                    let language = openwebide_core::highlight::language_from_path(&scope.key.1);
                    let publication = SyntaxRequest::publication_shared(
                        request_ticket,
                        document.clone(),
                        language,
                        scope.source.clone(),
                        scope.tab_width,
                        previous
                            .as_ref()
                            .map(|previous| (previous.ticket, previous.analysis.clone())),
                    );
                    let Some(message) = publish_syntax_request(publication, || {
                        !pending.is_disposed()
                            && ticket.get_value() == request_ticket
                            && self.syntax_scope_current(&scope)
                    })
                    .await
                    else {
                        continue;
                    };
                    let reply = client.request(request_ticket, message).await;
                    if pending.is_disposed() {
                        break;
                    }
                    if ticket.get_value() != request_ticket || !self.syntax_scope_current(&scope) {
                        continue;
                    }
                    let reception = reply.ok().and_then(|message| {
                        SyntaxReply::prepare_receive_shared(
                            &message,
                            request_ticket,
                            scope.source.clone(),
                            previous
                                .as_ref()
                                .map(|previous| (previous.ticket, previous.analysis.as_ref())),
                        )
                    });
                    let mut result = if let Some(reception) = reception {
                        receive_syntax_reply(reception, || {
                            !pending.is_disposed()
                                && ticket.get_value() == request_ticket
                                && self.syntax_scope_current(&scope)
                        })
                        .await
                    } else {
                        None
                    };
                    if pending.is_disposed() {
                        break;
                    }
                    if ticket.get_value() != request_ticket || !self.syntax_scope_current(&scope) {
                        continue;
                    }
                    if matches!(
                        result,
                        Some((openwebide_core::editor::SyntaxStatus::NeedsSource, None))
                    ) {
                        // Only one resync per scoped request. Recheck ownership after
                        // each await so a missing worker base cannot revive stale text.
                        published.set_value(None);
                        let full = SyntaxRequest::publication_shared(
                            request_ticket,
                            document,
                            language,
                            scope.source.clone(),
                            scope.tab_width,
                            None,
                        );
                        let Some(message) = publish_syntax_request(full, || {
                            !pending.is_disposed()
                                && ticket.get_value() == request_ticket
                                && self.syntax_scope_current(&scope)
                        })
                        .await
                        else {
                            continue;
                        };
                        let reply = client.request(request_ticket, message).await;
                        if pending.is_disposed() {
                            break;
                        }
                        if ticket.get_value() != request_ticket
                            || !self.syntax_scope_current(&scope)
                        {
                            continue;
                        }
                        let reception = reply.ok().and_then(|message| {
                            SyntaxReply::prepare_receive_shared(
                                &message,
                                request_ticket,
                                scope.source.clone(),
                                None,
                            )
                        });
                        result = if let Some(reception) = reception {
                            receive_syntax_reply(reception, || {
                                !pending.is_disposed()
                                    && ticket.get_value() == request_ticket
                                    && self.syntax_scope_current(&scope)
                            })
                            .await
                        } else {
                            None
                        };
                        if pending.is_disposed() {
                            break;
                        }
                        if ticket.get_value() != request_ticket
                            || !self.syntax_scope_current(&scope)
                        {
                            continue;
                        }
                        result = result.filter(|(status, _)| {
                            *status != openwebide_core::editor::SyntaxStatus::NeedsSource
                        });
                    }
                    if let Some((status, analysis)) = result {
                        published.set_value(analysis.as_ref().map(|analysis| PublishedSyntax {
                            scope: scope.clone(),
                            ticket: request_ticket,
                            analysis: analysis.clone(),
                        }));
                        self.workspace
                            .editor_preparation
                            .set(Some(PreparedEditorSyntax {
                                scope,
                                status,
                                analysis,
                            }));
                    } else {
                        // Keep the shared budgeted parser fallback and cooperative
                        // lexical paint after failure; never trust a malformed reply.
                        self.workspace.editor_worker_active.set(false);
                        self.workspace.editor_preparation.set(None);
                        client.stop();
                    }
                    self.workspace
                        .editor_preparation_revision
                        .update(|value| *value = value.wrapping_add(1));
                }
                running.set(false);
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{auth::AuthState, workspace::WorkspaceState};
    use std::sync::Arc;

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn request_publication_yields_and_rejects_replaced_owners_in_both_modes() {
        use crate::state::projects::ProjectsState;
        use openwebide_core::{Project, WorkspaceMode, highlight::Language};
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for change in 0..6 {
                let owner = Owner::new();
                let (actions, workspace, auth) = owner.with(|| {
                    let workspace = WorkspaceState::new();
                    let auth = AuthState::new();
                    provide_context(auth);
                    let projects = ProjectsState::with_active_project(workspace.active_project);
                    projects.projects.set(vec![Project {
                        id: 1,
                        name: "one".into(),
                        mode,
                        path: Some("one".into()),
                        user_id: None,
                        created_at: 1,
                    }]);
                    provide_context(projects);
                    workspace.active_project.set(Some(1));
                    workspace.open_file.set(Some("request.rs".into()));
                    workspace
                        .content
                        .set("// 文😀\\\"\t\r\n".repeat(10_000).into());
                    (EditorActions::new(workspace), workspace, auth)
                });
                let scope = actions.syntax_scope().unwrap();
                let publication = SyntaxRequest::publication_shared(
                    9,
                    "owner".into(),
                    Language::Rust,
                    scope.source.clone(),
                    4,
                    None,
                );
                let mut pending = Box::pin(publish_syntax_request(publication, || {
                    actions.syntax_scope_current(&scope)
                }));
                assert!(
                    futures::poll!(&mut pending).is_pending(),
                    "large publication must yield"
                );
                let message = serde_json::json!({
                    "version": openwebide_core::editor::SYNTAX_PROTOCOL_VERSION,
                    "ticket": 9,
                    "status": {"Ready": {"incremental": false}},
                    "analysis": {"source": scope.source.as_str(), "folds": [],
                        "structure": null, "highlights": null}
                })
                .to_string();
                let reception =
                    SyntaxReply::prepare_receive_shared(&message, 9, scope.source.clone(), None)
                        .unwrap();
                let mut receiving = Box::pin(receive_syntax_reply(reception, || {
                    actions.syntax_scope_current(&scope)
                }));
                assert!(
                    futures::poll!(&mut receiving).is_pending(),
                    "large source proof must yield"
                );
                match change {
                    0 => workspace.content.set("new source".into()),
                    1 => workspace.open_file.set(Some("other.rs".into())),
                    2 => workspace.active_project.set(Some(2)),
                    3 => auth.generation.update(|generation| *generation += 1),
                    4 => workspace
                        .editor_read_revision
                        .update(|revision| *revision += 1),
                    _ => workspace.pending_epoch.update(|epoch| *epoch += 1),
                }
                assert!(pending.await.is_none());
                assert!(receiving.await.is_none());
                let current = actions.syntax_scope().unwrap();
                let publication = SyntaxRequest::publication_shared(
                    10,
                    "owner".into(),
                    Language::Rust,
                    current.source.clone(),
                    4,
                    None,
                );
                let message =
                    publish_syntax_request(publication, || actions.syntax_scope_current(&current))
                        .await
                        .unwrap();
                let expected = SyntaxRequest::new(
                    10,
                    "owner".into(),
                    Language::Rust,
                    &current.source,
                    4,
                    None,
                );
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&message).unwrap(),
                    serde_json::to_value(expected).unwrap()
                );
                let reply = serde_json::json!({
                    "version": openwebide_core::editor::SYNTAX_PROTOCOL_VERSION,
                    "ticket": 10, "status": {"Ready": {"incremental": false}},
                    "analysis": {"source": current.source.as_str(), "folds": [],
                        "structure": null, "highlights": null}
                })
                .to_string();
                let reception =
                    SyntaxReply::prepare_receive_shared(&reply, 10, current.source.clone(), None)
                        .unwrap();
                let analysis =
                    receive_syntax_reply(reception, || actions.syntax_scope_current(&current))
                        .await
                        .unwrap()
                        .1
                        .unwrap();
                assert!(Arc::ptr_eq(analysis.source_snapshot(), &current.source));
            }
        }
    }

    fn reconstruction_message(source: &str, ticket: u32, structural: bool) -> String {
        let structure = structural.then(|| {
            let selections = source
                .char_indices()
                .step_by(8)
                .map(|(start, ch)| start..start + ch.len_utf8())
                .collect::<Vec<_>>();
            serde_json::json!({"language":"Rust", "scopes":[], "selections": selections,
                "opaque_starts":[], "protected":[], "brackets":[]})
        });
        let highlights = (!structural).then(|| serde_json::json!([[[source.len(), "Plain"]]]));
        serde_json::json!({"version": openwebide_core::editor::SYNTAX_PROTOCOL_VERSION,
            "ticket": ticket, "status": {"Ready": {"incremental": false}},
            "analysis": {"source": source, "folds":[], "structure": structure, "highlights": highlights}}).to_string()
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn token_reception_yields_and_rejects_replaced_owners_in_both_modes() {
        reconstruction_rejects_replaced_owners_in_both_modes(false).await;
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn structure_reception_yields_and_rejects_replaced_owners_in_both_modes() {
        reconstruction_rejects_replaced_owners_in_both_modes(true).await;
    }

    async fn reconstruction_rejects_replaced_owners_in_both_modes(structural: bool) {
        use crate::state::projects::ProjectsState;
        use openwebide_core::{Project, WorkspaceMode};
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for change in 0..6 {
                let owner = Owner::new();
                let (actions, workspace, auth) = owner.with(|| {
                    let workspace = WorkspaceState::new();
                    let auth = AuthState::new();
                    provide_context(auth);
                    let projects = ProjectsState::with_active_project(workspace.active_project);
                    projects.projects.set(vec![Project {
                        id: 1,
                        name: "one".into(),
                        mode,
                        path: Some("one".into()),
                        user_id: None,
                        created_at: 1,
                    }]);
                    provide_context(projects);
                    workspace.active_project.set(Some(1));
                    workspace.open_file.set(Some("long.txt".into()));
                    workspace.content.set("文😀".repeat(40_000).into());
                    (EditorActions::new(workspace), workspace, auth)
                });
                let scope = actions.syntax_scope().unwrap();
                let message = reconstruction_message(&scope.source, 9, structural);
                let checks = Cell::new(0);
                // Source proof consumes one 8 KiB unit per check. Interrupt later,
                // while token materialization or structural proof is still pending.
                let interrupt_after = scope.source.len().div_ceil(8 * 1024) + 8;
                let reception =
                    SyntaxReply::prepare_receive_shared(&message, 9, scope.source.clone(), None)
                        .unwrap();
                let result = receive_syntax_reply(reception, || {
                    checks.set(checks.get() + 1);
                    if checks.get() == interrupt_after {
                        match change {
                            0 => workspace.content.set("replacement".into()),
                            1 => workspace.open_file.set(Some("other.txt".into())),
                            2 => workspace.active_project.set(Some(2)),
                            3 => auth.generation.update(|generation| *generation += 1),
                            4 => workspace
                                .editor_read_revision
                                .update(|revision| *revision += 1),
                            _ => workspace.pending_epoch.update(|epoch| *epoch += 1),
                        }
                    }
                    actions.syntax_scope_current(&scope)
                })
                .await;
                assert!(
                    checks.get() >= interrupt_after,
                    "reconstruction must yield after source proof"
                );
                assert!(result.is_none());
                let fresh = actions.syntax_scope().unwrap();
                let message = reconstruction_message(&fresh.source, 10, structural);
                let reception =
                    SyntaxReply::prepare_receive_shared(&message, 10, fresh.source.clone(), None)
                        .unwrap();
                let analysis =
                    receive_syntax_reply(reception, || actions.syntax_scope_current(&fresh))
                        .await
                        .unwrap()
                        .1
                        .unwrap();
                assert!(Arc::ptr_eq(analysis.source_snapshot(), &fresh.source));
                if structural {
                    assert_eq!(
                        analysis.structure().unwrap().language_at(0),
                        openwebide_core::highlight::Language::Rust
                    );
                } else {
                    assert_eq!(
                        analysis.highlights().unwrap()[0][0].text,
                        fresh.source.as_str()
                    );
                }
            }
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn syntax_requests_share_source_across_pending_queries_and_tab_settings() {
        Owner::new().with(|| {
            let workspace = WorkspaceState::new();
            workspace.active_project.set(Some(1));
            workspace.open_file.set(Some("same.rs".into()));
            workspace
                .content
                .set("fn main() { 文😀(); }\r\n".repeat(1000).into());
            let actions = EditorActions::new(workspace);
            let first = actions.syntax_scope().unwrap();
            let second = actions.syntax_scope().unwrap();
            assert!(Arc::ptr_eq(&first.source, &second.source));
            assert!(Arc::ptr_eq(
                &first.source,
                &workspace.content.get_untracked().shared()
            ));
            assert_eq!(first.source_revision, second.source_revision);
            assert!(actions.syntax_source_retained(&second));
            let mut indentation = actions.rules_untracked().indentation;
            indentation.tab_width = first.tab_width + 4;
            actions.set_indentation(indentation);
            let changed = actions.syntax_scope().unwrap();
            assert!(Arc::ptr_eq(&first.source, &changed.source));
            assert_ne!(first.tab_width, changed.tab_width);
            assert!(!actions.syntax_scope_current(&first));
            assert!(actions.syntax_scope_current(&changed));
            assert_eq!(first.source_revision, changed.source_revision);
            workspace.content.set(first.source.to_string().into());
            assert!(!actions.syntax_scope_current(&changed));
            let refreshed = actions.syntax_scope().unwrap();
            assert!(Arc::ptr_eq(&first.source, &refreshed.source));
            assert_ne!(refreshed.source_revision, changed.source_revision);
            assert!(actions.syntax_source_retained(&refreshed));
            let mut external = refreshed.clone();
            external.source = Arc::new(refreshed.source.as_ref().clone());
            assert!(!actions.syntax_source_retained(&external));
            assert!(actions.syntax_scope_current(&external));
            external.source = Arc::new("forged source".to_owned());
            assert!(!actions.syntax_scope_current(&external));
            workspace.open_file.set(None);
            assert!(actions.syntax_scope().is_none());
            assert!(workspace.editor_syntax_scope.get_untracked().is_none());
        });
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn syntax_source_cache_rejects_content_and_ownership_changes_and_reset() {
        Owner::new().with(|| {
            let auth = AuthState::new();
            provide_context(auth);
            let workspace = WorkspaceState::new();
            workspace.active_project.set(Some(1));
            workspace.open_file.set(Some("same.rs".into()));
            workspace.content.set("same source\r\n文😀".into());
            let actions = EditorActions::new(workspace);
            let initial = actions.syntax_scope().unwrap();
            let mut previous = initial.clone();
            for change in 0..6 {
                match change {
                    0 => workspace.content.set("changed source\r\n文😀".into()),
                    1 => workspace.active_project.set(Some(2)),
                    2 => workspace.open_file.set(Some("other.rs".into())),
                    3 => workspace.pending_epoch.update(|epoch| *epoch += 1),
                    4 => workspace
                        .editor_read_revision
                        .update(|revision| *revision += 1),
                    _ => auth.generation.update(|generation| *generation += 1),
                }
                let next = actions.syntax_scope().unwrap();
                assert!(Arc::ptr_eq(
                    &workspace.content.get_untracked().shared(),
                    &next.source
                ));
                if change == 0 {
                    assert!(!Arc::ptr_eq(&previous.source, &next.source));
                }
                assert!(!actions.syntax_scope_current(&previous));
                assert!(actions.syntax_scope_current(&next));
                previous = next;
            }
            assert_eq!(initial.source.as_ref(), "same source\r\n文😀");
            workspace.reset();
            assert!(workspace.editor_syntax_scope.get_untracked().is_none());
            assert!(!actions.syntax_scope_current(&previous));
        });
    }
}
