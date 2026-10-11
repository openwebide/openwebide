//! Shared deferred-motion policy; frame and DOM measurement adapters call here.
use super::EditorActions;
use crate::state::editor_motion::PendingEditorMotion;
use leptos::prelude::*;
use openwebide_core::editor::{
    MotionQueue, MotionRequest, Selection, SelectionError, SelectionMotion, VisualLayout,
};

impl EditorActions {
    /// DOM callers provide their stamped project/file; source stays borrowed here.
    pub fn queue_current_motion(
        self,
        project: i64,
        path: &str,
        motion: SelectionMotion,
        extend: bool,
    ) -> Result<Option<(u64, bool)>, SelectionError> {
        self.workspace
            .content
            .with_untracked(|source| self.queue_motion(project, path, source, motion, extend))
    }

    pub fn queue_current_page_motion(
        self,
        project: i64,
        path: &str,
        down: bool,
        extend: bool,
        viewport: (f64, f64),
    ) -> Result<Option<(u64, bool)>, SelectionError> {
        self.workspace.content.with_untracked(|source| {
            self.queue_page_motion(project, path, source, down, extend, viewport)
        })
    }

    /// Page motion retains one visual row of overlap and uses the same ordered
    /// source-motion queue as arrows, including wrapped cursor-neighbor probes.
    pub fn queue_page_motion(
        self,
        project: i64,
        path: &str,
        source: &str,
        down: bool,
        extend: bool,
        viewport: (f64, f64),
    ) -> Result<Option<(u64, bool)>, SelectionError> {
        let (height, row_height) = viewport;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "finite viewport row counts are floored and clamped to 1..=512 before conversion"
        )]
        let rows = if height.is_finite() && row_height.is_finite() && row_height > 0.0 {
            (height / row_height - 1.0).floor().clamp(1.0, 512.0) as usize
        } else {
            1
        };
        let motion = if down {
            SelectionMotion::Down
        } else {
            SelectionMotion::Up
        };
        let mut result = None;
        let mut scheduled = false;
        for _ in 0..rows {
            if let Some((ticket, start)) =
                self.queue_motion(project, path, source, motion, extend)?
            {
                scheduled |= start;
                result = Some((ticket, scheduled));
            } else {
                return Ok(None);
            }
        }
        Ok(result)
    }

    fn motion_current(self, pending: &PendingEditorMotion) -> bool {
        self.key().as_ref() == Some(&pending.key)
            && self.workspace.pending_epoch.get_untracked() == pending.epoch
            && self.workspace.editor_read_revision.get_untracked() == pending.read_revision
            && self.auth.map_or(0, |auth| auth.generation.get_untracked())
                == pending.account_generation
            && untrack(|| self.preferences()) == pending.preferences
            && self.rules_untracked().indentation == pending.indentation
            && self.workspace.editor_documents.with_untracked(|documents| {
                documents.get(&pending.key).is_some_and(|document| {
                    pending.queue.matches(document)
                        && self
                            .workspace
                            .content
                            .with_untracked(|source| document.matches_text(source))
                })
            })
    }

    pub fn cancel_queued_motion(self, ticket: Option<u64>) {
        self.workspace.editor_motion.try_update(|pending| {
            if ticket.is_none()
                || pending
                    .as_ref()
                    .is_some_and(|pending| Some(pending.ticket) == ticket)
            {
                *pending = None;
            }
        });
    }

    pub fn queued_motion_ticket(self) -> Option<u64> {
        self.workspace
            .editor_motion
            .with_untracked(|pending| pending.as_ref().map(|pending| pending.ticket))
    }

    /// Returns a ticket and whether the browser should start a new frame callback.
    pub fn queue_motion(
        self,
        project: i64,
        path: &str,
        source: &str,
        motion: SelectionMotion,
        extend: bool,
    ) -> Result<Option<(u64, bool)>, SelectionError> {
        if !self.is_current(project, path)
            || self
                .workspace
                .content
                .with_untracked(|current| current != source)
        {
            return Ok(None);
        }
        let existing = self
            .workspace
            .editor_motion
            .try_update(Option::take)
            .flatten();
        let preferences = untrack(|| self.preferences());
        let indentation = self.rules_untracked().indentation;
        let (mut pending, scheduled) = if let Some(pending) =
            existing.filter(|pending| self.motion_current(pending))
        {
            (pending, false)
        } else {
            let key = (project, path.to_owned());
            let queue = self
                .workspace
                .editor_documents
                .try_update(|documents| MotionQueue::new(self.document(documents, key.clone())))
                .unwrap_or(Err(openwebide_core::editor::EditError::StaleContext.into()))?;
            self.workspace
                .editor_motion_ticket
                .update(|ticket| *ticket = ticket.wrapping_add(1));
            (
                PendingEditorMotion {
                    ticket: self.workspace.editor_motion_ticket.get_untracked(),
                    key,
                    epoch: self.workspace.pending_epoch.get_untracked(),
                    read_revision: self.workspace.editor_read_revision.get_untracked(),
                    account_generation: self.auth.map_or(0, |auth| auth.generation.get_untracked()),
                    preferences,
                    indentation,
                    queue,
                },
                true,
            )
        };
        let result = pending.queue.push(MotionRequest {
            motion,
            extend,
            wrapped: preferences.word_wrap,
        });
        let ticket = pending.ticket;
        self.workspace.editor_motion.set(Some(pending));
        result?;
        Ok(Some((ticket, scheduled)))
    }

    pub fn next_queued_motion(self, ticket: u64) -> Option<MotionRequest> {
        let result = self.workspace.editor_motion.with_untracked(|pending| {
            pending
                .as_ref()
                .filter(|pending| pending.ticket == ticket && self.motion_current(pending))
                .and_then(|pending| pending.queue.next_request())
        });
        if result.is_none() {
            self.cancel_queued_motion(Some(ticket));
        }
        result
    }

    pub fn wait_for_motion_layout(self, ticket: u64) -> Result<(), SelectionError> {
        let progress = self
            .workspace
            .editor_row_preparation
            .with_untracked(|preparation| {
                preparation
                    .as_ref()
                    .filter(|preparation| self.row_preparation_current(preparation.ticket))
                    .map(|preparation| (preparation.ticket, preparation.completed))
            });
        self.workspace
            .editor_motion
            .try_update(|pending| {
                let Some(pending) = pending.as_mut().filter(|pending| pending.ticket == ticket)
                else {
                    return Ok(());
                };
                pending.queue.wait_for_preparation(progress)
            })
            .unwrap_or(Ok(()))
    }

    pub fn apply_queued_motion(
        self,
        ticket: u64,
        layout: Option<&VisualLayout>,
    ) -> Result<Option<Vec<Selection>>, SelectionError> {
        let Some(mut pending) = self
            .workspace
            .editor_motion
            .try_update(Option::take)
            .flatten()
        else {
            return Ok(None);
        };
        if pending.ticket != ticket {
            self.workspace.editor_motion.set(Some(pending));
            return Ok(None);
        }
        if !self.motion_current(&pending) {
            return Ok(None);
        }
        let indentation = self.rules_untracked().indentation;
        let mut folds_changed = false;
        let result = self
            .workspace
            .editor_documents
            .try_update(|documents| {
                let Some(document) = documents.get_mut(&pending.key) else {
                    return Ok(None);
                };
                let folds = document.fold_state().clone();
                let result = pending.queue.apply_next(document, indentation, layout);
                folds_changed = document.fold_state() != &folds;
                result
            })
            .unwrap_or(Ok(None));
        if result.is_ok() && pending.queue.next_request().is_some() {
            self.workspace.editor_motion.set(Some(pending));
        }
        if matches!(result, Ok(Some(_))) {
            self.typing.set(None);
            if folds_changed {
                self.workspace
                    .editor_fold_revision
                    .update(|value| *value = value.wrapping_add(1));
            }
        }
        result
    }
}
