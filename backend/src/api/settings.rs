use super::*;
// -- settings ------------------------------------------------------------------

pub(crate) async fn get_settings(state: &AppState, user: AuthedUser) -> Result<JsonResp, ApiError> {
    let user_id = user.id;
    let settings = state.store.all_user_settings(user_id).await?;
    Ok(json_response(200, &settings))
}

#[derive(Deserialize)]
pub(super) struct SettingBody {
    key: String,
    value: String,
}

pub(crate) async fn set_setting(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let body = read_body(req, SETTINGS_BODY_LIMIT).await?;
    let setting: SettingBody = parse_json(body)?;
    write_setting_body(state, user, setting).await
}

pub(super) async fn write_setting_body(
    state: &AppState,
    user: AuthedUser,
    setting: SettingBody,
) -> Result<JsonResp, ApiError> {
    let user_id = user.id;
    validate_setting_key(&setting.key)?;
    if setting.key == "language_settings" {
        let preferences: openwebide_core::plugins::languages::LanguageSettings =
            serde_json::from_str(&setting.value)
                .map_err(|_| ApiError::bad_request("Invalid language preferences"))?;
        preferences.validate().map_err(ApiError::bad_request)?;
    }
    if setting.key == "default_prompt" && !setting.value.is_empty() {
        let id = setting
            .value
            .parse::<i64>()
            .map_err(|_| ApiError::bad_request("Invalid system prompt"))?;
        state.store.get_system_prompt(id, user_id).await?;
    }
    state
        .store
        .set_user_setting(user_id, &setting.key, &setting.value)
        .await?;
    Ok(json_response(200, &json!({ "key": setting.key })))
}

pub(super) fn validate_setting_key(key: &str) -> Result<(), ApiError> {
    if key.starts_with("editor_recovery_") {
        return Err(ApiError::bad_request(
            "Use the versioned editor recovery API",
        ));
    }
    Ok(())
}

const SYSTEM_THEME: &str = "matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light'";

pub(crate) async fn theme_script(
    state: &AppState,
    user: Option<AuthedUser>,
) -> Result<JsonResp, ApiError> {
    let theme = match user {
        Some(user) => state.store.get_user_setting(user.id, "theme").await?,
        None => None,
    };
    let value = match theme.as_deref() {
        Some("dark") => "'dark'",
        Some("light") => "'light'",
        _ => SYSTEM_THEME,
    };
    Ok(Response::builder()
        .status(200)
        .header("content-type", "application/javascript")
        .header("cache-control", "no-store")
        .body(box_body(FullBody::new(Bytes::from(format!(
            "document.documentElement.setAttribute('data-theme',{value});\n"
        )))))
        .expect("valid status and headers"))
}
