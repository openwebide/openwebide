//! Prompt resolution and composer controls above workspace/host primitives.
use crate::{
    backend::Api,
    project_git::ProjectGit,
    state::{auth::AuthState, chat::ChatState, projects::ProjectsState},
    workspace::Workspace,
};
use leptos::{prelude::*, task::spawn_local};
use openwebide_core::{
    PromptContent, PromptImage,
    prompt::{MentionChoice, PromptSource},
};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

pub struct ProjectPromptSource {
    files: Workspace,
    git: ProjectGit,
    project: i64,
}
impl ProjectPromptSource {
    pub fn new(
        api: Api,
        projects: ProjectsState,
        git: ProjectGit,
        project: Option<i64>,
    ) -> Result<Self, String> {
        let project = project.ok_or("File, folder and diff mentions need an open project.")?;
        let files = Workspace::for_project(api, projects, project)
            .ok_or("Grant access to the project folder before attaching context.")?;
        Ok(Self {
            files,
            git,
            project,
        })
    }
}
impl PromptSource for ProjectPromptSource {
    async fn read(&self, path: &str) -> Result<String, String> {
        self.files.read(path).await.map_err(String::from)
    }
    async fn list(&self, path: &str) -> Result<Vec<openwebide_core::FileEntry>, String> {
        self.files.list(path).await.map_err(String::from)
    }
    async fn diff(&self, path: Option<&str>) -> Result<String, String> {
        self.git
            .repository(Some(self.project))
            .await?
            .diff(path)
            .await
    }
}
pub async fn prepare(
    api: Api,
    projects: ProjectsState,
    git: ProjectGit,
    prompt: PromptContent,
    current: impl Fn() -> bool,
) -> Result<String, String> {
    let prompt = if openwebide_core::prompt::mentions(&prompt.text)?.is_empty() {
        prompt
    } else {
        let source =
            ProjectPromptSource::new(api, projects, git, projects.active_project.get_untracked())?;
        openwebide_core::prompt::attach(&source, prompt, &current).await?
    };
    if !current() {
        return Err("Project or session changed while attaching context.".into());
    }
    prompt.encode()
}

pub fn restore_draft(chat: ChatState, prompt: &PromptContent) {
    chat.messages.drop_optimistic_user();
    if chat.draft.with_untracked(String::is_empty)
        && chat.prompt_images.with_untracked(Vec::is_empty)
    {
        chat.draft.set(prompt.text.clone());
        chat.prompt_images.set(prompt.images.clone());
    }
}

