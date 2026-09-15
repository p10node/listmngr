//! The reader's own display name, interface language and time zone.
use super::{
    ApiResult, AppState, Form, HeaderMap, IntoResponse, Redirect, Response, Shell, State, html,
    load, reader_language, write_session,
};
use listmngr_db::Profile;
use listmngr_web::{Choice, Nav, choices};
use serde::Deserialize;

pub(super) async fn form(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let profile = s.db.browser_profile(&session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let locales: Vec<(&str, String)> = listmngr_i18n::SUPPORTED
        .iter()
        .map(|tag| (*tag, format!("web-language-{tag}")))
        .collect();
    let locales = choices(
        language,
        &locales
            .iter()
            .map(|(tag, id)| (*tag, id.as_str()))
            .collect::<Vec<_>>(),
        Some(&profile.locale),
    );
    let timezones = listmngr_db::web_profile::timezones()
        .iter()
        .map(|zone| Choice {
            value: (*zone).to_owned(),
            label: (*zone).to_owned(),
            selected: *zone == profile.timezone,
        })
        .collect();
    Ok(html(&listmngr_web::ProfilePage {
        shell: Shell::new(language, "web-title-profile", Nav::Account),
        csrf: session.csrf.clone(),
        display_name: profile.display_name,
        locales,
        timezones,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProfileForm {
    #[serde(default)]
    csrf: String,
    display_name: String,
    locale: String,
    timezone: String,
}

pub(super) async fn save(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ProfileForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    s.db.browser_update_profile(
        &session,
        &Profile {
            display_name: form.display_name,
            locale: form.locale,
            timezone: form.timezone,
        },
    )
    .await?;
    Ok(Redirect::to("/web/account/profile").into_response())
}
