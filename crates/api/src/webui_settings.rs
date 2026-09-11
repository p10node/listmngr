//! Owner settings forms; no bearer credentials or stale list snapshots.
use super::{
    ApiResult, AppState, Form, HeaderMap, IntoResponse, Path, Redirect, Response, State, escape,
    hidden, load, options, page, write_session,
};
use listmngr_core::{ListId, ModerationAction};
use serde::Deserialize;
use std::fmt::Write as _;

const ACTIONS: &[(&str, &str)] = &[
    ("default", "Use system fallback"),
    ("defer", "Defer (accept after safety checks)"),
    ("accept", "Accept"),
    ("hold", "Hold for review"),
    ("reject", "Reject"),
    ("discard", "Discard"),
];

pub(super) async fn form(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let list = s.db.browser_list_settings(&session, &id).await?;
    let mut body = format!(
        "<p>Settings for {}. Posting defaults apply to future decisions; safety checks and member overrides still apply. System fallback uses the server's configured posting action. Archive policy controls access and future archiving, not deletion of stored mail.</p><form method=\"post\" action=\"/web/lists/{}/settings\">{}",
        escape(id.as_str()),
        escape(id.as_str()),
        hidden(&session)
    );
    write!(&mut body, "<p><label for=\"display_name\">Display name</label><input id=\"display_name\" name=\"display_name\" value=\"{}\"></p><p><label for=\"description\">Description</label><textarea id=\"description\" name=\"description\">{}</textarea></p>", escape(&list.display_name), escape(&list.description)).expect("HTML");
    write!(&mut body, "<p><label for=\"subject_prefix\">Subject prefix</label><input type=\"text\" id=\"subject_prefix\" name=\"subject_prefix\" value=\"{}\" aria-describedby=\"subject-prefix-help\"><small id=\"subject-prefix-help\">Leave empty for no prefix. Spaces and Unicode are preserved; line breaks are not allowed. Applies when future outgoing posts are composed, not to mail already sent.</small></p>", escape(&list.subject_prefix)).expect("HTML");
    for (name, label, choices, selected) in [
        (
            "emergency",
            "Emergency moderation",
            &[("true", "Yes"), ("false", "No")][..],
            list.emergency.to_string(),
        ),
        (
            "advertised",
            "Show in public directory",
            &[("true", "Yes"), ("false", "No")][..],
            list.advertised.to_string(),
        ),
        (
            "send_welcome_message",
            "Send welcome messages",
            &[("true", "Yes"), ("false", "No")][..],
            list.send_welcome_message.to_string(),
        ),
        (
            "send_goodbye_message",
            "Send goodbye messages",
            &[("true", "Yes"), ("false", "No")][..],
            list.send_goodbye_message.to_string(),
        ),
        (
            "default_member_action",
            "Default member posting action",
            ACTIONS,
            list.default_member_action
                .map_or_else(|| "default".into(), |a| a.to_string()),
        ),
        (
            "default_nonmember_action",
            "Default nonmember posting action",
            ACTIONS,
            list.default_nonmember_action
                .map_or_else(|| "default".into(), |a| a.to_string()),
        ),
        (
            "archive_policy",
            "Archive policy",
            &[
                ("public", "Public"),
                ("private", "Private"),
                ("never", "Never"),
            ][..],
            list.archive_policy.to_string(),
        ),
    ] {
        write!(&mut body, "<p><label for=\"{name}\">{label}</label><select id=\"{name}\" name=\"{name}\">{}</select></p>", options(choices, Some(&selected))).expect("HTML");
    }
    body.push_str("<p>Emergency moderation holds otherwise eligible new posts for review, even when posting defaults accept them. It is not a delivery shutdown: already queued mail and explicit moderator approvals can still be delivered. Turning it off does not release held posts.</p>");
    for (name, label, value, help) in [
        (
            "max_message_size",
            "Maximum message size (KiB)",
            list.max_message_size,
            "Hold original posts larger than this size, including headers and attachments. 1 KiB is 1024 bytes; 0 disables this per-list limit, not the server intake limit.",
        ),
        (
            "max_num_recipients",
            "To/Cc recipient hold threshold",
            list.max_num_recipients,
            "Hold posts at or above this visible To/Cc mailbox count. Repeated addresses count; Bcc and list subscribers do not. Unparseable headers are held when enabled. 0 disables this check.",
        ),
    ] {
        write!(&mut body, "<p><label for=\"{name}\">{label}</label><input type=\"number\" id=\"{name}\" name=\"{name}\" value=\"{value}\" min=\"0\" max=\"2147483647\" step=\"1\" required aria-describedby=\"{name}-help\"><small id=\"{name}-help\">{help}</small></p>").expect("HTML");
    }
    body.push_str("<p>Welcome and goodbye messages apply to future completed subscriptions and removals. Changing these settings does not send notices to existing subscribers or recall queued notices.</p><button>Save list settings</button></form><p><a href=\"/web/admin\">List administration</a></p>");
    Ok(page("List settings", &body))
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
