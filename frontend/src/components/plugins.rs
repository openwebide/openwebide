use super::{
    dropdown::{ActionMenu, DropdownSelect, SelectOption},
    ui::{
        Button, ButtonSize, ButtonVariant, DisclosurePanel, FormField, FormSection, Icon, IconName,
        InlineActions, PanelSearchRow, TextInput,
    },
};
use crate::{
    project_plugins::{CatalogSelection, ProjectPluginActions},
    state::{plugins::PluginsState, projects::ProjectsState},
};
use leptos::prelude::*;
use openwebide_core::plugins::{
    PluginInstallation, PluginUpdatePolicy,
    marketplace::{CachedMarketplace, CatalogPlugin, MarketplaceSource},
};

#[component]
pub fn Plugins() -> impl IntoView {
    let state = expect_context::<PluginsState>();
    let actions = expect_context::<ProjectPluginActions>();
    let projects = expect_context::<ProjectsState>();
    let ui = expect_context::<crate::state::ui::UiState>();
    let settings = expect_context::<crate::state::settings::SettingsState>();
    let manual = RwSignal::new(false);
    let attempted = RwSignal::new(false);
    let auth = expect_context::<crate::state::auth::AuthState>();
    Effect::new(move |_| {
        auth.generation.track();
        attempted.set(false);
    });
    Effect::new(move |_| {
        if state.loaded.get()
            && !state.busy.get()
            && !attempted.get()
            && state
                .marketplaces
                .with(|m| m.catalogs.is_empty() && !m.sources.is_empty())
        {
            attempted.set(true);
            actions.refresh_catalogs.run(());
        }
    });
    view! {
        <div class="ui-section-content">
            <PanelSearchRow unpadded=true>
                <input type="search" class="form-input panel-search-input" aria-label="Search plugins" placeholder="Search plugins…" maxlength="128"
                    prop:value=move ||state.search.get() on:input=move |event|state.search.set(event_target_value(&event))/>
                <ActionMenu aria_label="Plugin actions">
                    <button type="button" role="menuitem" class="ui-dropdown-item recent-item" disabled=move ||state.busy.get() on:click=move |_|actions.refresh.run(())><Icon name=IconName::ListChecks/><span>"Refresh installations"</span></button>
                    <button type="button" role="menuitem" class="ui-dropdown-item recent-item" disabled=move ||state.busy.get() on:click=move |_|actions.refresh_catalogs.run(())><Icon name=IconName::RefreshCw/><span>"Refresh marketplaces"</span></button>
                    <button type="button" role="menuitem" class="ui-dropdown-item recent-item" on:click=move |_|manual.update(|open| *open = !*open)><Icon name=IconName::Plus/><span>"Install a pinned plugin manually"</span></button>
                    <button type="button" role="menuitem" class="ui-dropdown-item recent-item" on:click=move |_| {
                        ui.plugins_open.set(false);
                        settings.requested_tab.set(5);
                        settings.show_settings.set(true);
                    }><Icon name=IconName::Settings/><span>"Manage marketplace sources"</span></button>
                </ActionMenu>
            </PanelSearchRow>
            <Show when=move ||state.busy.get()><InlineActions>
                <p class="form-hint" role="status">{move || match state.preparation.get() {
                    Some(openwebide_core::plugins::preparation::PreparationState::Queued) => "Queued on host…",
                    Some(openwebide_core::plugins::preparation::PreparationState::Preparing) => "Preparing plugin on host…",
                    _ => "Working…",
                }}</p>
                <Show when=move ||state.preparation.get().is_some()>
                    <Button variant=ButtonVariant::Ghost disabled=state.cancel_preparation.read_only()
                        on_click=Callback::new(move |_|state.cancel_preparation.set(true))>
                        {move ||if state.cancel_preparation.get(){"Cancelling…"}else{"Cancel installation"}}
                    </Button>
                </Show>
            </InlineActions></Show>
            <Show when=move ||state.error.get().is_some()><p class="error" role="alert">{move ||state.error.get().unwrap_or_default()}</p></Show>
            <For each=move ||state.pending_updates.get() key=|update|(update.prepared.source.repository.clone(), update.prepared.source.path.clone(), update.prepared.source.commit.clone()) children=move |update| {
                let source = StoredValue::new(update.prepared.source.clone());
                view! { <FormSection title="Review plugin update"><div class="ui-section-content">
                    <p>{format!("{} {} requests additional access: {}.", update.prepared.manifest.display_name, update.prepared.manifest.version, update.approved_capabilities.join(", "))}</p>
                    <p class="form-hint">"Your installed version stays active until you approve this update."</p>
                    <InlineActions>
                        <Button disabled=state.busy.read_only() on_click=Callback::new(move |_|actions.approve_update.run(source.get_value()))>"Approve and update"</Button>
                        <Button variant=ButtonVariant::Ghost disabled=state.busy.read_only() on_click=Callback::new(move |_|actions.dismiss_update.run(source.get_value()))>"Keep current version"</Button>
                    </InlineActions>
                </div></FormSection> }
            }/>
            <For each=move ||state.failures.get() key=|f|(f.source.repository.clone(),f.source.reference.clone(),f.source.path.clone()) children=move |failure|view!{<p class="error" role="alert">{format!("{}: {} Previously cached releases remain available.",failure.source.repository,failure.message)}</p>}/>
            <Show when=move ||projects.active_project.get().is_none()><p class="form-hint">"Install plugins on the server host. They are enabled by default in your projects."</p></Show>
            <PluginSection title="Installed" count=Signal::derive(move ||filtered_installations(state).len()) header_actions=move || view! {
                <Show when=move ||!state.updates().is_empty()>
                    <Button size=ButtonSize::Sm disabled=state.busy.read_only() on_click=Callback::new(move |_|actions.update_all.run(()))>
                        <Icon name=IconName::ArrowDownToLine/>{move ||format!("Update all ({})",state.updates().len())}
                    </Button>
                </Show>
            }>
                <For each=move ||filtered_installations(state) key=|e|(e.prepared.source.repository.clone(),e.prepared.source.path.clone(),e.revision) children=move |entry|view!{<InstalledPackage entry=entry/>}/>
                <Show when=move ||state.loaded.get() && !state.busy.get() && filtered_installations(state).is_empty()><p class="form-hint plugin-empty">{move ||if state.search.get().is_empty(){"No plugins installed yet."}else{"No installed plugins match your search."}}</p></Show>
            </PluginSection>
            <PluginSection title="Available" count=Signal::derive(move ||state.marketplaces.with(|m|m.catalogs.iter().map(|c|filtered_catalog(state,c).len()).sum()))>
                <For each=move ||available_plugins(state) key=|(catalog,plugin)|(catalog.source.repository.clone(),catalog.source.reference.clone(),catalog.source.path.clone(),catalog.commit.clone(),plugin.publisher.clone(),plugin.name.clone()) children=move |(catalog,plugin)|view!{<CatalogPackage catalog=catalog plugin=plugin/>}/>
                <Show when=move ||state.loaded.get()&&!state.busy.get()&&state.marketplaces.with(|m|m.catalogs.iter().all(|c|filtered_catalog(state,c).is_empty()))><p class="form-hint plugin-empty">{move ||if state.search.get().is_empty(){if state.marketplaces.with(|markets|markets.catalogs.iter().any(|catalog|!catalog.catalog.plugins.is_empty())){"All available plugins are installed."}else{"No available plugins. Refresh marketplaces from the menu."}}else{"No available plugins match your search."}}</p></Show>
            </PluginSection>
            <Show when=move ||manual.get()><FormSection title="Install a pinned plugin manually"><div class="ui-section-content">
                <FormField label="Repository URL"><TextInput label="Plugin repository URL" value=state.repository.read_only() on_change=Callback::new(move |v|state.repository.set(v)) maxlength=2048 disabled=state.busy.read_only()/></FormField>
                <FormField label="Commit"><TextInput label="Plugin commit" value=state.commit.read_only() on_change=Callback::new(move |v|state.commit.set(v)) maxlength=64 disabled=state.busy.read_only()/></FormField>
                <FormField label="Plugin directory"><TextInput label="Plugin directory" value=state.path.read_only() on_change=Callback::new(move |v|state.path.set(v)) maxlength=512 disabled=state.busy.read_only()/></FormField>
                <p class="form-hint">"Use a full commit ID and the directory containing plugin.json. Use . for the repository root."</p>
                <InlineActions><Button disabled=Signal::derive(move ||state.busy.get()) on_click=Callback::new(move |_|actions.install.run(()))><Icon name=IconName::Download/>"Install on host"</Button></InlineActions>
                <InlineActions><Button variant=ButtonVariant::Ghost on_click=Callback::new(move |_|manual.set(false))>"Cancel"</Button></InlineActions>
            </div></FormSection></Show>
        </div>
    }
}
/// Search and plugin lifecycle controls opened from the status bar.
#[component]
pub fn PluginsDialog() -> impl IntoView {
    let ui = expect_context::<crate::state::ui::UiState>();
    view! {
        <super::modal::Modal title=Signal::derive(|| "Plugins".to_string())
            on_close=Callback::new(move |()| ui.plugins_open.set(false))
            size=super::ui::DialogSize::Wide>
            <super::ui::DialogBody><Plugins/></super::ui::DialogBody>
        </super::modal::Modal>
    }
}

