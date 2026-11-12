//! Account preference, separate from the public plugin provider-selection payload.
use super::*;

pub(super) fn save(
    api: Api,
    state: LanguagesState,
    auth: AuthState,
    settings: crate::state::settings::SettingsState,
    ready: Memo<bool>,
    load_ticket: StoredValue<u64>,
) -> Callback<bool> {
    let pending = StoredValue::new(None::<bool>);
    let confirmed = StoredValue::new(false);
    let saving = StoredValue::new(None::<(u64, u64)>);
    Callback::new(move |enabled: bool| {
        if !ready.get_untracked() {
            return;
        }
        let previous = state.prefetch_enabled.get_untracked();
        if enabled == previous {
            return;
        }
        let account = auth.generation.get_untracked();
        let ticket = load_ticket.get_value();
        let scope = (account, ticket);
        if saving.get_value() != Some(scope) {
            confirmed.set_value(previous);
        }
        // Turning it off cancels optional work before the database round trip.
        state.prefetch_enabled.set(enabled);
        state.prefetch_error.set(None);
        pending.set_value(Some(enabled));
        if saving.get_value() == Some(scope) {
            return;
        }
        saving.set_value(Some(scope));
        state.prefetch_saving.set(true);
        spawn_local(async move {
            while auth.generation.try_get_untracked() == Some(account)
                && load_ticket.try_get_value() == Some(ticket)
            {
                let Some(enabled) = pending.try_update_value(Option::take).flatten() else {
                    break;
                };
                let result = api
                    .with_value(Clone::clone)
                    .set_setting(
                        PREFETCH_SETTINGS_KEY,
                        if enabled { "true" } else { "false" },
                    )
                    .await;
                if auth.generation.try_get_untracked() != Some(account)
                    || load_ticket.try_get_value() != Some(ticket)
                {
                    return;
                }
                match result {
                    Ok(()) => {
                        confirmed.set_value(enabled);
                        settings.refresh_values();
                        if pending.get_value().is_none() {
                            state.prefetch_error.set(None);
                        }
                    }
                    Err(error) => {
                        if pending.get_value().is_none() {
                            state.prefetch_enabled.set(confirmed.get_value());
                        }
                        state.prefetch_error.set(Some(format!(
                            "Could not save idle language analysis: {error}"
                        )));
                    }
                }
            }
            if saving.try_get_value() == Some(Some(scope)) {
                saving.set_value(None);
                state.prefetch_saving.set(false);
            }
        });
    })
}