#[derive(Clone, Copy)]
pub struct Composer {
    pub choices: RwSignal<Vec<MentionChoice>>,
    pub selected: RwSignal<usize>,
    query: RwSignal<Option<(usize, usize, String)>>,
    ticket: StoredValue<u64>,
    input: NodeRef<leptos::html::Textarea>,
    chat: ChatState,
    projects: ProjectsState,
    auth: AuthState,
    api: Api,
    git: ProjectGit,
}
impl Composer {
    pub fn new(input: NodeRef<leptos::html::Textarea>) -> Self {
        let composer = Self {
            choices: RwSignal::new(Vec::new()),
            selected: RwSignal::new(0),
            query: RwSignal::new(None),
            ticket: StoredValue::new(0),
            input,
            chat: expect_context(),
            projects: expect_context(),
            auth: expect_context(),
            api: expect_context(),
            git: expect_context(),
        };
        Effect::new(move |_| {
            composer.auth.generation.get();
            composer.projects.active_project.get();
            composer.chat.active_session.get();
            composer.chat.prompt_images.set(Vec::new());
            composer.chat.reading_images.set(false);
            composer.chat.image_picker_requested.set(false);
            composer.dismiss();
        });
        composer
    }
    /// Restore typing after actions, once dropdown and reactive focus updates settle.
    pub fn focus(self) {
        let account = self.auth.generation.get_untracked();
        let project = self.projects.active_project.get_untracked();
        let session = self.chat.active_session.get_untracked();
        let restore = move || {
            if self.auth.generation.try_get_untracked() == Some(account)
                && self.projects.active_project.try_get_untracked() == Some(project)
                && self.chat.active_session.try_get_untracked() == Some(session)
                && let Some(Some(input)) = self.input.try_get_untracked()
                && crate::components::modal::allows_focus(input.as_ref())
            {
                let _ = input.focus();
            }
        };
        restore();
        leptos::leptos_dom::helpers::request_animation_frame(restore);
    }
    pub fn after<T: Send + 'static>(self, action: Callback<T>) -> Callback<T> {
        Callback::new(move |value| {
            action.run(value);
            self.focus();
        })
    }
    pub fn dismiss(self) {
        self.choices.set(Vec::new());
        self.query.set(None);
        self.ticket.update_value(|ticket| *ticket += 1);
    }
    pub fn update(self) {
        let Some(input) = self.input.get() else {
            return;
        };
        let text = input.value();
        let caret = crate::text::utf16_to_byte(
            &text,
            input.selection_start().ok().flatten().unwrap_or(0) as usize,
        );
        let query = openwebide_core::prompt::query_at(&text, caret);
        if query == self.query.get_untracked() {
            return;
        }
        self.dismiss();
        let Some((start, end, query_text)) = query else {
            return;
        };
        if self.projects.active_project.get_untracked().is_none() {
            return;
        }
        self.query.set(Some((start, end, query_text.clone())));
        self.selected.set(0);
        let ticket = self.ticket.get_value();
        let generation = self.auth.generation.get_untracked();
        let project = self.projects.active_project.get_untracked();
        let session = self.chat.active_session.get_untracked();
        spawn_local(async move {
            let result = match ProjectPromptSource::new(self.api, self.projects, self.git, project)
            {
                Ok(source) => openwebide_core::prompt::complete(&source, &query_text).await,
                Err(error) => Err(error),
            };
            if self.ticket.try_get_value() != Some(ticket)
                || self.auth.generation.try_get_untracked() != Some(generation)
                || self.projects.active_project.try_get_untracked() != Some(project)
                || self.chat.active_session.try_get_untracked() != Some(session)
            {
                return;
            }
            if let Ok(choices) = result {
                self.choices.set(choices);
            }
        });
    }
    pub fn choose(self, index: usize) {
        let Some(choice) = self
            .choices
            .with_untracked(|choices| choices.get(index).cloned())
        else {
            return;
        };
        let Some((start, end, _)) = self.query.get_untracked() else {
            return;
        };
        let mut text = self.chat.draft.get_untracked();
        if text.get(start..end).is_none() {
            self.dismiss();
            return;
        }
        text.replace_range(start..end, &choice.insertion);
        let directory_quote = choice.insertion.ends_with("/\"");
        let byte_caret = start + choice.insertion.len() - usize::from(directory_quote);
        let caret = u32::try_from(text[..byte_caret].encode_utf16().count()).unwrap_or(u32::MAX);
        self.chat.draft.set(text.clone());
        self.dismiss();
        if let Some(input) = self.input.get() {
            input.set_value(&text);
            let _ = input.focus();
            let _ = input.set_selection_range(caret, caret);
            self.update();
        }
    }
    pub fn key(self, event: &web_sys::KeyboardEvent) -> bool {
        let count = self.choices.with_untracked(Vec::len);
        if count == 0 {
            return false;
        }
        match event.key().as_str() {
            "ArrowDown" => self.selected.update(|index| *index = (*index + 1) % count),
            "ArrowUp" => self
                .selected
                .update(|index| *index = (*index + count - 1) % count),
            "Enter" | "Tab" if !event.shift_key() => self.choose(self.selected.get_untracked()),
            "Escape" => self.dismiss(),
            _ => return false,
        }
        event.prevent_default();
        true
    }
    pub fn import(self, files: web_sys::FileList) {
        if self.chat.reading_images.get_untracked() {
            return;
        }
        let files: Vec<_> = (0..files.length())
            .filter_map(|index| files.item(index))
            .collect();
        if files.is_empty() {
            return;
        }
        let generation = self.auth.generation.get_untracked();
        let project = self.projects.active_project.get_untracked();
        let session = self.chat.active_session.get_untracked();
        let current = move || {
            self.auth.generation.try_get_untracked() == Some(generation)
                && self.projects.active_project.try_get_untracked() == Some(project)
                && self.chat.active_session.try_get_untracked() == Some(session)
        };
        self.chat.reading_images.set(true);
        spawn_local(async move {
            let result = async {
                let mut prompt = PromptContent {
                    images: self.chat.prompt_images.get_untracked(),
                    ..Default::default()
                };
                for file in files {
                    if file.size() > openwebide_core::prompt::MAX_IMAGE_BYTES as f64 {
                        return Err("Each image must be at most 2 MiB.".into());
                    }
                    if !current() {
                        return Err("Workspace changed while reading images.".into());
                    }
                    prompt.images.push(read_image(&file).await?);
                    prompt.validate()?;
                }
                Ok::<_, String>(prompt.images)
            }
            .await;
            if current() {
                self.chat.reading_images.set(false);
                match result {
                    Ok(images) => self.chat.prompt_images.set(images),
                    Err(error) => self.chat.error.set(Some(error)),
                }
                self.focus();
            }
        });
    }
}
async fn read_image(file: &web_sys::File) -> Result<PromptImage, String> {
    let bytes = JsFuture::from(file.array_buffer())
        .await
        .map_err(|_| "Could not read image.")?;
    let image = PromptImage::from_bytes(file.name(), &js_sys::Uint8Array::new(&bytes).to_vec())?;
    let window = web_sys::window().ok_or("Browser window unavailable.")?;
    let bitmap = JsFuture::from(
        window
            .create_image_bitmap_with_blob(file)
            .map_err(|_| "Could not decode image.")?,
    )
    .await
    .map_err(|_| "Could not decode image.")?
    .dyn_into::<web_sys::ImageBitmap>()
    .map_err(|_| "Could not decode image.")?;
    let longest = bitmap.width().max(bitmap.height()).max(1);
    let limit = longest.min(2048);
    if matches!(image.mime.as_str(), "image/png" | "image/jpeg") && longest == limit {
        bitmap.close();
        return Ok(image);
    }
    // Normalize formats the providers may not decode and bound raster dimensions.
    let canvas = window
        .document()
        .ok_or("Document unavailable.")?
        .create_element("canvas")
        .map_err(|_| "Could not convert image.")?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| "Could not convert image.")?;
    let dimension = |value: u32| {
        u32::try_from((u64::from(value) * u64::from(limit) / u64::from(longest)).max(1))
            .map_err(|_| "Could not resize image.")
    };
    canvas.set_width(dimension(bitmap.width())?);
    canvas.set_height(dimension(bitmap.height())?);
    let context = canvas
        .get_context("2d")
        .map_err(|_| "Could not convert image.")?
        .ok_or("Could not convert image.")?
        .dyn_into::<web_sys::CanvasRenderingContext2d>()
        .map_err(|_| "Could not convert image.")?;
    let drawn = context.draw_image_with_image_bitmap_and_dw_and_dh(
        &bitmap,
        0.0,
        0.0,
        f64::from(canvas.width()),
        f64::from(canvas.height()),
    );
    bitmap.close();
    drawn.map_err(|_| "Could not convert image.")?;
    let data = canvas
        .to_data_url_with_type("image/png")
        .map_err(|_| "Could not convert image.")?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.split_once(',').ok_or("Could not convert image.")?.1)
        .map_err(|_| "Could not convert image.")?;
    PromptImage::from_bytes(file.name(), &bytes)
}

