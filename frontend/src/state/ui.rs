use leptos::prelude::*;

/// A pending confirmation request. `action` runs when the user confirms.
#[derive(Clone)]
pub struct ConfirmRequest {
    pub title: String,
    pub message: String,
    /// Label for the confirm button (for example, "Delete" or "Log out").
    pub confirm_label: String,
    pub action: Callback<()>,
}

/// A pending single-field input request.
#[derive(Clone)]
pub struct PromptRequest {
    pub title: String,
    /// The field's initial value.
    pub value: String,
    /// Hint shown when the field is empty.
    pub placeholder: String,
    /// Label for the submit button.
    pub submit_label: String,
    pub on_submit: Callback<String>,
}

/// Signals shared by app-level notices and dialogs.
#[derive(Clone, Copy)]
pub struct UiState {
    pub whats_new_open: RwSignal<bool>,
    pub about_release_notes: RwSignal<bool>,
    pub about_open: RwSignal<bool>,
    pub plugins_open: RwSignal<bool>,
    pub palette_open: RwSignal<bool>,
    pub shortcuts_open: RwSignal<bool>,
    pub context_open: RwSignal<bool>,
    pub generation_open: RwSignal<bool>,
    pub run_context: RwSignal<Option<i64>>,
    pub editor_convert: RwSignal<Option<Callback<()>>>,
    pub editor_can_convert: RwSignal<bool>,
    pub toast: RwSignal<Option<String>>,
    pub confirm: RwSignal<Option<ConfirmRequest>>,
    pub prompt: RwSignal<Option<PromptRequest>>,
}

impl UiState {
    pub fn new() -> Self {
        Self {
            whats_new_open: RwSignal::new(false),
            about_release_notes: RwSignal::new(false),
            about_open: RwSignal::new(false),
            plugins_open: RwSignal::new(false),
            palette_open: RwSignal::new(false),
            shortcuts_open: RwSignal::new(false),
            context_open: RwSignal::new(false),
            generation_open: RwSignal::new(false),
            run_context: RwSignal::new(None),
            editor_convert: RwSignal::new(None),
            editor_can_convert: RwSignal::new(false),
            toast: RwSignal::new(None),
            confirm: RwSignal::new(None),
            prompt: RwSignal::new(None),
        }
    }

    /// Show a toast, replacing any toast already on screen.
    pub fn notify(&self, text: impl Into<String>) {
        self.toast.set(Some(text.into()));
    }

    pub fn clear_toast(&self) {
        self.toast.set(None);
    }

    pub fn set_confirm(&self, request: ConfirmRequest) {
        self.confirm.set(Some(request));
    }

    pub fn clear_confirm(&self) {
        self.confirm.set(None);
    }

    pub fn set_prompt(&self, request: PromptRequest) {
        self.prompt.set(Some(request));
    }

    pub fn clear_prompt(&self) {
        self.prompt.set(None);
    }
}

impl Default for UiState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_replaces_the_previous_message() {
        Owner::new().with(|| {
            let ui = UiState::new();

            ui.notify("first");
            ui.notify("second");

            assert_eq!(ui.toast.get_untracked().as_deref(), Some("second"));
        });
    }

    #[test]
    fn cancelling_a_confirmation_clears_it() {
        Owner::new().with(|| {
            let ui = UiState::new();
            ui.set_confirm(ConfirmRequest {
                title: "Delete project?".into(),
                message: "This cannot be undone.".into(),
                confirm_label: "Delete".into(),
                action: Callback::new(|()| {}),
            });

            assert!(ui.confirm.get_untracked().is_some());

            ui.clear_confirm();

            assert!(ui.confirm.get_untracked().is_none());
        });
    }
}
