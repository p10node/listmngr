//! Owner settings forms; no bearer credentials or stale list snapshots.
use super::{
    ApiResult, AppState, Form, HeaderMap, IntoResponse, Path, Redirect, Response, Shell, State,
    html, language, load, write_session,
};
use listmngr_core::{ListId, ModerationAction};
use listmngr_web::{Nav, NumberField, SelectField, choices};
use serde::Deserialize;

const ACTIONS: &[(&str, &str)] = &[
    ("default", "web-action-default"),
    ("defer", "web-action-defer"),
    ("accept", "web-policy-accept"),
    ("hold", "web-policy-hold"),
    ("reject", "web-policy-reject"),
    ("discard", "web-policy-discard"),
];
const YES_NO: &[(&str, &str)] = &[("true", "web-yes"), ("false", "web-no")];
const ARCHIVE: &[(&str, &str)] = &[
    ("public", "web-archive-public"),
    ("private", "web-archive-private"),
    ("never", "web-archive-never"),
];

pub(super) async fn form(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let language = language(&s, &headers);
    let list = s.db.browser_list_settings(&session, &id).await?;
    let selects = [
        (
            "emergency",
            "web-settings-emergency",
            YES_NO,
            list.emergency.to_string(),
        ),
        (
            "advertised",
            "web-settings-advertised",
            YES_NO,
            list.advertised.to_string(),
        ),
        (
            "send_welcome_message",
            "web-settings-welcome",
            YES_NO,
            list.send_welcome_message.to_string(),
        ),
        (
            "send_goodbye_message",
            "web-settings-goodbye",
            YES_NO,
            list.send_goodbye_message.to_string(),
        ),
        (
            "default_member_action",
            "web-settings-member-action",
            ACTIONS,
            list.default_member_action
                .map_or_else(|| "default".into(), |a| a.to_string()),
        ),
        (
            "default_nonmember_action",
            "web-settings-nonmember-action",
            ACTIONS,
            list.default_nonmember_action
                .map_or_else(|| "default".into(), |a| a.to_string()),
        ),
        (
            "archive_policy",
            "web-settings-archive-policy",
            ARCHIVE,
            list.archive_policy.to_string(),
        ),
    ]
    .into_iter()
    .map(|(name, label, values, selected)| SelectField {
        name: name.to_owned(),
        label: listmngr_i18n::message(language, label, &[]),
        choices: choices(language, values, Some(&selected)),
    })
    .collect();
    let numbers = [
        (
            "max_message_size",
            "web-settings-max-size",
            list.max_message_size,
        ),
        (
            "max_num_recipients",
            "web-settings-max-recipients",
            list.max_num_recipients,
        ),
    ]
    .into_iter()
    .map(|(name, label, value)| NumberField {
        name: name.to_owned(),
        label: listmngr_i18n::message(language, label, &[]),
        value,
        help: listmngr_i18n::message(language, &format!("{label}-help"), &[]),
    })
    .collect();
    Ok(html(&listmngr_web::Settings {
        shell: Shell::new(language, "web-title-settings", Nav::Account),
        intro: listmngr_i18n::message(language, "web-settings-intro", &[("list", id.as_str())]),
        action: format!("/web/lists/{}/settings", id.as_str()),
        csrf: session.csrf.clone(),
        display_name: list.display_name,
        description: list.description,
        subject_prefix: list.subject_prefix,
        selects,
        numbers,
        admin_href: "/web/admin".into(),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SettingsForm {
    csrf: String,
    display_name: String,
    description: String,
    advertised: String,
    default_member_action: String,
    default_nonmember_action: String,
    archive_policy: String,
    max_message_size: Option<u32>,
    max_num_recipients: Option<u32>,
    send_goodbye_message: Option<bool>,
    send_welcome_message: Option<bool>,
    emergency: Option<bool>,
    subject_prefix: Option<String>,
}

fn action(value: &str) -> listmngr_core::Result<Option<ModerationAction>> {
    if value == "default" {
        Ok(None)
    } else {
        value.parse().map(Some)
    }
}

pub(super) async fn save(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<SettingsForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    let advertised = match form.advertised.as_str() {
        "true" => true,
        "false" => false,
        _ => return Err(listmngr_core::Error::Validation("advertised".into()).into()),
    };
    let member = action(&form.default_member_action)?;
    let nonmember = action(&form.default_nonmember_action)?;
    let archive: listmngr_core::ArchivePolicy = form.archive_policy.parse()?;
    let mut patch = serde_json::json!({"display_name":form.display_name, "description":form.description, "advertised":advertised, "default_member_action":member, "default_nonmember_action":nonmember, "archive_policy":archive});
    // Legacy forms omit these fields; only supplied values enter the locked patch.
    for (name, value) in [
        ("max_message_size", form.max_message_size),
        ("max_num_recipients", form.max_num_recipients),
    ] {
        if let Some(value) = value {
            patch[name] = value.into();
        }
    }
    for (name, value) in [
        ("send_goodbye_message", form.send_goodbye_message),
        ("send_welcome_message", form.send_welcome_message),
        ("emergency", form.emergency),
    ] {
        if let Some(value) = value {
            patch[name] = value.into();
        }
    }
    if let Some(value) = form.subject_prefix {
        patch["subject_prefix"] = value.into();
    }
    s.db.browser_update_list_settings(&session, &id, &patch)
        .await?;
    Ok(Redirect::to(&format!("/web/lists/{id}/settings")).into_response())
}