#[component]
pub fn PromptControls(composer: Composer) -> impl IntoView {
    let file_input = NodeRef::<leptos::html::Input>::new();
    let menu_open = RwSignal::new(false);
    Effect::new(move || {
        if composer.chat.image_picker_requested.get()
            && let Some(input) = file_input.get()
        {
            composer.chat.image_picker_requested.set(false);
            input.click();
        }
    });
    let count = Memo::new(move |_| {
        composer.chat.prompt_images.with(Vec::len)
            + usize::from(composer.chat.active_editor_context.get().is_some())
    });
    view! {
        <div class="prompt-attachments">
            <input type="file" accept="image/png,image/jpeg,image/gif,image/webp" multiple=true node_ref=file_input style="display:none" on:change=move |event| {
                let target = event_target::<web_sys::HtmlInputElement>(&event);
                if let Some(files) = target.files() { composer.import(files); }
                target.set_value("");
            } on:cancel=move |_: web_sys::Event| composer.focus() />
            <Show when=move || count.get() != 0 || composer.chat.reading_images.get()>
                <crate::components::dropdown::Dropdown aria_label="Prompt attachments" menu_role="dialog" class="prompt-attachment-menu" open=menu_open trigger_class="btn ghost ui-icon" hide_caret=true above=true
                    disabled=Signal::derive(move || composer.chat.reading_images.get())
                    label=move || view! { <crate::components::ui::Icon name=crate::components::ui::IconName::Paperclip/><span class="prompt-attachment-count">{move || if composer.chat.reading_images.get() { "…".to_string() } else { count.get().to_string() }}</span><span class="sr-only">{move || if composer.chat.reading_images.get() {"Reading images"} else {"attachments"}}</span> }>
                    <Show when=move || composer.chat.active_editor_context.get().is_some()>
                        <div class="prompt-editor-context"><span>{move || composer.chat.active_editor_context.get().map(|context| context.pill_label())}</span><button class="btn ghost ui-icon" title="Detach editor context (Esc)" aria-label="Detach editor context" on:click=move |_| { menu_open.set(false); composer.chat.active_editor_context.set(None); composer.focus(); }><crate::components::ui::Icon name=crate::components::ui::IconName::X/></button></div>
                    </Show>
                    <div class="prompt-image-list">
                        <For each={move || composer.chat.prompt_images.get().into_iter().enumerate().collect::<Vec<_>>()} key=|(index, image)| (*index, image.data.clone()) children=move |(index, image)| {
                            let remove_label = format!("Remove image {}", image.name);
                            view! { <div class="prompt-image"><img src=image.url() alt=image.name.clone() /><span>{image.name}</span><button class="btn ghost ui-icon" title="Remove image" aria-label=remove_label disabled=move || composer.chat.reading_images.get() on:click=move |_| { menu_open.set(false); composer.chat.prompt_images.update(|images| { if index < images.len() { images.remove(index); } }); composer.focus(); }><crate::components::ui::Icon name=crate::components::ui::IconName::X/></button></div> }
                        } />
                    </div>
                    <button class="btn ghost" on:click=move |_| { menu_open.set(false); if let Some(input) = file_input.get_untracked() { input.click(); } }><crate::components::ui::Icon name=crate::components::ui::IconName::Paperclip/>"Add images"</button>
                </crate::components::dropdown::Dropdown>
            </Show>
        </div>
    }
}

