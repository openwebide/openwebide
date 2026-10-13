use super::ui::{FormField, FormSection, Icon, IconButton, IconName, InlineActions};
use crate::host_admin::HostState;
use leptos::prelude::*;
use openwebide_core::host_admin::updates::{
    Deployment, ReleaseChannel, UpdatePhase, UpdateRequest, UpdateSettings,
};

#[component]
pub fn HostUpdates() -> impl IntoView {
    let host = expect_context::<HostState>();
    let updates = host.updates;
    let draft = RwSignal::new(UpdateSettings::default());
    updates.request(UpdateRequest::Settings);
    Effect::new(move || draft.set(updates.settings.get()));
    let attempted_discovery = RwSignal::new(false);
    Effect::new(move || {
        if updates.running.get().is_some() && !updates.busy.get() && !attempted_discovery.get() {
            attempted_discovery.set(true);
            if matches!(updates.settings.get().deployment, Deployment::Unconfigured) {
                updates.request(UpdateRequest::Discover);
            }
        }
    });
    Effect::new(move || {
        if let Some(deployment) = updates.discovered.get() {
            draft.update(|value| value.deployment = deployment);
        }
    });
    let active = Memo::new(move |_| updates.tracking.get());
    let timer = set_interval_with_handle(
        move || {
            if active.get_untracked() {
                updates.request(UpdateRequest::Status);
            }
        },
        std::time::Duration::from_secs(3),
    )
    .ok();
    on_cleanup(move || {
        if let Some(timer) = timer {
            timer.clear();
        }
    });
    view! {
        <FormSection title="Updates" description="Install a published release on this host. All projects and devices use the same deployment and release channel.">
            <InlineActions>
                <span class="form-hint">{move||updates.running.with(|build|build.as_ref().map(|build|format!("Running {} · {}",build.version,build.commit.chars().take(8).collect::<String>())).unwrap_or_else(||"Loading build…".into()))}</span>
                <IconButton label="Check for updates" disabled=Signal::derive(move||updates.busy.get() || active.get()) on_click=Callback::new(move|_|updates.request(UpdateRequest::Check))><Icon name=IconName::RefreshCw/></IconButton>
            </InlineActions>
            <FormField label="Release channel" group=true>
                <super::ui::SegmentedControl options=vec![super::ui::SegmentOption::new("Nightly",ReleaseChannel::Nightly),super::ui::SegmentOption::new("Preview",ReleaseChannel::Preview),super::ui::SegmentOption::new("Stable",ReleaseChannel::Stable)] value=Signal::derive(move||draft.with(|value|value.channel)) on_change=Callback::new(move|channel|draft.update(|value|value.channel=channel))/>
            </FormField>
            <p class="form-hint">{move||draft.with(|value|match &value.deployment {
                Deployment::Unconfigured => "Installation has not been identified. Connect SSH in Host settings, then detect the installation or use the advanced override.".into(),
                Deployment::Compose { directory, .. } => format!("Docker Compose installation: {directory}"),
                Deployment::Quadlet { image_file, .. } => format!("Podman Quadlet installation: {image_file}"),
            })}</p>
            <InlineActions><button class="btn ghost" disabled=move||updates.busy.get() || active.get() on:click=move|_|updates.request(UpdateRequest::Discover)>"Detect installation"</button></InlineActions>
            <super::ui::DisclosurePanel summary=move||view!{"Advanced deployment settings"}>
                <p class="form-hint">"Uses the saved host SSH connection. Release installation requires Linux, Python 3, skopeo and a user systemd manager. Discovery uses the running container’s metadata. Review and save the detected settings before checking for updates."</p>
                <FormField label="Deployment"><select class="form-input" aria-label="Update deployment" prop:value=move||draft.with(|value|match value.deployment{Deployment::Unconfigured=>"none",Deployment::Compose{..}=>"compose",Deployment::Quadlet{..}=>"quadlet"}) on:change=move|event|draft.update(|value|value.deployment=match event_target_value(&event).as_str(){"compose"=>Deployment::Compose{directory:String::new(),files:Vec::new(),service:"openwebide".into()},"quadlet"=>Deployment::Quadlet{image_file:String::new(),service:"openwebide.service".into()},_=>Deployment::Unconfigured})><option value="none">"Not configured"</option><option value="compose">"Docker Compose"</option><option value="quadlet">"Podman / systemd"</option></select></FormField>
                <Show when=move||draft.with(|value|matches!(value.deployment,Deployment::Compose{..}))>
                    <FormField label="Installation directory"><input class="form-input" aria-label="Update installation directory" placeholder="/home/user/openwebide" prop:value=move||draft.with(|value|match &value.deployment{Deployment::Compose{directory,..}=>directory.clone(),_=>String::new()}) on:input=move|event|draft.update(|value|if let Deployment::Compose{directory,..}=&mut value.deployment{*directory=event_target_value(&event)})/></FormField>
                    <FormField label="Compose files, one absolute path per line"><textarea class="form-input" aria-label="Update Compose files" rows="3" prop:value=move||draft.with(|value|match &value.deployment{Deployment::Compose{files,..}=>files.join("\n"),_=>String::new()}) on:input=move|event|draft.update(|value|if let Deployment::Compose{files,..}=&mut value.deployment{*files=event_target_value(&event).lines().filter(|line|!line.trim().is_empty()).map(|line|line.trim().to_owned()).collect()})/></FormField>
                    <p class="form-hint">"Include HTTPS and SSH overlays in their usual order. The updater preserves .env and changes only OPENWEBIDE_IMAGE."</p>
                </Show>
                <Show when=move||draft.with(|value|matches!(value.deployment,Deployment::Quadlet{..}))>
                    <FormField label="Quadlet image file"><input class="form-input" aria-label="Update Quadlet image file" placeholder="/home/user/.config/containers/systemd/openwebide.image" prop:value=move||draft.with(|value|match &value.deployment{Deployment::Quadlet{image_file,..}=>image_file.clone(),_=>String::new()}) on:input=move|event|draft.update(|value|if let Deployment::Quadlet{image_file,..}=&mut value.deployment{*image_file=event_target_value(&event)})/></FormField>
                </Show>
                <Show when=move||draft.with(|value|!matches!(value.deployment,Deployment::Unconfigured))>
                    <FormField label="App service"><input class="form-input" aria-label="Update app service" prop:value=move||draft.with(|value|match &value.deployment{Deployment::Compose{service,..}|Deployment::Quadlet{service,..}=>service.clone(),Deployment::Unconfigured=>String::new()}) on:input=move|event|draft.update(|value|match &mut value.deployment{Deployment::Compose{service,..}|Deployment::Quadlet{service,..}=>*service=event_target_value(&event),Deployment::Unconfigured=>{}})/></FormField>
                </Show>
            </super::ui::DisclosurePanel>
            <InlineActions><button class="btn" disabled=move||updates.busy.get() || active.get() on:click=move|_|updates.request(UpdateRequest::Save{settings:draft.get_untracked()})>"Save update settings"</button></InlineActions>
            <Show when=move||updates.busy.get()><p class="form-hint" role="status">"Checking the host…"</p></Show>
            <Show when=move||updates.error.get().is_some()><p class="form-hint" role="alert">{move||updates.error.get().unwrap_or_default()}</p></Show>
            <Show when=move||updates.check.get().is_some()>
                <p class="form-hint">{move||updates.check.with(|check|check.as_ref().map(|check|if check.available{format!("Available: {} · {}",check.candidate.build.version,check.candidate.build.commit.chars().take(8).collect::<String>())}else{"This build is current.".into()}).unwrap_or_default())}</p>
                <InlineActions>
                    <a href=move||updates.check.with(|check|check.as_ref().map(|check|check.candidate.release_notes()).unwrap_or_default()) target="_blank" rel="noopener noreferrer">"Release notes"</a>
                    <Show when=move||updates.check.with(|check|check.as_ref().is_some_and(|check|check.available))>
                        <button class="btn" disabled=move||updates.busy.get() || active.get() || updates.settings.with(|settings|matches!(settings.deployment,Deployment::Unconfigured)) on:click=move|_|{if let Some(check)=updates.check.get_untracked(){updates.request(UpdateRequest::Start{candidate:check.candidate,settings_revision:check.settings.revision,connection_revision:check.connection_revision});}}><Icon name=IconName::Download/>"Install release"</button>
                    </Show>
                </InlineActions>
                <p class="form-hint">"Updating restarts Open WebIDE and interrupts active runs and terminals. Persistent data and project files are retained."</p>
            </Show>
            <Show when=move||updates.job.get().is_some()>
                <p class="form-hint" role="status">{move||updates.job.with(|job|job.as_ref().map(|job|job.detail.clone()).unwrap_or_default())}</p>
                <Show when=move||updates.job.with(|job|job.as_ref().is_some_and(|job|job.phase==UpdatePhase::Succeeded))><button class="btn ghost" on:click=move|_|{if let Some(window)=web_sys::window(){let _=window.location().reload();}}>"Reload app"</button></Show>
            </Show>
        </FormSection>
    }
}