/// Marketplace configuration stays separate from discovery and plugin lifecycle.
#[component]
pub fn PluginMarketplaceSources() -> impl IntoView {
    let state = expect_context::<PluginsState>();
    let actions = expect_context::<ProjectPluginActions>();
    let ui = expect_context::<crate::state::ui::UiState>();
    let settings = expect_context::<crate::state::settings::SettingsState>();
    view! {
        <FormSection title="Plugin marketplaces" description="Configure the public Git repositories used to discover plugins.">
            <Show when=move ||state.busy.get()><p class="form-hint" role="status">"Working…"</p></Show>
            <Show when=move ||state.error.get().is_some()><p class="error" role="alert">{move ||state.error.get().unwrap_or_default()}</p></Show>
            <div class="ui-section-content">
                <p class="form-hint">"Each catalog lists plugins in its own public Git repository. Leave the reference empty to follow its default branch."</p>
                <For each=move ||state.marketplaces.get().sources key=|s|(s.repository.clone(),s.reference.clone(),s.path.clone()) children=move |source| {
                    let official=source==MarketplaceSource::official();let remove=StoredValue::new(source.clone());
                    view!{<div class="ui-section-content"><p class="form-hint">{format!("{} · {} · {}",source.repository,if source.reference.is_empty(){"default branch"}else{&source.reference},source.path)}</p><Show when=move ||official><p class="form-hint">"Official marketplace · always available"</p></Show><Show when=move ||!official><InlineActions><Button disabled=state.busy.read_only() on_click=Callback::new(move |_|{let mut sources=state.marketplaces.get_untracked().sources;sources.retain(|s|s!=&remove.get_value());actions.save_sources.run(sources);})>"Remove marketplace"</Button></InlineActions></Show></div>}
                }/>
                <FormField label="Repository URL"><TextInput label="Marketplace repository URL" value=state.marketplace_repository.read_only() on_change=Callback::new(move |v|state.marketplace_repository.set(v)) maxlength=2048 disabled=state.busy.read_only()/></FormField>
                <FormField label="Reference"><TextInput label="Marketplace reference" value=state.marketplace_reference.read_only() on_change=Callback::new(move |v|state.marketplace_reference.set(v)) maxlength=256 disabled=state.busy.read_only()/></FormField>
                <FormField label="Catalog path"><TextInput label="Marketplace catalog path" value=state.marketplace_path.read_only() on_change=Callback::new(move |v|state.marketplace_path.set(v)) maxlength=512 disabled=state.busy.read_only()/></FormField>
                <InlineActions><Button disabled=state.busy.read_only() on_click=Callback::new(move |_|{let mut sources=state.marketplaces.get_untracked().sources;sources.push(MarketplaceSource{repository:state.marketplace_repository.get_untracked().trim().into(),reference:state.marketplace_reference.get_untracked().trim().into(),path:state.marketplace_path.get_untracked().trim().into()});actions.save_sources.run(sources);})><Icon name=IconName::Plus/>"Add marketplace"</Button></InlineActions>
            </div>

            <InlineActions><Button variant=ButtonVariant::Ghost on_click=Callback::new(move |_| {
                settings.show_settings.set(false);
                ui.plugins_open.set(true);
            })>"Browse plugins"</Button></InlineActions>
        </FormSection>
    }
}

