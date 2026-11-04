//! Shared lifecycle decisions; the captured adapter can revoke an old service
//! after folder mapping or provider selection changes, without reopening it.
use super::*;
use crate::project_host::{PluginExecutionHost, ProjectHost};
use openwebide_core::plugins::languages::{LanguageSelection, intelligence::Revoke};
use std::collections::BTreeMap;

#[derive(Clone)]
struct Control {
    user: i64,
    project: i64,
    account: u64,
    revision: Option<u64>,
    transport: PluginExecutionHost,
    providers: BTreeMap<String, String>,
}
pub(super) fn install(
    actions: ProjectLanguageActions,
    auth: AuthState,
    projects: ProjectsState,
    host: ProjectHost,
) {
    let previous = StoredValue::new_local(BTreeMap::<(u64, i64), Control>::new());
    Effect::new(move |_| {
        let account = auth.generation.get();
        let user = auth
            .user
            .with(|user| user.as_ref().map(|user| user.id.get()));
        let project = projects.active_project.get();
        host.track_revision();
        let revision = host.revision();
        let ready = actions.ready.get();
        let registry = actions.registry.get();
        let preferences = actions.preferences.get();
        let mut current = previous.get_value();
        let mut revoked = Vec::new();
        current.retain(|_, control| {
            if control.account != account
                || control.revision != revision
                || user != Some(control.user)
            {
                revoked.push((control.clone(), None));
                false
            } else {
                true
            }
        });
        if let (Some(user), Some(project), true) = (user, project, ready) {
            let providers = registry.providers.iter().filter(|provider| {
                matches!(registry.select_server("", "", Some(&provider.definition.id), &preferences), LanguageSelection::Selected(selected) if selected == *provider)
            }).map(|provider| (provider.plugin_namespace.clone(), provider.digest.clone())).collect::<BTreeMap<_, _>>();
            if let Some(control) = current.get(&(account, project)) {
                for (provider, digest) in &control.providers {
                    if providers.get(provider) != Some(digest) {
                        revoked.push((control.clone(), Some(provider.clone())));
                    }
                }
            }
            if let Ok(transport) = host.plugin_host(Some(project), || true) {
                current.insert(
                    (account, project),
                    Control {
                        user,
                        project,
                        account,
                        revision,
                        transport,
                        providers,
                    },
                );
            }
        }
        previous.set_value(current);
        for (control, provider) in revoked {
            spawn_local(async move {
                let _ = control
                    .transport
                    .revoke_language_servers(&Revoke {
                        user: Some(control.user),
                        project: Some(control.project),
                        provider,
                    })
                    .await;
            });
        }
    });
}