#[component]
pub fn MentionSuggestions(composer: Composer) -> impl IntoView {
    view! {
        <Show when=move || !composer.choices.with(Vec::is_empty)>
            <div class="mention-choices" role="listbox" aria-label="Mention suggestions">
                <For each={move || composer.choices.get().into_iter().enumerate().collect::<Vec<_>>() } key=|(index, choice)| (*index, choice.insertion.clone()) children=move |(index, choice)| view! {
                    <button class="btn ghost" role="option" aria-selected=move || composer.selected.get() == index class:active=move || composer.selected.get() == index on:click=move |_| composer.choose(index)>{choice.label}</button>
                } />
            </div>
        </Show>
    }
}

#[component]
pub fn PromptHistory(content: Memo<String>) -> impl IntoView {
    let prompt = Memo::new(move |_| PromptContent::decode(&content.get()));
    view! {
        <div class="prompt-image-list">{move || prompt.get().images.into_iter().map(|image| view! { <img class="prompt-history-image" src=image.url() alt=image.name /> }).collect_view()}</div>
        {move || prompt.get().references.into_iter().map(|reference| view! { <details class="prompt-reference"><summary>{format!("{:?}: {}", reference.mention.kind, if reference.mention.path.is_empty() { "." } else { &reference.mention.path })}</summary><pre>{reference.content}</pre></details> }).collect_view()}
    }
}