fn filtered_installations(state: PluginsState) -> Vec<PluginInstallation> {
    let query = state.search.get().trim().to_lowercase();
    state.installations.with(|entries| {
        entries
            .iter()
            .filter(|entry| {
                let manifest = &entry.prepared.manifest;
                format!(
                    "{} {} {} {}",
                    manifest.publisher, manifest.name, manifest.display_name, manifest.description
                )
                .to_lowercase()
                .contains(&query)
            })
            .cloned()
            .collect()
    })
}
fn filtered_catalog(state: PluginsState, catalog: &CachedMarketplace) -> Vec<CatalogPlugin> {
    let query = state.search.get().trim().to_lowercase();
    catalog
        .catalog
        .plugins
        .iter()
        .filter(|plugin| {
            !state.installations.with(|entries| {
                entries.iter().any(|entry| {
                    entry.prepared.source.repository == catalog.source.repository
                        && entry.prepared.manifest.publisher == plugin.publisher
                        && entry.prepared.manifest.name == plugin.name
                })
            }) && format!(
                "{} {} {} {}",
                plugin.publisher, plugin.name, plugin.display_name, plugin.description
            )
            .to_lowercase()
            .contains(&query)
        })
        .cloned()
        .collect()
}
fn available_plugins(state: PluginsState) -> Vec<(CachedMarketplace, CatalogPlugin)> {
    state
        .marketplaces
        .get()
        .catalogs
        .into_iter()
        .flat_map(|catalog| {
            filtered_catalog(state, &catalog)
                .into_iter()
                .map(move |plugin| (catalog.clone(), plugin))
        })
        .collect()
}
fn marketplace_name(catalog: &CachedMarketplace) -> String {
    if catalog.source == MarketplaceSource::official() {
        "Open WebIDE".into()
    } else {
        catalog.catalog.name.clone()
    }
}
#[component]
fn PluginSection(
    title: &'static str,
    #[prop(into)] count: Signal<usize>,
    #[prop(into, optional)] header_actions: Option<ViewFn>,
    children: Children,
) -> impl IntoView {
    let actions = ViewFn::from(move || {
        view! {
            {header_actions.as_ref().map(ViewFn::run)}
            <span class="plugin-count">{move ||count.get()}</span>
        }
    });
    view! {<DisclosurePanel initially_open=true class="plugin-section" header_actions=Some(actions) summary=move ||view!{<span class="plugin-section-title">{title}</span>}>
        {children()}
    </DisclosurePanel>}
}
#[component]
fn PluginRow(
    name: String,
    description: String,
    publisher: String,
    #[prop(into)] source: Signal<String>,
    #[prop(into)] version: Signal<String>,
    #[prop(into)] status: Signal<String>,
    on_details: Callback<()>,
    children: Children,
) -> impl IntoView {
    let description_title = description.clone();
    view! {<div class="plugin-row">
        <span class="plugin-symbol"><Icon name=IconName::Puzzle/></span>
        <div class="plugin-info">
            <button type="button" class="btn ghost plugin-name" on:click=move |_|on_details.run(())>{name}</button>
            <p class="plugin-description" title=description_title>{description}</p>
            <p class="plugin-metadata"><span>{publisher}</span><span>{move ||version.get()}</span><span>{move ||status.get()}</span><Show when=move ||!source.get().is_empty()><span>{move ||format!("Source: {}", source.get())}</span></Show></p>
        </div>
        <div class="plugin-row-actions">{children()}</div>
    </div>}
}
#[component]
fn CatalogPackage(catalog: CachedMarketplace, plugin: CatalogPlugin) -> impl IntoView {
    let state = expect_context::<PluginsState>();
    let actions = expect_context::<ProjectPluginActions>();
    let details = RwSignal::new(false);
    let versions = RwSignal::new(false);
    let version = RwSignal::new(plugin.releases[0].version.clone());
    let options = StoredValue::new(
        plugin
            .releases
            .iter()
            .map(|r| SelectOption::new(&r.version, &r.version))
            .collect::<Vec<_>>(),
    );
    let inspect = StoredValue::new((catalog, plugin.clone()));
    let selected = Signal::derive(move || {
        inspect.with_value(|(catalog, plugin)| {
            catalog
                .catalog
                .resolve(
                    &catalog.source,
                    &plugin.publisher,
                    &plugin.name,
                    &version.get(),
                )
                .ok()
        })
    });
    let installed = Signal::derive(move || {
        selected.get().is_some_and(|source| {
            state
                .installations
                .with(|entries| entries.iter().any(|entry| entry.prepared.source == source))
        })
    });
    let install = Callback::new(move |_| {
        inspect.with_value(|(catalog, plugin)| {
            actions.install_release.run(CatalogSelection {
                marketplace: catalog.source.clone(),
                publisher: plugin.publisher.clone(),
                name: plugin.name.clone(),
                version: version.get_untracked(),
            });
        });
    });
    view! {<div class="plugin-entry">
        <PluginRow name=plugin.display_name description=plugin.description publisher=plugin.publisher source=Signal::derive(move ||inspect.with_value(|(catalog,_)|marketplace_name(catalog))) version=version.read_only()
            status=Signal::derive(move ||if installed.get(){"Installed".into()}else{String::new()}) on_details=Callback::new(move |()|details.update(|open| *open = !*open))>
            <Button size=ButtonSize::Sm disabled=Signal::derive(move ||state.busy.get()||installed.get()) on_click=install><Icon name=IconName::Download/>{move ||if installed.get(){"Installed"}else{"Install"}}</Button>
            <ActionMenu aria_label="Available plugin actions" icon=IconName::Settings>
                <button type="button" role="menuitem" class="ui-dropdown-item recent-item" on:click=move |_|versions.update(|open| *open = !*open)><Icon name=IconName::GitBranch/><span>"Choose release"</span></button>
                <button type="button" role="menuitem" class="ui-dropdown-item recent-item" disabled=move ||state.busy.get()||installed.get() on:click=move |event|install.run(event)><Icon name=IconName::Download/><span>"Install on host"</span></button>
            </ActionMenu>
        </PluginRow>
        <Show when=move ||versions.get()><div class="plugin-details ui-section-content">
            <FormField label="Release"><DropdownSelect label="Plugin release" value=version.read_only() options=Signal::derive(move ||options.get_value()) on_change=Callback::new(move |v|version.set(v)) disabled=state.busy.read_only()/></FormField>
        </div></Show>
        <Show when=move ||details.get()><div class="plugin-details ui-section-content">
            <p class="form-hint">{inspect.with_value(|(_,p)|p.description.clone())}</p>
            <p class="form-hint">{move ||selected.get().map(|s|format!("{} · {} · {}",s.repository,s.path,s.commit))}</p>
        </div></Show>
    </div>}
}
#[component]
fn InstalledPackage(entry: PluginInstallation) -> impl IntoView {
    let state = expect_context::<PluginsState>();
    let actions = expect_context::<ProjectPluginActions>();
    let projects = expect_context::<ProjectsState>();
    let manifest = entry.prepared.manifest.clone();
    let entry = StoredValue::new(entry);
    let details = RwSignal::new(false);
    let confirming = RwSignal::new(false);
    let preferences = RwSignal::new(false);
    let versions = RwSignal::new(false);
    let version = RwSignal::new(manifest.version.clone());
    let release_catalog = Signal::derive(move || {
        state.marketplaces.with(|marketplaces| {
            entry.with_value(|entry| {
                marketplaces.catalogs.iter().find_map(|catalog| {
                    (catalog.source.repository == entry.prepared.source.repository)
                        .then(|| {
                            catalog
                                .catalog
                                .plugins
                                .iter()
                                .find(|plugin| {
                                    plugin.publisher == entry.prepared.manifest.publisher
                                        && plugin.name == entry.prepared.manifest.name
                                })
                                .map(|plugin| (catalog.source.clone(), plugin.clone()))
                        })
                        .flatten()
                })
            })
        })
    });
    let update = Signal::derive(move || {
        state.updates().into_iter().find(|update| {
            entry.with_value(|entry| {
                update.installation.prepared.source.repository == entry.prepared.source.repository
                    && update.installation.prepared.source.path == entry.prepared.source.path
            })
        })
    });
    let binding = Signal::derive(move || {
        state.project_plugins.with(|entries| {
            entry.with_value(|installed| {
                entries
                    .iter()
                    .find(|e| {
                        e.prepared.source.repository == installed.prepared.source.repository
                            && e.prepared.source.path == installed.prepared.source.path
                    })
                    .cloned()
            })
        })
    });
    let enabled = Signal::derive(move || binding.get().is_some_and(|b| b.enabled));
    let requires_update = Signal::derive(move || {
        entry.with_value(|entry| entry.prepared.manifest.requires_update())
            || binding.get().is_some_and(|binding| {
                binding.enabled && binding.prepared.manifest.requires_update()
            })
    });
    let current = Signal::derive(move || {
        binding.get().is_some_and(|b| {
            b.enabled && entry.with_value(|e| b.prepared.source == e.prepared.source)
        })
    });
    view! {<div class="plugin-entry">
        <PluginRow name=manifest.display_name description=manifest.description publisher=manifest.publisher
            source=Signal::derive(move ||state.marketplaces.with(|marketplaces|entry.with_value(|entry|marketplaces.catalogs.iter().find(|catalog|catalog.source.repository==entry.prepared.source.repository).map(marketplace_name).unwrap_or_default())))
            version=Signal::derive(move ||entry.with_value(|e|e.prepared.manifest.version.clone()))
            status=Signal::derive(move ||if requires_update.get(){"Update required".into()}else{binding.get().map_or_else(||entry.with_value(|entry|if entry.default_enabled{"Enabled by default"}else{"Activation pending"}.into()),|b|format!("{} in this project: {}",if b.enabled{"Enabled"}else{"Disabled"},b.prepared.manifest.version))})
            on_details=Callback::new(move |()|details.update(|open| *open = !*open))>
            <Show when=move ||projects.active_project.get().is_some()&&binding.get().is_some()>
            <Show when=move ||enabled.get() fallback=move ||view!{<Button size=ButtonSize::Sm disabled=Signal::derive(move ||state.busy.get()||projects.active_project.get().is_none()||entry.with_value(|entry|entry.prepared.manifest.requires_update())) on_click=Callback::new(move |_|actions.enable.run(entry.get_value()))>"Enable"</Button>}>
                <Button size=ButtonSize::Sm disabled=state.busy.read_only() on_click=Callback::new(move |_|{if let Some(binding)=binding.get_untracked(){actions.disable.run(binding);}})>"Disable"</Button>
            </Show>
            </Show>
            <ActionMenu aria_label="Installed plugin actions" icon=IconName::Settings>
                <Show when=move ||update.get().is_some()><button type="button" role="menuitem" class="ui-dropdown-item recent-item" disabled=move ||state.busy.get() on:click=move |_| {
                    if let Some(update)=update.get_untracked(){actions.install_release.run(CatalogSelection{marketplace:update.marketplace,publisher:update.installation.prepared.manifest.publisher,name:update.installation.prepared.manifest.name,version:update.version});}
                }><Icon name=IconName::Download/><span>{move ||update.get().map(|update|format!("Update to {}",update.version))}</span></button></Show>
                <button type="button" role="menuitem" class="ui-dropdown-item recent-item" on:click=move |_|preferences.update(|open| *open = !*open)><Icon name=IconName::Bell/><span>"Update preferences"</span></button>
                <button type="button" role="menuitem" class="ui-dropdown-item recent-item" disabled=move ||release_catalog.get().is_none() on:click=move |_|versions.update(|open| *open = !*open)><Icon name=IconName::GitBranch/><span>"Choose release"</span></button>
                <button type="button" role="menuitem" class="ui-dropdown-item recent-item" disabled=move ||state.busy.get()||projects.active_project.get().is_none()||current.get()||entry.with_value(|entry|entry.prepared.manifest.requires_update()) on:click=move |_|actions.enable.run(entry.get_value())><Icon name=IconName::Check/><span>{move ||if enabled.get(){"Apply installed version"}else{"Enable for project"}}</span></button>
                <button type="button" role="menuitem" class="ui-dropdown-item recent-item" disabled=move ||state.busy.get()||!enabled.get() on:click=move |_|{if let Some(binding)=binding.get_untracked(){actions.disable.run(binding);}}><Icon name=IconName::Pause/><span>"Disable for project"</span></button>
                <button type="button" role="menuitem" class="ui-dropdown-item recent-item" disabled=move ||state.busy.get() on:click=move |_|confirming.set(true)><Icon name=IconName::Trash2/><span>"Uninstall"</span></button>
            </ActionMenu>
        </PluginRow>
        <Show when=move ||requires_update.get()><p class="form-hint">"This version uses retired built-in tool groups. Choose an executable release, then apply it to this project. Your data and update preferences are retained."</p></Show>
        <Show when=move ||update.get().is_some()><p class="form-hint">{move ||update.get().map(|update|format!("Update available: {}",update.version))}</p></Show>
        <Show when=move ||preferences.get()><div class="plugin-details ui-section-content"><FormField label="Updates"><DropdownSelect label="Plugin update policy" value=Signal::derive(move ||entry.with_value(|entry|match entry.update_policy {PluginUpdatePolicy::Notify=>"notify",PluginUpdatePolicy::Automatic=>"automatic",PluginUpdatePolicy::Off=>"off"}.to_string())) options=Signal::derive(||vec![SelectOption::new("notify","Notify"),SelectOption::new("automatic","Automatic"),SelectOption::new("off","Off")]) on_change=Callback::new(move |value: String|{
            let policy=match value.as_str(){"automatic"=>PluginUpdatePolicy::Automatic,"off"=>PluginUpdatePolicy::Off,_=>PluginUpdatePolicy::Notify};
            actions.set_update_policy.run((entry.get_value(),policy));
        }) disabled=state.busy.read_only()/></FormField><p class="form-hint">"Notify shows available updates. Automatic applies compatible releases while the app is open. Off stops notifications and automatic updates."</p></div></Show>
        <Show when=move ||versions.get()><div class="plugin-details ui-section-content">
            <FormField label="Release"><DropdownSelect label="Plugin release" value=version.read_only() options=Signal::derive(move ||release_catalog.get().map_or_else(Vec::new,|(_,plugin)|plugin.releases.iter().map(|release|SelectOption::new(&release.version,&release.version)).collect())) on_change=Callback::new(move |value|version.set(value)) disabled=state.busy.read_only()/></FormField>
            <InlineActions><Button size=ButtonSize::Sm disabled=Signal::derive(move ||state.busy.get()||release_catalog.get().is_none()||entry.with_value(|entry|entry.prepared.manifest.version==version.get())) on_click=Callback::new(move |_|{
                if let Some((marketplace,plugin))=release_catalog.get_untracked(){actions.install_release.run(CatalogSelection{marketplace,publisher:plugin.publisher,name:plugin.name,version:version.get_untracked()});}
            })><Icon name=IconName::Download/>"Install selected release"</Button></InlineActions>
        </div></Show>
        <Show when=move ||details.get()><div class="plugin-details ui-section-content">
            <p class="form-hint">{entry.with_value(|e|format!("{} · {} · {} · {} host(s)",e.prepared.source.repository,e.prepared.source.path,e.prepared.source.commit,e.hosts.len()))}</p>
            <p class="form-hint">{entry.with_value(|e|format!("Skills: {}",e.prepared.manifest.contributions.skills.iter().map(|s|s.path.clone()).collect::<Vec<_>>().join(", ")))}</p>
        </div></Show>
        <Show when=move ||confirming.get()><div class="plugin-details ui-section-content"><p class="form-hint">"Uninstall removes this plugin’s managed skills from all your projects. Host caches remain available to running tasks."</p><InlineActions><Button variant=ButtonVariant::Danger disabled=state.busy.read_only() on_click=Callback::new(move |_|actions.remove.run(entry.get_value()))>"Confirm uninstall"</Button><Button variant=ButtonVariant::Ghost on_click=Callback::new(move |_|confirming.set(false))>"Cancel"</Button></InlineActions></div></Show>
    </div>}
}
