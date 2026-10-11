use super::ui::{FormField, FormSection, InlineActions};
use crate::host_admin::HostState;
use leptos::prelude::*;
use openwebide_core::host_admin::{HostConnection, HostInput, HostPlatform};

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
        <FormSection title="Host administration" description="Connect project-less chat to the physical homelab machine over SSH. This connection is shared server configuration.">
            <super::ui::CheckboxField label="Enable host administration" checked=Signal::derive(move||draft.with(|value|value.enabled)) on_change=Callback::new(move|enabled|draft.update(|value|value.enabled=enabled))/>
            <FormField label="SSH host"><input class="form-input" type="text" aria-label="Administration SSH host" placeholder="user@host or SSH alias" prop:value=move||draft.with(|value|value.destination.clone()) on:input=move|event|draft.update(|value|value.destination=event_target_value(&event))/></FormField>
            <FormField label="SSH port"><input class="form-input" type="number" min="1" max="65535" aria-label="Administration SSH port" prop:value=move||draft.with(|value|value.port.to_string()) on:change=move|event|{if let Ok(port)=event_target_value(&event).parse(){draft.update(|value|value.port=port);}}/></FormField>
            <FormField label="Host operating system" group=true><super::ui::SegmentedControl options=vec![super::ui::SegmentOption::new("Linux",HostPlatform::Linux),super::ui::SegmentOption::new("macOS",HostPlatform::Macos),super::ui::SegmentOption::new("Windows",HostPlatform::Windows)] value=Signal::derive(move||draft.with(|value|value.platform)) on_change=Callback::new(move|platform|draft.update(|value|value.platform=platform))/></FormField>
            <FormField label="Pinned public host keys"><textarea class="form-input" rows="3" aria-label="Administration SSH known hosts" placeholder="Optional known_hosts entries; otherwise use the bridge's known_hosts file" prop:value=move||draft.with(|value|value.known_hosts.clone()) on:input=move|event|draft.update(|value|value.known_hosts=event_target_value(&event))/></FormField>
            <p class="form-hint">"SSH uses the bridge's existing keys or forwarded agent. Verify host keys before saving them. Save before testing. Interactive sudo replies are entered in the host operation, outside chat."</p>
            <InlineActions><button class="btn" disabled=move||host.busy.get() on:click=move|_|host.save_connection(draft.get_untracked())>"Save host connection"</button><button class="btn ghost" disabled=move||host.busy.get() || !host.connection.with(|value|value.as_ref().is_some_and(HostConnection::is_configured)) on:click=move|_|host.test_connection()>"Test connection"</button></InlineActions>
            <Show when=move||host.busy.get()><p class="form-hint" role="status">"Connecting…"</p></Show>
            <Show when=move||host.error.get().is_some()><p class="form-hint" role="alert">{move||host.error.get().unwrap_or_default()}</p></Show>
            <Show when=move||host.environment.get().is_some()><p class="form-hint" role="status">{move||host.environment.with(|value|value.as_ref().map(|host|format!("Connected to {} ({})",host.target,host.info.os.as_deref().unwrap_or("OS unavailable"))).unwrap_or_default())}</p></Show>
        </FormSection>
    }
}
#[component]
pub fn HostPanel() -> impl IntoView {
    let host = expect_context::<HostState>();
    let chat = expect_context::<crate::state::chat::ChatState>();
    let filter = RwSignal::new(String::new());
    let environment = Memo::new(move |_| host.environment.get());
    let ticks = std::cell::Cell::new(0_u8);
    let timer = set_interval_with_handle(
        move || {
            if host.open.get_untracked() {
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
        <super::dropdown::Dropdown class="tui-host-status" trigger_class="btn sm ghost" menu_class="tui-host-menu" menu_role="dialog" aria_label="Host environment" open=host.open above=true hide_caret=true on_open=Callback::new(move |()| host.refresh(true)) label=|| view! { <super::ui::Icon name=super::ui::IconName::Server /><span>"Host"</span> }>
        <section class="host-panel" aria-label="Host environment details">
            <InlineActions><strong>"Host environment"</strong><button class="btn ghost" disabled=move||host.busy.get() || chat.active_session.get().is_none() on:click=move|_|host.refresh(true)><super::ui::Icon name=super::ui::IconName::RefreshCw />"Refresh"</button></InlineActions>
            <Show when=move||host.open.get()>
                <Show when=move||chat.active_session.get().is_none()><p class="form-hint">"Start a project-less chat to inspect and manage the host."</p></Show>
                <Show when=move||host.error.get().is_some()><p class="form-hint" role="alert">{move||host.error.get().unwrap_or_default()}</p></Show>
                {move||environment.get().map(|environment|view!{
                    <p><strong>{environment.target.clone()}</strong>" · "{environment.info.os.clone().unwrap_or_default()}</p>
                    <p class="form-hint">{environment.info.scope.clone()}</p>
                    <p class="form-hint">{format!("{} logical CPUs · {} / {} MiB RAM available",environment.info.logical_cores,environment.info.ram_available_bytes/1048576,environment.info.ram_total_bytes/1048576)}</p>
                    {environment.notes.into_iter().map(|note|view!{<p class="form-hint">{note}</p>}).collect::<Vec<_>>()}
                })}
                <super::ui::PanelSearchRow unpadded=true><input class="form-input" type="search" aria-label="Filter host resources" placeholder="Filter services, processes, ports, storage…" prop:value=move||filter.get() on:input=move|event|filter.set(event_target_value(&event))/></super::ui::PanelSearchRow>
                <div class="host-resources"><For each=move || {
                    let query = filter.get().to_lowercase();
                    environment.with(|value| value.as_ref().map(|environment| environment.resources.iter().filter(|resource| format!("{} {} {}", resource.name, resource.detail, resource.status).to_lowercase().contains(&query)).take(100).map(|resource| resource.id.clone()).collect::<Vec<_>>()).unwrap_or_default())
                } key=|id| id.clone() children=move |id| view! { <HostResourceView id=id /> } /></div>
                <p class="form-hint">"Showing up to 100 matching resources. Filter to narrow the view."</p>
                <For each=move||host.operations.with(|operations|operations.iter().map(|operation|operation.id).collect::<Vec<_>>()) key=|id|*id children=move|id|view!{<HostOperationView id=id/>}/>
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
                            format!(
                                "{} → {} ({})",
                                relation.from, relation.to, relation.description
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
            <p class="form-hint">{move || resource.get().map(|resource| resource.detail)}</p>
            <pre>{move || links.get()}</pre>
            <button class="btn ghost" on:click=move |_| { if let Some(resource) = resource.get_untracked() { host.ask(&resource); } }>"Ask about this"</button>
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
            host.reply(HostInput {
                operation: id,
                token,
                data: format!("{data}\n"),
            });
        }
    });
    view! {
        <details class="host-operation"><summary>{move||operation.with(|value|value.as_ref().map(|operation|format!("#{} {} · {:?}",id,operation.plan.title,operation.state)).unwrap_or_default())}</summary>
            <p class="form-hint">{move||operation.with(|value|value.as_ref().map(|operation|operation.detail.clone()).unwrap_or_default())}</p>
            <pre class="host-output">{move||operation.with(|value|value.as_ref().map(|operation|operation.live_output.clone()).unwrap_or_default())}</pre>
            <Show when=move||operation.with(|value|value.as_ref().is_some_and(|operation|operation.input_token.is_some()))>
                <label>"Private terminal reply"<input class="form-input" type="password" autocomplete="off" aria-label="Private host prompt reply" prop:value=move||reply.get() on:input=move|event|reply.set(event_target_value(&event)) on:keydown=move|event: web_sys::KeyboardEvent|{if event.key()=="Enter"{event.prevent_default();send.run(());}}/></label>
                <InlineActions><button class="btn" on:click=move|_|send.run(())>"Send reply"</button><button class="btn stop" on:click=move|_|{if let Some(operation)=operation.get_untracked() && let Some(token)=operation.input_token{host.reply(HostInput {operation:id,token,data:"\u{3}".into()});}}>"Interrupt command"</button></InlineActions>
                <p class="form-hint">"Replies go directly to the terminal and are hidden from chat and the model."</p>
            </Show>
            <details><summary>"Approved plan and retained results"</summary><pre>{move||operation.with(|value|value.as_ref().map(|operation|operation.plan.preview()).unwrap_or_default())}</pre><pre>{move||operation.with(|value|value.as_ref().map(|operation|operation.outputs.iter().map(|result|result.outcome.to_model_text()).collect::<Vec<_>>().join("\n")).unwrap_or_default())}</pre></details>
        </details>
    }
}
