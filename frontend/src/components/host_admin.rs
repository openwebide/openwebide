use super::ui::{FormField, FormSection, InlineActions};
use crate::host_admin::HostState;
use leptos::prelude::*;
use openwebide_core::host_admin::{HostConnection, HostPlatform};

#[component]
pub fn HostSettings() -> impl IntoView {
    let host = expect_context::<HostState>();
    let draft = RwSignal::new(HostConnection::default());
    host.load_connection();
    Effect::new(move || {
        if let Some(connection) = host.connection.get() {
            draft.set(connection);
        }
    });
    view! {
        <FormSection title="Host administration" description="Allow project-less chat to manage the machine running Open WebIDE. Enter your SSH username; the address and operating system are discovered automatically.">
            <super::ui::CheckboxField label="Enable host administration" checked=Signal::derive(move||draft.with(|value|value.enabled)) disabled=Signal::derive(move||host.connection_busy.get() || host.connection.get().is_none()) on_change=Callback::new(move|enabled|{draft.update(|value|value.enabled=enabled); let connection=draft.get_untracked(); if !enabled || connection.is_configured(){host.save_connection(connection);}})/>
            <FormField label="SSH username"><input class="form-input" type="text" aria-label="Administration SSH username" autocomplete="username" placeholder="Your host account" prop:value=move||draft.with(|value|value.username.clone()) on:input=move|event|draft.update(|value|value.username=event_target_value(&event))/></FormField>
            <p class="form-hint">"The host needs an SSH server reachable from the bridge (usually port 22), with an authorized key for this account. SSH uses the bridge's existing keys or forwarded agent."</p>
            <super::ui::DisclosurePanel summary=move||view!{ "Advanced connection settings" }>
                <FormField label="SSH address override"><input class="form-input" type="text" aria-label="Administration SSH host" placeholder="Automatic" prop:value=move||draft.with(|value|value.destination.clone()) on:input=move|event|draft.update(|value|value.destination=event_target_value(&event))/></FormField>
                <p class="form-hint">"Leave blank to use the host automatically. Override with a hostname, SSH alias or user@hostname."</p>
                <FormField label="SSH port"><input class="form-input" type="number" min="1" max="65535" aria-label="Administration SSH port" prop:value=move||draft.with(|value|value.port.to_string()) on:change=move|event|{if let Ok(port)=event_target_value(&event).parse(){draft.update(|value|value.port=port);}}/></FormField>
                <FormField label="Host operating system override" group=true><super::ui::SegmentedControl options=vec![super::ui::SegmentOption::new("Automatic",HostPlatform::Auto),super::ui::SegmentOption::new("Linux",HostPlatform::Linux),super::ui::SegmentOption::new("macOS",HostPlatform::Macos),super::ui::SegmentOption::new("Windows",HostPlatform::Windows)] value=Signal::derive(move||draft.with(|value|value.platform)) on_change=Callback::new(move|platform|draft.update(|value|value.platform=platform))/></FormField>
                <FormField label="Pinned public host keys"><textarea class="form-input" rows="3" aria-label="Administration SSH known hosts" placeholder="Optional verified known_hosts entries" prop:value=move||draft.with(|value|value.known_hosts.clone()) on:input=move|event|draft.update(|value|value.known_hosts=event_target_value(&event))/></FormField>
                <p class="form-hint">"Uses the bridge's verified known_hosts file by default. For a new host, verify its public keys through a trusted channel before adding them here."</p>
            </super::ui::DisclosurePanel>
            <InlineActions><button class="btn" disabled=move||host.connection_busy.get() || host.connection.get().is_none() || draft.with(|value|value.validate().is_err()) on:click=move|_|host.save_and_test_connection(draft.get_untracked())>{move||if draft.with(|value|value.enabled){"Save and test connection"}else{"Save settings"}}</button></InlineActions>
            <Show when=move||host.connection_busy.get()><p class="form-hint" role="status">"Saving and checking SSH access…"</p></Show>
            <Show when=move||host.connection_error.get().is_some()><p class="form-hint" role="alert">{move||host.connection_error.get().unwrap_or_default()}</p></Show>
            <Show when=move||host.connection_status.get().is_some()><p class="form-hint" role="status">{move||host.connection_status.get().unwrap_or_default()}</p></Show>
        </FormSection>
    }
}
#[component]
pub fn HostPanel() -> impl IntoView {
    let host = expect_context::<HostState>();
    let chat = expect_context::<crate::state::chat::ChatState>();
    let settings = expect_context::<crate::state::settings::SettingsState>();
    let auth = expect_context::<crate::state::auth::AuthState>();
    Effect::new(move || {
        auth.generation.track();
        host.load_connection();
    });
    let configured = Memo::new(move |_| {
        host.connection.with(|connection| {
            connection
                .as_ref()
                .is_some_and(HostConnection::is_configured)
        })
    });
    let filter = RwSignal::new(String::new());
    let environment = Memo::new(move |_| host.environment.get());
    let ticks = std::cell::Cell::new(0_u8);
    let timer = set_interval_with_handle(
        move || {
            if host.open.get_untracked() && configured.get_untracked() {
                ticks.set((ticks.get() + 1) % 5);
                host.refresh(ticks.get() == 0);
            }
        },
        std::time::Duration::from_secs(2),
    )
    .ok();
    on_cleanup(move || {
        if let Some(timer) = timer {
            timer.clear();
        }
    });
    view! {
        <super::dropdown::Dropdown class="tui-host-status" trigger_class="btn sm ghost" menu_class="tui-host-menu" menu_role="dialog" preferred_width=640.0 max_height=640.0 aria_label="Host environment" open=host.open above=true hide_caret=true on_open=Callback::new(move |()| host.refresh(true)) label=|| view! { <span>"Host"</span> }>
        <section class="host-panel" class:host-panel-configured=move||configured.get() aria-label="Host environment details">
        <Show when=move||configured.get() fallback=move||view!{
            <p class="form-hint">"Set up host administration to inspect and manage your homelab."</p>
            <button class="btn ghost" on:click=move|_|{host.open.set(false); settings.requested_tab.set(4); settings.show_settings.set(true);}>"Open host settings"</button>
        }>
            <InlineActions><strong>"Host environment"</strong><button class="btn ghost" aria-label="Refresh host environment" title="Refresh host environment" disabled=move||host.busy.get() || chat.active_session.get().is_none() on:click=move|_|host.refresh(true)><super::ui::Icon name=super::ui::IconName::RefreshCw /></button></InlineActions>
            <Show when=move||host.open.get()>
                <Show when=move||chat.active_session.get().is_none()><p class="form-hint">"Start a project-less chat to inspect and manage the host."</p></Show>
                <Show when=move||host.error.get().is_some()><p class="form-hint" role="alert">{move||host.error.get().unwrap_or_default()}</p></Show>
                <div class="host-summary">
                    <p><strong>{move||environment.with(|value|value.as_ref().map(|host|host.target.clone()).unwrap_or_else(||"Loading host…".into()))}</strong>" · "{move||environment.with(|value|value.as_ref().and_then(|host|host.info.os.clone()).unwrap_or_default())}</p>
                    <details class="ui-disclosure"><summary>"Connection details"</summary><p class="form-hint">{move||environment.with(|value|value.as_ref().map(|host|host.info.scope.clone()).unwrap_or_default())}</p></details>
                    <p class="form-hint">{move||environment.with(|value|value.as_ref().map(|host|format!("{} logical CPUs · {} / {} MiB RAM available",host.info.logical_cores,host.info.ram_available_bytes/1048576,host.info.ram_total_bytes/1048576)).unwrap_or_default())}</p>
                </div>
                <Show when=move||environment.with(|value|value.as_ref().is_some_and(|host|!host.notes.is_empty()))><details class="ui-disclosure"><summary>{move||environment.with(|value|format!("Collection notes ({})",value.as_ref().map_or(0,|host|host.notes.len())))}</summary><For each=move||environment.with(|value|value.as_ref().map(|host|host.notes.clone()).unwrap_or_default()) key=|note|note.clone() children=|note|view!{<p class="form-hint">{note}</p>}/></details></Show>
                <p class="form-hint">"Select a resource to expand its details. Ask about this adds it to your chat draft."</p>
                <super::ui::PanelSearchRow unpadded=true><input class="form-input" type="search" aria-label="Filter host resources" placeholder="Filter services, processes, ports, storage…" prop:value=move||filter.get() on:input=move|event|filter.set(event_target_value(&event))/></super::ui::PanelSearchRow>
                <div class="host-resources"><For each=move || {
                    let query = filter.get().to_lowercase();
                    environment.with(|value| value.as_ref().map(|environment| environment.resources.iter().filter(|resource| format!("{} {} {}", resource.name, resource.detail, resource.status).to_lowercase().contains(&query)).take(100).map(|resource| resource.id.clone()).collect::<Vec<_>>()).unwrap_or_default())
                } key=|id| id.clone() children=move |id| view! { <HostResourceView id=id /> } /></div>
                <p class="form-hint">"Showing up to 100 matching resources. Filter to narrow the view."</p>
                <For each=move||host.operations.with(|operations|operations.iter().map(|operation|operation.id).collect::<Vec<_>>()) key=|id|*id children=move|id|view!{<HostOperationView id=id/>}/>
            </Show>
        </Show>
        </section>
        </super::dropdown::Dropdown>
    }
}
#[component]
fn HostResourceView(id: String) -> impl IntoView {
    let host = expect_context::<HostState>();
    let id = StoredValue::new(id);
    let resource = Memo::new(move |_| {
        host.environment.with(|value| {
            value.as_ref().and_then(|environment| {
                environment
                    .resources
                    .iter()
                    .find(|resource| resource.id == id.get_value())
                    .cloned()
            })
        })
    });
    let links = Memo::new(move |_| {
        host.environment.with(|value| {
            value
                .as_ref()
                .map(|environment| {
                    environment
                        .relationships
                        .iter()
                        .filter(|relation| {
                            relation.from == id.get_value() || relation.to == id.get_value()
                        })
                        .map(|relation| {
                            let name = |id: &str| {
                                environment
                                    .resources
                                    .iter()
                                    .find(|resource| resource.id == id)
                                    .map_or_else(
                                        || id.to_owned(),
                                        |resource| format!("{} ({id})", resource.name),
                                    )
                            };
                            format!(
                                "{} → {} ({})",
                                name(&relation.from),
                                name(&relation.to),
                                relation.description
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        })
    });
    view! {
        <details class="host-resource">
            <summary>{move || resource.get().map(|resource| format!("{:?}: {} · {}", resource.kind, resource.name, resource.status))}</summary>
            <div class="host-resource-details">
            <strong>"Observed details"</strong><pre>{move || resource.get().map(|resource| resource.detail)}</pre>
            <Show when=move||!links.get().is_empty()><strong>"Related resources"</strong><pre>{move || links.get()}</pre></Show>
            <button class="btn ghost" on:click=move |_| { if let Some(resource) = resource.get_untracked() { host.ask(&resource); } }>"Ask about this"</button>
            </div>
        </details>
    }
}
#[component]
fn HostOperationView(id: i64) -> impl IntoView {
    let host = expect_context::<HostState>();
    let operation = Memo::new(move |_| {
        host.operations.with(|operations| {
            operations
                .iter()
                .find(|operation| operation.id == id)
                .cloned()
        })
    });
    let reply = RwSignal::new(String::new());
    let input_token = Memo::new(move |_| {
        operation.with(|value| {
            value
                .as_ref()
                .and_then(|operation| operation.input_token.clone())
        })
    });
    Effect::new(move || {
        input_token.track();
        reply.set(String::new());
    });
    let send = Callback::new(move |()| {
        if let Some(operation) = operation.get_untracked()
            && let Some(token) = operation.input_token
        {
            let data = reply.get_untracked();
            reply.set(String::new());
            host.reply(id, token, format!("{data}\n"));
        }
    });
    view! {
        <details class="host-operation"><summary>{move||operation.with(|value|value.as_ref().map(|operation|format!("#{} {} · {:?}",id,operation.plan.title,operation.state)).unwrap_or_default())}</summary>
            <p class="form-hint">{move||operation.with(|value|value.as_ref().map(|operation|operation.detail.clone()).unwrap_or_default())}</p>
            <pre class="host-output">{move||operation.with(|value|value.as_ref().map(|operation|operation.live_output.clone()).unwrap_or_default())}</pre>
            <Show when=move||operation.with(|value|value.as_ref().is_some_and(|operation|operation.input_token.is_some()))>
                <label>"Private terminal reply"<input class="form-input" type="password" autocomplete="off" aria-label="Private host prompt reply" prop:value=move||reply.get() on:input=move|event|reply.set(event_target_value(&event)) on:keydown=move|event: web_sys::KeyboardEvent|{if event.key()=="Enter"{event.prevent_default();send.run(());}}/></label>
                <InlineActions><button class="btn" on:click=move|_|send.run(())>"Send reply"</button><button class="btn stop" on:click=move|_|{if let Some(operation)=operation.get_untracked() && let Some(token)=operation.input_token{host.reply(id,token,"\u{3}".into());}}>"Interrupt command"</button></InlineActions>
                <p class="form-hint">"Replies go directly to the terminal and are hidden from chat and the model."</p>
            </Show>
            <details><summary>"Approved plan and retained results"</summary><pre>{move||operation.with(|value|value.as_ref().map(|operation|operation.plan.preview()).unwrap_or_default())}</pre><pre>{move||operation.with(|value|value.as_ref().map(|operation|operation.outputs.iter().map(|result|result.outcome.to_model_text()).collect::<Vec<_>>().join("\n")).unwrap_or_default())}</pre></details>
        </details>
    }
}
