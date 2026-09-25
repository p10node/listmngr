//! The list's settings pages: nine groups on the configuration patch engine
//! (with a preview that writes nothing and refusals shown inline), header
//! rules, bans, templates, digest and archiver actions, and deleting the
//! list. Every page needs the owner's live authority.
use super::{
    ApiResult, AppState, Form, HeaderMap, IntoResponse, Path, Redirect, Response, Shell, State,
    html, inline_refusal, load, privileged, reader_language, write_session,
};
use axum::extract::Query;
use listmngr_core::WebhookId;
use listmngr_core::{Error, ListId, MailingList};
use listmngr_db::WebhookScope;
use listmngr_db::header_matches::HeaderMatchRow;
use listmngr_db::{HeaderMatchChange, header_match_outcomes};
use listmngr_web::{
    ActionForm, Bans, DeleteList, DiffRow, Fact, GroupLink, HeaderRuleDraft, HeaderRuleRow,
    HeaderRules, Nav, SelectField, SettingField, SettingsGroup, TemplateCatalogue, TemplateEditor,
    TemplateRow, choices,
};
use serde::Deserialize;
use std::collections::HashMap;

use axum::routing::{get, post};

/// The pages under `/web/lists/{id}/settings` beyond the essentials form,
/// and the site bans under `/web/admin`.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/web/lists/{id}/settings/header-matches", get(rules))
        .route(
            "/web/lists/{id}/settings/header-matches/add",
            post(rule_add),
        )
        .route(
            "/web/lists/{id}/settings/header-matches/test",
            post(rule_test),
        )
        .route(
            "/web/lists/{id}/settings/header-matches/{position}",
            post(rule_update),
        )
        .route(
            "/web/lists/{id}/settings/header-matches/{position}/move",
            post(rule_move),
        )
        .route(
            "/web/lists/{id}/settings/header-matches/{position}/remove",
            post(rule_remove),
        )
        .route("/web/lists/{id}/settings/bans", get(list_bans))
        .route("/web/lists/{id}/settings/bans/add", post(list_ban_add))
        .route(
            "/web/lists/{id}/settings/bans/remove",
            post(list_ban_remove),
        )
        .route("/web/admin/bans", get(site_bans))
        .route("/web/admin/bans/add", post(site_ban_add))
        .route("/web/admin/bans/remove", post(site_ban_remove))
        .route("/web/lists/{id}/settings/webhooks", get(list_webhooks))
        .route(
            "/web/lists/{id}/settings/webhooks/add",
            post(list_webhook_add),
        )
        .route(
            "/web/lists/{id}/settings/webhooks/{webhook}",
            get(list_webhook),
        )
        .route(
            "/web/lists/{id}/settings/webhooks/{webhook}/enable",
            post(list_webhook_enable),
        )
        .route(
            "/web/lists/{id}/settings/webhooks/{webhook}/ping",
            post(list_webhook_ping),
        )
        .route(
            "/web/lists/{id}/settings/webhooks/{webhook}/rotate",
            post(list_webhook_rotate),
        )
        .route(
            "/web/lists/{id}/settings/webhooks/{webhook}/remove",
            post(list_webhook_remove),
        )
        .route("/web/admin/webhooks", get(site_webhooks))
        .route("/web/admin/webhooks/add", post(site_webhook_add))
        .route("/web/admin/webhooks/{webhook}", get(site_webhook))
        .route(
            "/web/admin/webhooks/{webhook}/enable",
            post(site_webhook_enable),
        )
        .route(
            "/web/admin/webhooks/{webhook}/ping",
            post(site_webhook_ping),
        )
        .route(
            "/web/admin/webhooks/{webhook}/rotate",
            post(site_webhook_rotate),
        )
        .route(
            "/web/admin/webhooks/{webhook}/remove",
            post(site_webhook_remove),
        )
        .route("/web/lists/{id}/settings/templates", get(templates))
        .route(
            "/web/lists/{id}/settings/templates/{name}",
            get(template_editor).post(template_save),
        )
        .route(
            "/web/lists/{id}/settings/templates/{name}/remove",
            post(template_remove),
        )
        .route("/web/lists/{id}/settings/digest/send", post(digest_send))
        .route("/web/lists/{id}/settings/digest/bump", post(digest_bump))
        .route(
            "/web/lists/{id}/settings/archiving/archiver",
            post(archiver),
        )
        .route(
            "/web/lists/{id}/settings/delete",
            get(delete_form).post(delete),
        )
        .route(
            "/web/lists/{id}/settings/{group}",
            get(group_form).post(group_save),
        )
}

type Options = &'static [(&'static str, &'static str)];

const YES_NO: Options = &[("true", "web-yes"), ("false", "web-no")];
const ACTIONS: Options = &[
    ("default", "web-action-default"),
    ("defer", "web-action-defer"),
    ("accept", "web-policy-accept"),
    ("hold", "web-policy-hold"),
    ("reject", "web-policy-reject"),
    ("discard", "web-policy-discard"),
];
const RESPONSE: Options = &[
    ("none", "web-ls-response-none"),
    ("respond_and_continue", "web-ls-response-respond"),
    ("respond_and_discard", "web-ls-response-discard"),
];
const FILTER_ACTION: Options = &[
    ("discard", "web-policy-discard"),
    ("reject", "web-policy-reject"),
    ("forward", "web-ls-filter-forward"),
    ("preserve", "web-ls-filter-preserve"),
];
const REPLY_TO: Options = &[
    ("no_munging", "web-ls-reply-no-munging"),
    ("point_to_list", "web-ls-reply-point-to-list"),
    ("explicit_header", "web-ls-reply-explicit"),
    ("explicit_header_only", "web-ls-reply-explicit-only"),
];
const PERSONALIZE: Options = &[
    ("none", "web-ls-personalize-none"),
    ("individual", "web-ls-personalize-individual"),
    ("full", "web-ls-personalize-full"),
];
const DMARC: Options = &[
    ("no_mitigation", "web-ls-dmarc-none"),
    ("munge_from", "web-ls-dmarc-munge"),
    ("wrap_message", "web-ls-dmarc-wrap"),
    ("reject", "web-policy-reject"),
    ("discard", "web-policy-discard"),
];
const FREQUENCY: Options = &[
    ("yearly", "web-ls-frequency-yearly"),
    ("quarterly", "web-ls-frequency-quarterly"),
    ("monthly", "web-ls-frequency-monthly"),
    ("weekly", "web-ls-frequency-weekly"),
    ("daily", "web-ls-frequency-daily"),
];
const ARCHIVE: Options = &[
    ("public", "web-archive-public"),
    ("private", "web-archive-private"),
    ("never", "web-archive-never"),
];
const RENDERING: Options = &[
    ("text", "web-ls-rendering-text"),
    ("markdown", "web-ls-rendering-markdown"),
];
const SUBSCRIPTION: Options = &[
    ("open", "web-ls-policy-open"),
    ("confirm", "web-ls-policy-confirm"),
    ("moderate", "web-ls-policy-moderate"),
    (
        "confirm_then_moderate",
        "web-ls-policy-confirm-then-moderate",
    ),
];
const ROSTER: Options = &[
    ("public", "web-ls-roster-public"),
    ("members", "web-ls-roster-members"),
    ("moderators", "web-ls-roster-moderators"),
];
const UNRECOGNIZED: Options = &[
    ("discard", "web-policy-discard"),
    ("site_owner", "web-ls-unrecognized-site-owner"),
    ("administrators", "web-ls-unrecognized-administrators"),
];
const LANGUAGES: Options = listmngr_i18n::NOTICE_LANGUAGE_OPTIONS;
const PIPELINES: Options = &[
    ("default-posting-pipeline", "web-ls-pipeline-default"),
    ("virgin", "web-ls-pipeline-virgin"),
];
/// Archivers the list can switch on. Each also needs the server to
/// configure it under `[archive] archivers`, and `mail-archive` takes a
/// public list only; the local archive follows `archive_policy`.
const ARCHIVERS: Options = &[
    ("mail-archive", "web-ls-archiver-mail-archive"),
    ("mhonarc", "web-ls-archiver-mhonarc"),
    ("prototype", "web-ls-archiver-prototype"),
    ("hyperkitty", "web-ls-archiver-hyperkitty"),
];

/// How a control is rendered and how its text becomes a patch value.
#[derive(Clone, Copy)]
enum Kind {
    Text,
    TextArea,
    /// One entry per line, submitted as a JSON array.
    Lines,
    Select(Options),
    /// A moderation action or `default` for none.
    Action,
    Number,
    Decimal,
}

struct Field {
    name: &'static str,
    kind: Kind,
    help: bool,
}

const fn f(name: &'static str, kind: Kind) -> Field {
    Field {
        name,
        kind,
        help: false,
    }
}
const fn h(name: &'static str, kind: Kind) -> Field {
    Field {
        name,
        kind,
        help: true,
    }
}

struct Group {
    slug: &'static str,
    fields: &'static [Field],
}

const GROUPS: &[Group] = &[
    Group {
        slug: "identity",
        fields: &[
            f("display_name", Kind::Text),
            f("description", Kind::TextArea),
            h("info", Kind::TextArea),
            h("subject_prefix", Kind::Text),
            f("advertised", Kind::Select(YES_NO)),
            f("preferred_language", Kind::Select(LANGUAGES)),
        ],
    },
    Group {
        slug: "responses",
        fields: &[
            f("autorespond_owner", Kind::Select(RESPONSE)),
            f("autoresponse_owner_text", Kind::TextArea),
            f("autorespond_postings", Kind::Select(RESPONSE)),
            f("autoresponse_postings_text", Kind::TextArea),
            f("autorespond_requests", Kind::Select(RESPONSE)),
            f("autoresponse_request_text", Kind::TextArea),
            h("autoresponse_grace_period", Kind::Number),
            f("respond_to_post_requests", Kind::Select(YES_NO)),
            h("send_welcome_message", Kind::Select(YES_NO)),
            f("send_goodbye_message", Kind::Select(YES_NO)),
            f("admin_immed_notify", Kind::Select(YES_NO)),
            f("admin_notify_mchanges", Kind::Select(YES_NO)),
        ],
    },
    Group {
        slug: "messages",
        fields: &[
            h("filter_content", Kind::Select(YES_NO)),
            h("filter_types", Kind::Lines),
            f("pass_types", Kind::Lines),
            h("filter_extensions", Kind::Lines),
            f("pass_extensions", Kind::Lines),
            f("collapse_alternatives", Kind::Select(YES_NO)),
            f("convert_html_to_plaintext", Kind::Select(YES_NO)),
            f("filter_action", Kind::Select(FILTER_ACTION)),
            h("anonymous_list", Kind::Select(YES_NO)),
            f("include_rfc2369_headers", Kind::Select(YES_NO)),
            f("allow_list_posts", Kind::Select(YES_NO)),
            f("reply_goes_to_list", Kind::Select(REPLY_TO)),
            f("reply_to_address", Kind::Text),
            f("first_strip_reply_to", Kind::Select(YES_NO)),
            h("personalize", Kind::Select(PERSONALIZE)),
            f("include_sender_header", Kind::Select(YES_NO)),
        ],
    },
    Group {
        slug: "dmarc",
        fields: &[
            h("dmarc_mitigate_action", Kind::Select(DMARC)),
            f("dmarc_mitigate_unconditionally", Kind::Select(YES_NO)),
            h("dmarc_addresses", Kind::Lines),
            f("dmarc_moderation_notice", Kind::TextArea),
            f("dmarc_wrapped_message_text", Kind::TextArea),
        ],
    },
    Group {
        slug: "digest",
        fields: &[
            f("digests_enabled", Kind::Select(YES_NO)),
            h("digest_size_threshold", Kind::Decimal),
            f("digest_send_periodic", Kind::Select(YES_NO)),
            f("digest_volume_frequency", Kind::Select(FREQUENCY)),
            f("next_digest_number", Kind::Number),
        ],
    },
    Group {
        slug: "acceptance",
        fields: &[
            f("default_member_action", Kind::Action),
            f("default_nonmember_action", Kind::Action),
            h("accept_these_nonmembers", Kind::Lines),
            f("hold_these_nonmembers", Kind::Lines),
            f("reject_these_nonmembers", Kind::Lines),
            f("discard_these_nonmembers", Kind::Lines),
            h("require_explicit_destination", Kind::Select(YES_NO)),
            f("acceptable_aliases", Kind::Lines),
            h("administrivia", Kind::Select(YES_NO)),
            h("max_message_size", Kind::Number),
            h("max_num_recipients", Kind::Number),
            h("emergency", Kind::Select(YES_NO)),
            h("posting_pipeline", Kind::Select(PIPELINES)),
        ],
    },
    Group {
        slug: "archiving",
        fields: &[
            h("archive_policy", Kind::Select(ARCHIVE)),
            f("archive_rendering_mode", Kind::Select(RENDERING)),
        ],
    },
    Group {
        slug: "members",
        fields: &[
            h("subscription_policy", Kind::Select(SUBSCRIPTION)),
            f("unsubscription_policy", Kind::Select(SUBSCRIPTION)),
            f("member_roster_visibility", Kind::Select(ROSTER)),
        ],
    },
    Group {
        slug: "bounces",
        fields: &[
            f("process_bounces", Kind::Select(YES_NO)),
            h("bounce_score_threshold", Kind::Decimal),
            h("bounce_info_stale_after", Kind::Number),
            h("bounce_you_are_disabled_warnings", Kind::Number),
            h("bounce_you_are_disabled_warnings_interval", Kind::Number),
            f("bounce_notify_owner_on_disable", Kind::Select(YES_NO)),
            f("bounce_notify_owner_on_removal", Kind::Select(YES_NO)),
            f(
                "bounce_notify_owner_on_bounce_increment",
                Kind::Select(YES_NO),
            ),
            f(
                "forward_unrecognized_bounces_to",
                Kind::Select(UNRECOGNIZED),
            ),
        ],
    },
];

/// The other pages under settings, after the nine groups.
const EXTRA_PAGES: &[&str] = &["header-matches", "bans", "templates", "webhooks", "delete"];

fn group(slug: &str) -> ApiResult<&'static Group> {
    GROUPS
        .iter()
        .find(|group| group.slug == slug)
        .ok_or_else(|| Error::NotFound("settings group".into()).into())
}

fn base(id: &ListId) -> String {
    format!("/web/lists/{}/settings", id.as_str())
}

fn label_id(name: &str) -> String {
    format!("web-ls-{}", name.replace('_', "-"))
}

pub(super) fn groups(language: &str, id: &ListId, current: &str) -> Vec<GroupLink> {
    let base = base(id);
    GROUPS
        .iter()
        .map(|group| group.slug)
        .chain(EXTRA_PAGES.iter().copied())
        .map(|slug| GroupLink {
            href: format!("{base}/{slug}"),
            label: listmngr_i18n::message(language, &format!("web-ls-group-{slug}"), &[]),
            current: slug == current,
        })
        .collect()
}

/// The text a control shows for a wire value.
fn display(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "default".into(),
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

const fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Text => "text",
        Kind::TextArea => "textarea",
        Kind::Lines => "lines",
        Kind::Select(_) | Kind::Action => "select",
        Kind::Number => "number",
        Kind::Decimal => "decimal",
    }
}

/// The wire value a submitted text stands for, or the refusal to show.
fn wire(field: &Field, text: &str) -> Result<serde_json::Value, &'static str> {
    match field.kind {
        Kind::Text => Ok(text.into()),
        Kind::TextArea => Ok(text.replace("\r\n", "\n").into()),
        Kind::Lines => Ok(text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(String::from)
            .collect::<Vec<_>>()
            .into()),
        Kind::Select(options) => match options.iter().find(|(value, _)| *value == text) {
            Some((value, _)) if *value == "true" || *value == "false" => {
                Ok((*value == "true").into())
            }
            Some((value, _)) => Ok((*value).into()),
            None => Err("web-ls-error-choice"),
        },
        Kind::Action => match ACTIONS.iter().find(|(value, _)| *value == text) {
            Some(("default", _)) => Ok(serde_json::Value::Null),
            Some((value, _)) => Ok((*value).into()),
            None => Err("web-ls-error-choice"),
        },
        Kind::Number => text
            .trim()
            .parse::<u64>()
            .map(serde_json::Value::from)
            .map_err(|_| "web-ls-error-number"),
        Kind::Decimal => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite())
            .map(serde_json::Value::from)
            .ok_or("web-ls-error-number"),
    }
}

fn select_choices(language: &str, kind: Kind, selected: &str) -> Vec<listmngr_web::Choice> {
    match kind {
        Kind::Select(options) => choices(language, options, Some(selected)),
        Kind::Action => choices(language, ACTIONS, Some(selected)),
        _ => Vec::new(),
    }
}

/// The controls of a group, from current values or a submitted form.
fn fields(
    language: &str,
    group: &Group,
    values: &dyn Fn(&str) -> String,
    errors: &HashMap<&str, String>,
) -> Vec<SettingField> {
    group
        .fields
        .iter()
        .map(|field| {
            let value = values(field.name);
            SettingField {
                name: field.name.into(),
                label: listmngr_i18n::message(language, &label_id(field.name), &[]),
                help: if field.help {
                    listmngr_i18n::message(language, &format!("{}-help", label_id(field.name)), &[])
                } else {
                    String::new()
                },
                kind: kind_name(field.kind).into(),
                choices: select_choices(language, field.kind, &value),
                value,
                error: errors.get(field.name).cloned(),
            }
        })
        .collect()
}

/// The read-only facts of the identity group.
fn facts(s: &AppState, language: &str, list: &MailingList) -> Vec<Fact> {
    let id = &list.id;
    let stamp = |at: Option<chrono::DateTime<chrono::Utc>>| {
        at.map_or_else(
            || listmngr_i18n::message(language, "web-ls-never", &[]),
            |at| at.format("%Y-%m-%d %H:%M UTC").to_string(),
        )
    };
    [
        ("list-id", id.to_string()),
        ("posting-address", id.posting_address()),
        ("owner-address", id.owner_address()),
        ("request-address", id.request_address()),
        ("bounces-address", id.bounces_address()),
        ("join-address", id.join_address()),
        ("leave-address", id.leave_address()),
        (
            "no-reply-address",
            format!("{}@{}", s.config.mailman.noreply_address, id.mail_host()),
        ),
        ("created-at", stamp(Some(list.created_at))),
        ("last-post-at", stamp(list.last_post_at)),
        ("post-id", list.post_id.to_string()),
        ("volume", list.volume.to_string()),
    ]
    .into_iter()
    .map(|(name, value)| Fact {
        label: listmngr_i18n::message(language, &format!("web-ls-fact-{name}"), &[]),
        value,
    })
    .collect()
}

/// The digest and archiver actions shown after their groups.
fn extras(
    language: &str,
    group: &Group,
    id: &ListId,
    archivers: &[(String, bool)],
) -> Vec<ActionForm> {
    let base = base(id);
    match group.slug {
        "digest" => vec![
            ActionForm {
                action: format!("{base}/digest/send"),
                button: listmngr_i18n::message(language, "web-ls-digest-send", &[]),
                help: listmngr_i18n::message(language, "web-ls-digest-send-help", &[]),
                selects: Vec::new(),
            },
            ActionForm {
                action: format!("{base}/digest/bump"),
                button: listmngr_i18n::message(language, "web-ls-digest-bump", &[]),
                help: listmngr_i18n::message(language, "web-ls-digest-bump-help", &[]),
                selects: Vec::new(),
            },
        ],
        "archiving" => {
            let state = ARCHIVERS
                .iter()
                .map(|(name, _)| {
                    let enabled = archivers.iter().any(|(stored, on)| stored == name && *on);
                    format!(
                        "{name}: {}",
                        listmngr_i18n::message(
                            language,
                            if enabled { "web-yes" } else { "web-no" },
                            &[]
                        )
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            vec![ActionForm {
                action: format!("{base}/archiving/archiver"),
                button: listmngr_i18n::message(language, "web-ls-archiver-save", &[]),
                help: listmngr_i18n::message(
                    language,
                    "web-ls-archiver-help",
                    &[("state", &state)],
                ),
                selects: vec![
                    SelectField {
                        name: "archiver".into(),
                        label: listmngr_i18n::message(language, "web-ls-archiver", &[]),
                        choices: choices(language, ARCHIVERS, None),
                    },
                    SelectField {
                        name: "enabled".into(),
                        label: listmngr_i18n::message(language, "web-ls-archiver-enabled", &[]),
                        choices: choices(language, YES_NO, Some("true")),
                    },
                ],
            }]
        }
        _ => Vec::new(),
    }
}

#[derive(Deserialize)]
pub(super) struct Notice {
    #[serde(default)]
    saved: String,
    #[serde(default)]
    language: String,
    /// A header rule to prefill (the held queue's shortcut).
    #[serde(default)]
    header: String,
    #[serde(default)]
    pattern: String,
}

fn notice(language: &str, q: &Notice) -> Option<String> {
    match q.saved.as_str() {
        "1" => Some(listmngr_i18n::message(language, "web-ls-saved", &[])),
        "digest" => Some(listmngr_i18n::message(language, "web-ls-digest-sent", &[])),
        "bump" => Some(listmngr_i18n::message(
            language,
            "web-ls-digest-bumped",
            &[],
        )),
        "archiver" => Some(listmngr_i18n::message(
            language,
            "web-ls-archiver-saved",
            &[],
        )),
        "removed" => Some(listmngr_i18n::message(language, "web-ls-removed", &[])),
        _ => None,
    }
}

/// The current wire values of the list, keyed as the patch engine expects.
fn current_values(list: &MailingList) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::to_value(list).expect("list serializes") {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_group(
    s: &AppState,
    language: &str,
    session_csrf: &str,
    list: &MailingList,
    group: &Group,
    values: &dyn Fn(&str) -> String,
    errors: &HashMap<&str, String>,
    error: Option<String>,
    notice: Option<String>,
    preview: Option<Vec<DiffRow>>,
    archivers: &[(String, bool)],
) -> Response {
    let id = &list.id;
    html(&SettingsGroup {
        shell: Shell::new(language, "web-title-settings", Nav::Account),
        title: listmngr_i18n::message(language, &format!("web-ls-group-{}", group.slug), &[]),
        intro: listmngr_i18n::message(
            language,
            &format!("web-ls-group-{}-intro", group.slug),
            &[("list", id.as_str())],
        ),
        groups: groups(language, id, group.slug),
        action: format!("{}/{}", base(id), group.slug),
        csrf: session_csrf.to_owned(),
        facts: if group.slug == "identity" {
            facts(s, language, list)
        } else {
            Vec::new()
        },
        fields: fields(language, group, values, errors),
        notice,
        error,
        preview,
        extras: extras(language, group, id, archivers),
        admin_href: "/web/admin".into(),
    })
}

/// `GET /web/lists/{id}/settings/{group}`.
pub(super) async fn group_form(
    State(s): State<AppState>,
    Path((id, slug)): Path<(ListId, String)>,
    Query(q): Query<Notice>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let group = group(&slug)?;
    let session = load(&s, &headers).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let list = s.db.browser_list_settings(&session, &id).await?;
    let archivers = if group.slug == "archiving" {
        s.db.browser_archivers(&session, &id).await?
    } else {
        Vec::new()
    };
    let current = current_values(&list);
    let values = |name: &str| current.get(name).map(display).unwrap_or_default();
    Ok(render_group(
        &s,
        language,
        &session.csrf,
        &list,
        group,
        &values,
        &HashMap::new(),
        None,
        notice(language, &q),
        None,
        &archivers,
    ))
}

/// Which field a repository refusal is about, when it names one.
fn refused_field(group: &'static Group, message: &str) -> Option<&'static str> {
    group.fields.iter().map(|field| field.name).find(|name| {
        message == *name
            || message.starts_with(&format!("{name}:"))
            || message.contains(&format!(" {name}"))
            || message.ends_with(name)
    })
}

/// `POST /web/lists/{id}/settings/{group}`: a preview or a save.
/// The rows a preview shows: every field whose value would change.
fn diff_rows(
    language: &str,
    group: &Group,
    list: &MailingList,
    patch: &serde_json::Value,
) -> Vec<DiffRow> {
    let current = current_values(list);
    group
        .fields
        .iter()
        .filter_map(|field| {
            let before = current.get(field.name)?;
            let after = patch.get(field.name)?;
            (!same(before, after)).then(|| DiffRow {
                label: listmngr_i18n::message(language, &label_id(field.name), &[]),
                name: field.name.into(),
                before: display(before),
                after: display(after),
            })
        })
        .collect()
}

/// A repository refusal as the form shows it: on its field when the
/// message names one, otherwise above the form.
fn refusal(
    language: &str,
    group: &'static Group,
    message: String,
) -> (HashMap<&'static str, String>, Option<String>) {
    refused_field(group, &message).map_or_else(
        || (HashMap::new(), Some(message)),
        |name| {
            (
                HashMap::from([(
                    name,
                    listmngr_i18n::message(language, "web-ls-error-refused", &[]),
                )]),
                None,
            )
        },
    )
}

/// `POST /web/lists/{id}/settings/{group}`: a preview or a save.
pub(super) async fn group_save(
    State(s): State<AppState>,
    Path((id, slug)): Path<(ListId, String)>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> ApiResult<Response> {
    let group = group(&slug)?;
    let csrf = form.get("csrf").cloned().unwrap_or_default();
    let session = write_session(&s, &headers, &csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let list = s.db.browser_list_settings(&session, &id).await?;
    let submitted = |name: &str| form.get(name).cloned().unwrap_or_default();
    let mut errors: HashMap<&str, String> = HashMap::new();
    let mut patch = serde_json::Map::new();
    for field in group.fields {
        match wire(field, &submitted(field.name)) {
            Ok(value) => {
                patch.insert(field.name.into(), value);
            }
            Err(message) => {
                errors.insert(field.name, listmngr_i18n::message(language, message, &[]));
            }
        }
    }
    let archivers = if group.slug == "archiving" {
        s.db.browser_archivers(&session, &id).await?
    } else {
        Vec::new()
    };
    let render =
        |errors: &HashMap<&str, String>, error: Option<String>, preview: Option<Vec<DiffRow>>| {
            render_group(
                &s,
                language,
                &session.csrf,
                &list,
                group,
                &submitted,
                errors,
                error,
                None,
                preview,
                &archivers,
            )
        };
    if !errors.is_empty() {
        return Ok(inline_refusal(render(&errors, None, None)));
    }
    let patch = serde_json::Value::Object(patch);
    let preview = form.get("preview").is_some_and(|value| value == "1");
    // The preview runs the same validator the save runs, on a copy that is
    // thrown away; the save runs it on the locked row.
    let outcome = if preview {
        listmngr_db::ListRepo::validate_patch(&list, &patch).map(|_| ())
    } else {
        s.db.browser_update_list_settings(&session, &id, &patch)
            .await
    };
    match outcome {
        Ok(()) if preview => Ok(render(
            &HashMap::new(),
            None,
            Some(diff_rows(language, group, &list, &patch)),
        )),
        Ok(()) => {
            Ok(Redirect::to(&format!("{}/{}?saved=1", base(&id), group.slug)).into_response())
        }
        Err(Error::Validation(message)) => {
            let (errors, error) = refusal(language, group, message);
            Ok(inline_refusal(render(&errors, error, None)))
        }
        Err(other) => Err(other.into()),
    }
}

/// JSON equality that treats `1` and `1.0` alike.
fn same(before: &serde_json::Value, after: &serde_json::Value) -> bool {
    match (before.as_f64(), after.as_f64()) {
        (Some(a), Some(b)) => (a - b).abs() < f64::EPSILON,
        _ => before == after,
    }
}

// ----- digest and archiver actions -----------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Csrf {
    #[serde(default)]
    csrf: String,
}

pub(super) async fn digest_send(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    s.db.browser_digest_send(&session, &id, super::now())
        .await?;
    Ok(Redirect::to(&format!("{}/digest?saved=digest", base(&id))).into_response())
}

pub(super) async fn digest_bump(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    s.db.browser_digest_bump(&session, &id).await?;
    Ok(Redirect::to(&format!("{}/digest?saved=bump", base(&id))).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArchiverForm {
    #[serde(default)]
    csrf: String,
    archiver: String,
    enabled: String,
}

pub(super) async fn archiver(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<ArchiverForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    if !ARCHIVERS.iter().any(|(name, _)| *name == form.archiver) {
        return Err(Error::Validation("archiver".into()).into());
    }
    let enabled = match form.enabled.as_str() {
        "true" => true,
        "false" => false,
        _ => return Err(Error::Validation("enabled".into()).into()),
    };
    s.db.browser_archiver_set(&session, &id, &form.archiver, enabled)
        .await?;
    Ok(Redirect::to(&format!("{}/archiving?saved=archiver", base(&id))).into_response())
}

// ----- header rules ---------------------------------------------------------

const RULE_ACTIONS: Options = &[
    ("", "web-rules-action-none"),
    ("accept", "web-policy-accept"),
    ("hold", "web-policy-hold"),
    ("reject", "web-policy-reject"),
    ("discard", "web-policy-discard"),
];

fn rule_rows(
    language: &str,
    rows: &[HeaderMatchRow],
    matched: Option<&[bool]>,
) -> Vec<HeaderRuleRow> {
    rows.iter()
        .enumerate()
        .map(|(position, row)| HeaderRuleRow {
            position,
            header: row.header.clone(),
            pattern: row.pattern.clone(),
            actions: choices(
                language,
                RULE_ACTIONS,
                Some(row.chain.as_deref().unwrap_or_default()),
            ),
            tag: row.tag.clone().unwrap_or_default(),
            matched: matched.and_then(|outcomes| outcomes.get(position).copied()),
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_rules(
    language: &str,
    csrf: &str,
    id: &ListId,
    rows: &[HeaderMatchRow],
    draft: HeaderRuleDraft,
    header_error: Option<String>,
    pattern_error: Option<String>,
    test: Option<&(String, String)>,
    notice: Option<String>,
    error: Option<String>,
) -> Response {
    let outcomes = test.map(|(header, value)| header_match_outcomes(rows, header, value));
    let verdict = test.map(|(header, value)| {
        let first = outcomes
            .as_ref()
            .and_then(|outcomes| outcomes.iter().position(|hit| *hit));
        let verdict = first.map_or_else(
            || listmngr_i18n::message(language, "web-rules-verdict-none", &[]),
            |position| {
                let chain = rows[position].chain.as_deref().unwrap_or("hold");
                listmngr_i18n::message(
                    language,
                    "web-rules-verdict",
                    &[("position", &(position + 1).to_string()), ("action", chain)],
                )
            },
        );
        (header.clone(), value.clone(), verdict)
    });
    html(&HeaderRules {
        shell: Shell::new(language, "web-title-settings", Nav::Account),
        groups: groups(language, id, "header-matches"),
        base: format!("{}/header-matches", base(id)),
        csrf: csrf.to_owned(),
        rules: rule_rows(language, rows, outcomes.as_deref()),
        draft,
        pattern_error,
        header_error,
        actions: choices(language, RULE_ACTIONS, Some("")),
        test: verdict,
        notice,
        error,
    })
}

pub(super) async fn rules(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(q): Query<Notice>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let rows = s.db.browser_header_matches(&session, &id).await?;
    let draft = HeaderRuleDraft {
        header: q.header.chars().take(1024).collect(),
        pattern: q.pattern.chars().take(1024).collect(),
        tag: String::new(),
    };
    Ok(render_rules(
        language,
        &session.csrf,
        &id,
        &rows,
        draft,
        None,
        None,
        None,
        notice(language, &q),
        None,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuleForm {
    #[serde(default)]
    csrf: String,
    header: String,
    pattern: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    tag: String,
}

impl RuleForm {
    fn row(&self) -> ApiResult<HeaderMatchRow> {
        if !self.action.is_empty()
            && !listmngr_db::header_matches::TARGET_CHAINS.contains(&self.action.as_str())
        {
            return Err(Error::Validation(
                "header match chain must be one of accept, hold, reject, discard".into(),
            )
            .into());
        }
        Ok(HeaderMatchRow {
            header: self.header.trim().to_owned(),
            pattern: self.pattern.clone(),
            chain: (!self.action.is_empty()).then(|| self.action.clone()),
            tag: {
                let tag = self.tag.trim();
                (!tag.is_empty()).then(|| tag.to_owned())
            },
        })
    }
}

/// A refused rule: the message names the header, the pattern, or neither.
async fn refuse_rule(
    s: &AppState,
    language: &str,
    session: &listmngr_db::web_sessions::WebSession,
    id: &ListId,
    form: &RuleForm,
    message: &str,
) -> ApiResult<Response> {
    let rows = s.db.browser_header_matches(session, id).await?;
    let (header_error, pattern_error, error) = if message.contains("pattern") {
        (
            None,
            Some(listmngr_i18n::message(
                language,
                "web-rules-error-pattern",
                &[],
            )),
            None,
        )
    } else if message.contains("header must") {
        (
            Some(listmngr_i18n::message(
                language,
                "web-rules-error-header",
                &[],
            )),
            None,
            None,
        )
    } else {
        (None, None, Some(message.to_owned()))
    };
    Ok(inline_refusal(render_rules(
        language,
        &session.csrf,
        id,
        &rows,
        HeaderRuleDraft {
            header: form.header.clone(),
            pattern: form.pattern.clone(),
            tag: form.tag.clone(),
        },
        header_error,
        pattern_error,
        None,
        None,
        error,
    )))
}

async fn apply_rule(
    s: &AppState,
    headers: &HeaderMap,
    id: &ListId,
    form: &RuleForm,
    change: impl FnOnce(HeaderMatchRow) -> HeaderMatchChange,
) -> ApiResult<Response> {
    let session = write_session(s, headers, &form.csrf).await?;
    privileged(s, &session).await?;
    let language = reader_language(s, headers, &session).await?;
    let row = match form.row() {
        Ok(row) => row,
        Err(super::ApiError(Error::Validation(message))) => {
            return refuse_rule(s, language, &session, id, form, &message).await;
        }
        Err(other) => return Err(other),
    };
    match s
        .db
        .browser_header_match_change(&session, id, change(row))
        .await
    {
        Ok(()) => Ok(Redirect::to(&format!("{}/header-matches?saved=1", base(id))).into_response()),
        Err(Error::Validation(message)) => {
            refuse_rule(s, language, &session, id, form, &message).await
        }
        Err(other) => Err(other.into()),
    }
}

pub(super) async fn rule_add(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<RuleForm>,
) -> ApiResult<Response> {
    apply_rule(&s, &headers, &id, &form, HeaderMatchChange::Append).await
}

pub(super) async fn rule_update(
    State(s): State<AppState>,
    Path((id, position)): Path<(ListId, usize)>,
    headers: HeaderMap,
    Form(form): Form<RuleForm>,
) -> ApiResult<Response> {
    apply_rule(&s, &headers, &id, &form, |row| {
        HeaderMatchChange::Update(position, row)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MoveForm {
    #[serde(default)]
    csrf: String,
    direction: String,
}

pub(super) async fn rule_move(
    State(s): State<AppState>,
    Path((id, position)): Path<(ListId, usize)>,
    headers: HeaderMap,
    Form(form): Form<MoveForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    let target = match form.direction.as_str() {
        "up" => position.saturating_sub(1),
        "down" => position.saturating_add(1),
        _ => return Err(Error::Validation("direction".into()).into()),
    };
    if target != position {
        s.db.browser_header_match_change(&session, &id, HeaderMatchChange::Move(position, target))
            .await?;
    }
    Ok(Redirect::to(&format!("{}/header-matches?saved=1", base(&id))).into_response())
}

pub(super) async fn rule_remove(
    State(s): State<AppState>,
    Path((id, position)): Path<(ListId, usize)>,
    headers: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    s.db.browser_header_match_change(&session, &id, HeaderMatchChange::Remove(position))
        .await?;
    Ok(Redirect::to(&format!("{}/header-matches?saved=removed", base(&id))).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TestForm {
    #[serde(default)]
    csrf: String,
    header: String,
    value: String,
}

/// Which stored rule a header value would trip; nothing is written.
pub(super) async fn rule_test(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<TestForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    if form.header.len() > 1024 || form.value.len() > 4096 {
        return Err(Error::Validation("header value".into()).into());
    }
    let rows = s.db.browser_header_matches(&session, &id).await?;
    Ok(render_rules(
        language,
        &session.csrf,
        &id,
        &rows,
        HeaderRuleDraft::default(),
        None,
        None,
        Some(&(form.header, form.value)),
        None,
        None,
    ))
}

// ----- bans -----------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct BanPage {
    #[serde(default)]
    page: u32,
    #[serde(default)]
    saved: String,
}

#[allow(clippy::too_many_arguments)]
fn render_bans(
    language: &str,
    csrf: &str,
    list: Option<&ListId>,
    page_base: &str,
    page: u32,
    bans: Vec<String>,
    total: i64,
    draft: String,
    error: Option<String>,
    notice: Option<String>,
) -> Response {
    let more = bans.len() > 20;
    html(&Bans {
        shell: Shell::new(language, "web-title-settings", Nav::Account),
        groups: list
            .map(|id| groups(language, id, "bans"))
            .unwrap_or_default(),
        intro: listmngr_i18n::message(
            language,
            if list.is_some() {
                "web-bans-intro-list"
            } else {
                "web-bans-intro-site"
            },
            &[],
        ),
        base: page_base.to_owned(),
        csrf: csrf.to_owned(),
        bans: bans.into_iter().take(20).collect(),
        total,
        draft,
        error,
        notice,
        pagination: listmngr_web::Pagination::numbered(page_base, page, more, 10_000),
    })
}

pub(super) async fn list_bans(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(q): Query<BanPage>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    if q.page > 10_000 {
        return Err(Error::Validation("page out of range".into()).into());
    }
    let (bans, total) =
        s.db.browser_list_bans(&session, &id, i64::from(q.page) * 20)
            .await?;
    let notice = (q.saved == "1").then(|| listmngr_i18n::message(language, "web-ls-saved", &[]));
    Ok(render_bans(
        language,
        &session.csrf,
        Some(&id),
        &format!("{}/bans", base(&id)),
        q.page,
        bans,
        total,
        String::new(),
        None,
        notice,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BanForm {
    #[serde(default)]
    csrf: String,
    email_or_regex: String,
}

pub(super) async fn list_ban_add(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<BanForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    match s
        .db
        .browser_list_ban_add(&session, &id, &form.email_or_regex)
        .await
    {
        Ok(_) => Ok(Redirect::to(&format!("{}/bans?saved=1", base(&id))).into_response()),
        Err(Error::Validation(_) | Error::Conflict(_)) => {
            let (bans, total) = s.db.browser_list_bans(&session, &id, 0).await?;
            Ok(inline_refusal(render_bans(
                language,
                &session.csrf,
                Some(&id),
                &format!("{}/bans", base(&id)),
                0,
                bans,
                total,
                form.email_or_regex,
                Some(listmngr_i18n::message(language, "web-bans-error", &[])),
                None,
            )))
        }
        Err(other) => Err(other.into()),
    }
}

pub(super) async fn list_ban_remove(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<BanForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    s.db.browser_list_ban_remove(&session, &id, &form.email_or_regex)
        .await?;
    Ok(Redirect::to(&format!("{}/bans?saved=1", base(&id))).into_response())
}

pub(super) async fn site_bans(
    State(s): State<AppState>,
    Query(q): Query<BanPage>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    if q.page > 10_000 {
        return Err(Error::Validation("page out of range".into()).into());
    }
    let (bans, total) =
        s.db.browser_site_bans(&session, i64::from(q.page) * 20)
            .await?;
    let notice = (q.saved == "1").then(|| listmngr_i18n::message(language, "web-ls-saved", &[]));
    Ok(render_bans(
        language,
        &session.csrf,
        None,
        "/web/admin/bans",
        q.page,
        bans,
        total,
        String::new(),
        None,
        notice,
    ))
}

pub(super) async fn site_ban_add(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<BanForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    match s
        .db
        .browser_site_ban_add(&session, &form.email_or_regex)
        .await
    {
        Ok(_) => Ok(Redirect::to("/web/admin/bans?saved=1").into_response()),
        Err(Error::Validation(_) | Error::Conflict(_)) => {
            let (bans, total) = s.db.browser_site_bans(&session, 0).await?;
            Ok(inline_refusal(render_bans(
                language,
                &session.csrf,
                None,
                "/web/admin/bans",
                0,
                bans,
                total,
                form.email_or_regex,
                Some(listmngr_i18n::message(language, "web-bans-error", &[])),
                None,
            )))
        }
        Err(other) => Err(other.into()),
    }
}

pub(super) async fn site_ban_remove(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<BanForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    s.db.browser_site_ban_remove(&session, &form.email_or_regex)
        .await?;
    Ok(Redirect::to("/web/admin/bans?saved=1").into_response())
}

// ----- templates ------------------------------------------------------------

pub(super) async fn templates(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(q): Query<Notice>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let stored = s.db.browser_templates(&session, &id).await?;
    let rows = listmngr_mail::templates::NAMES
        .iter()
        .filter(|name| name.starts_with("list:"))
        .map(|name| TemplateRow {
            name: (*name).to_owned(),
            href: format!("{}/templates/{name}", base(&id)),
            languages: stored
                .iter()
                .filter(|row| row.name == *name && row.body.is_some())
                .map(|row| row.language.clone())
                .collect(),
        })
        .collect();
    Ok(html(&TemplateCatalogue {
        shell: Shell::new(language, "web-title-settings", Nav::Account),
        groups: groups(language, &id, "templates"),
        rows,
        notice: notice(language, &q),
    }))
}

/// The placeholders a template can use, with the values a preview shows.
fn placeholders(list: &MailingList) -> listmngr_mail::templates::Placeholders {
    listmngr_mail::templates::list_placeholders(list)
        .set("user_email", "member@example.org")
        .set("user_name", "A. Member")
        .set("user_address", "member@example.org")
        .set("user_delivered_to", "member@example.org")
        .set("user_language", "en")
        .set("subject", "An example subject")
        .set("sender_email", "sender@example.org")
        .set("reasons", "- an example reason")
        .set("token", "0123456789abcdef")
        .set("site_name", "Example Lists")
}

#[allow(clippy::too_many_arguments)]
fn render_editor(
    language: &str,
    csrf: &str,
    id: &ListId,
    name: &str,
    list: &MailingList,
    view: &listmngr_db::TemplateView,
    template_language: &str,
    body: String,
    preview: Option<String>,
    error: Option<String>,
) -> Response {
    let values = placeholders(list);
    let placeholders = listmngr_mail::templates::PLACEHOLDER_NAMES
        .iter()
        .map(|name| Fact {
            label: (*name).to_owned(),
            value: values.get(name).unwrap_or_default().to_owned(),
        })
        .collect();
    html(&TemplateEditor {
        shell: Shell::new(language, "web-title-settings", Nav::Account),
        groups: groups(language, id, "templates"),
        name: name.to_owned(),
        action: format!("{}/templates/{name}", base(id)),
        catalogue_href: format!("{}/templates", base(id)),
        csrf: csrf.to_owned(),
        languages: choices(language, LANGUAGES, Some(template_language)),
        body,
        stored: view.stored.is_some(),
        source: view.source.clone(),
        effective: view.effective.clone(),
        preview,
        placeholders,
        error,
    })
}

pub(super) async fn template_editor(
    State(s): State<AppState>,
    Path((id, name)): Path<(ListId, String)>,
    Query(q): Query<Notice>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let template_language = if q.language.is_empty() {
        "en"
    } else {
        q.language.as_str()
    };
    if !LANGUAGES.iter().any(|(code, _)| *code == template_language) {
        return Err(Error::Validation("language".into()).into());
    }
    let (list, view) =
        s.db.browser_template(&session, &id, &name, template_language)
            .await?;
    let body = view
        .stored
        .clone()
        .unwrap_or_else(|| view.effective.clone());
    Ok(render_editor(
        language,
        &session.csrf,
        &id,
        &name,
        &list,
        &view,
        template_language,
        body,
        None,
        None,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TemplateForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    preview: String,
    language: String,
    body: String,
}

pub(super) async fn template_save(
    State(s): State<AppState>,
    Path((id, name)): Path<(ListId, String)>,
    headers: HeaderMap,
    Form(form): Form<TemplateForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    if !LANGUAGES.iter().any(|(code, _)| *code == form.language) {
        return Err(Error::Validation("language".into()).into());
    }
    let (list, view) =
        s.db.browser_template(&session, &id, &name, &form.language)
            .await?;
    if form.preview == "1" {
        let rendered = listmngr_mail::templates::expand(&form.body, &placeholders(&list));
        return Ok(render_editor(
            language,
            &session.csrf,
            &id,
            &name,
            &list,
            &view,
            &form.language,
            form.body,
            Some(rendered),
            None,
        ));
    }
    match s
        .db
        .browser_template_set(&session, &id, &name, &form.language, &form.body)
        .await
    {
        Ok(()) => Ok(Redirect::to(&format!(
            "{}/templates/{name}?language={}",
            base(&id),
            form.language
        ))
        .into_response()),
        Err(Error::Validation(message)) => Ok(inline_refusal(render_editor(
            language,
            &session.csrf,
            &id,
            &name,
            &list,
            &view,
            &form.language,
            form.body,
            None,
            Some(message),
        ))),
        Err(other) => Err(other.into()),
    }
}

pub(super) async fn template_remove(
    State(s): State<AppState>,
    Path((id, name)): Path<(ListId, String)>,
    headers: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    s.db.browser_template_delete(&session, &id, &name).await?;
    Ok(Redirect::to(&format!("{}/templates?saved=removed", base(&id))).into_response())
}

// ----- deleting the list ----------------------------------------------------

fn render_delete(
    language: &str,
    csrf: &str,
    list: &MailingList,
    error: Option<String>,
) -> Response {
    let id = &list.id;
    html(&DeleteList {
        shell: Shell::new(language, "web-title-settings", Nav::Account),
        groups: groups(language, id, "delete"),
        list_id: id.to_string(),
        consequences: vec![
            listmngr_i18n::message(language, "web-delete-list-members", &[]),
            listmngr_i18n::message(
                language,
                "web-delete-list-archive",
                &[("policy", list.archive_policy.as_str())],
            ),
            listmngr_i18n::message(language, "web-delete-list-settings", &[]),
        ],
        action: format!("{}/delete", base(id)),
        csrf: csrf.to_owned(),
        error,
    })
}

pub(super) async fn delete_form(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let list = s.db.browser_list_settings(&session, &id).await?;
    Ok(render_delete(language, &session.csrf, &list, None))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    confirm: String,
}

pub(super) async fn delete(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<DeleteForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let list = s.db.browser_list_settings(&session, &id).await?;
    if form.confirm.trim() != id.as_str() {
        return Ok(inline_refusal(render_delete(
            language,
            &session.csrf,
            &list,
            Some(listmngr_i18n::message(
                language,
                "web-delete-list-mismatch",
                &[],
            )),
        )));
    }
    s.db.browser_delete_list(&session, &id).await?;
    Ok(Redirect::to("/web/admin").into_response())
}

// ---- Webhooks: a list's for its owner, every one for a server owner ----

/// The webhook pages of a list, or of the site.
struct WebhookPages<'a> {
    scope: WebhookScope<'a>,
    /// Base path: the collection page, `<base>/add`, `<base>/<id>/…`.
    base: String,
}

impl WebhookPages<'_> {
    fn list(id: &ListId) -> WebhookPages<'_> {
        WebhookPages {
            scope: WebhookScope::List(id),
            base: format!("{}/webhooks", base(id)),
        }
    }

    const fn site() -> WebhookPages<'static> {
        WebhookPages {
            scope: WebhookScope::Site,
            base: String::new(),
        }
    }

    fn path(&self) -> &str {
        if self.base.is_empty() {
            "/web/admin/webhooks"
        } else {
            &self.base
        }
    }

    fn row(&self, webhook: &listmngr_db::Webhook) -> listmngr_web::WebhookRow {
        let actions = format!("{}/{}", self.path(), webhook.id);
        listmngr_web::WebhookRow {
            href: actions.clone(),
            actions,
            url: webhook.url.clone(),
            events: webhook.events.join(", "),
            list_id: webhook.list_id.as_ref().map(ToString::to_string),
            enabled: webhook.enabled,
            fingerprint: webhook.secret_fingerprint.clone(),
            description: webhook.description.clone(),
        }
    }

    fn groups(&self, language: &str) -> Vec<listmngr_web::GroupLink> {
        match self.scope {
            WebhookScope::List(id) => groups(language, id, "webhooks"),
            WebhookScope::Site => Vec::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WebhookForm {
    #[serde(default)]
    csrf: String,
    url: String,
    events: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EnableForm {
    #[serde(default)]
    csrf: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CsrfForm {
    #[serde(default)]
    csrf: String,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct WebhookNotice {
    #[serde(default)]
    saved: String,
    #[serde(default)]
    pinged: String,
}

fn stamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn render_webhooks(
    language: &str,
    csrf: &str,
    pages: &WebhookPages<'_>,
    webhooks: &[listmngr_db::Webhook],
    draft: (String, String, String),
    error: Option<String>,
    notice: Option<String>,
    secret: Option<(WebhookId, String)>,
) -> Response {
    html(&listmngr_web::Webhooks {
        shell: Shell::new(
            language,
            if pages.base.is_empty() {
                "web-title-webhooks"
            } else {
                "web-title-settings"
            },
            Nav::Account,
        ),
        groups: pages.groups(language),
        intro: listmngr_i18n::message(
            language,
            match pages.scope {
                WebhookScope::List(_) => "web-webhooks-intro-list",
                WebhookScope::Site => "web-webhooks-intro-site",
            },
            &[],
        ),
        base: pages.path().to_owned(),
        csrf: csrf.to_owned(),
        rows: webhooks.iter().map(|webhook| pages.row(webhook)).collect(),
        draft_url: draft.0,
        draft_events: draft.1,
        draft_description: draft.2,
        error,
        notice,
        secret: secret.map(|(_, secret)| listmngr_web::ShownSecret { secret }),
    })
}

fn split_events(events: &str) -> Vec<String> {
    events
        .split(',')
        .map(str::trim)
        .filter(|event| !event.is_empty())
        .map(str::to_owned)
        .collect()
}

async fn webhooks_page(
    s: &AppState,
    headers: &HeaderMap,
    pages: &WebhookPages<'_>,
    q: &WebhookNotice,
) -> ApiResult<Response> {
    let session = load(s, headers).await?;
    privileged(s, &session).await?;
    let language = reader_language(s, headers, &session).await?;
    let webhooks = s.db.browser_webhooks(&session, pages.scope).await?;
    let notice = if q.saved == "1" {
        Some(listmngr_i18n::message(language, "web-ls-saved", &[]))
    } else if q.pinged == "1" {
        Some(listmngr_i18n::message(language, "web-webhooks-pinged", &[]))
    } else {
        None
    };
    Ok(render_webhooks(
        language,
        &session.csrf,
        pages,
        &webhooks,
        (String::new(), "*".into(), String::new()),
        None,
        notice,
        None,
    ))
}

async fn webhook_add(
    s: &AppState,
    headers: &HeaderMap,
    pages: &WebhookPages<'_>,
    form: WebhookForm,
) -> ApiResult<Response> {
    let session = write_session(s, headers, &form.csrf).await?;
    privileged(s, &session).await?;
    let language = reader_language(s, headers, &session).await?;
    match s
        .db
        .browser_webhook_create(
            &session,
            pages.scope,
            &form.url,
            split_events(&form.events),
            &form.description,
        )
        .await
    {
        Ok((webhook, secret)) => {
            // The secret is shown on this response and never again, so
            // the page is rendered here rather than redirected to.
            let webhooks = s.db.browser_webhooks(&session, pages.scope).await?;
            Ok(render_webhooks(
                language,
                &session.csrf,
                pages,
                &webhooks,
                (String::new(), "*".into(), String::new()),
                None,
                Some(listmngr_i18n::message(language, "web-ls-saved", &[])),
                Some((webhook.id, secret)),
            ))
        }
        Err(Error::Validation(_) | Error::Conflict(_)) => {
            let webhooks = s.db.browser_webhooks(&session, pages.scope).await?;
            Ok(inline_refusal(render_webhooks(
                language,
                &session.csrf,
                pages,
                &webhooks,
                (form.url, form.events, form.description),
                Some(listmngr_i18n::message(language, "web-webhooks-error", &[])),
                None,
                None,
            )))
        }
        Err(other) => Err(other.into()),
    }
}

async fn webhook_page(
    s: &AppState,
    headers: &HeaderMap,
    pages: &WebhookPages<'_>,
    id: WebhookId,
    q: &WebhookNotice,
) -> ApiResult<Response> {
    let session = load(s, headers).await?;
    privileged(s, &session).await?;
    let language = reader_language(s, headers, &session).await?;
    let (webhook, deliveries) =
        s.db.browser_webhook_deliveries(&session, pages.scope, id)
            .await?;
    let notice =
        (q.pinged == "1").then(|| listmngr_i18n::message(language, "web-webhooks-pinged", &[]));
    Ok(html(&listmngr_web::WebhookDeliveries {
        shell: Shell::new(
            language,
            if pages.base.is_empty() {
                "web-title-webhooks"
            } else {
                "web-title-settings"
            },
            Nav::Account,
        ),
        groups: pages.groups(language),
        back: pages.path().to_owned(),
        csrf: session.csrf.clone(),
        row: pages.row(&webhook),
        deliveries: deliveries
            .iter()
            .map(|delivery| listmngr_web::DeliveryRow {
                event: delivery.event.clone(),
                state: serde_json::to_value(delivery.state)
                    .ok()
                    .and_then(|state| state.as_str().map(ToOwned::to_owned))
                    .unwrap_or_default(),
                attempts: delivery.attempts,
                status: delivery
                    .last_status
                    .map(|status| status.to_string())
                    .unwrap_or_default(),
                error: delivery.last_error.clone().unwrap_or_default(),
                created: stamp(delivery.created_at),
                next: match delivery.state {
                    listmngr_db::DeliveryState::Pending => stamp(delivery.next_attempt_at),
                    _ => String::new(),
                },
            })
            .collect(),
        notice,
    }))
}

async fn webhook_enable(
    s: &AppState,
    headers: &HeaderMap,
    pages: &WebhookPages<'_>,
    id: WebhookId,
    form: EnableForm,
) -> ApiResult<Response> {
    let session = write_session(s, headers, &form.csrf).await?;
    privileged(s, &session).await?;
    s.db.browser_webhook_set_enabled(&session, pages.scope, id, form.enabled)
        .await?;
    Ok(Redirect::to(&format!("{}?saved=1", pages.path())).into_response())
}

async fn webhook_ping(
    s: &AppState,
    headers: &HeaderMap,
    pages: &WebhookPages<'_>,
    id: WebhookId,
    form: CsrfForm,
) -> ApiResult<Response> {
    let session = write_session(s, headers, &form.csrf).await?;
    privileged(s, &session).await?;
    s.db.browser_webhook_ping(&session, pages.scope, id).await?;
    Ok(Redirect::to(&format!("{}/{id}?pinged=1", pages.path())).into_response())
}

async fn webhook_rotate(
    s: &AppState,
    headers: &HeaderMap,
    pages: &WebhookPages<'_>,
    id: WebhookId,
    form: CsrfForm,
) -> ApiResult<Response> {
    let session = write_session(s, headers, &form.csrf).await?;
    privileged(s, &session).await?;
    let language = reader_language(s, headers, &session).await?;
    let (webhook, secret) =
        s.db.browser_webhook_rotate(&session, pages.scope, id)
            .await?;
    let webhooks = s.db.browser_webhooks(&session, pages.scope).await?;
    Ok(render_webhooks(
        language,
        &session.csrf,
        pages,
        &webhooks,
        (String::new(), "*".into(), String::new()),
        None,
        Some(listmngr_i18n::message(language, "web-ls-saved", &[])),
        Some((webhook.id, secret)),
    ))
}

async fn webhook_remove(
    s: &AppState,
    headers: &HeaderMap,
    pages: &WebhookPages<'_>,
    id: WebhookId,
    form: CsrfForm,
) -> ApiResult<Response> {
    let session = write_session(s, headers, &form.csrf).await?;
    privileged(s, &session).await?;
    s.db.browser_webhook_remove(&session, pages.scope, id)
        .await?;
    Ok(Redirect::to(&format!("{}?saved=1", pages.path())).into_response())
}

pub(super) async fn list_webhooks(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(q): Query<WebhookNotice>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    webhooks_page(&s, &headers, &WebhookPages::list(&id), &q).await
}

pub(super) async fn list_webhook_add(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<WebhookForm>,
) -> ApiResult<Response> {
    webhook_add(&s, &headers, &WebhookPages::list(&id), form).await
}

pub(super) async fn list_webhook(
    State(s): State<AppState>,
    Path((id, webhook)): Path<(ListId, WebhookId)>,
    Query(q): Query<WebhookNotice>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    webhook_page(&s, &headers, &WebhookPages::list(&id), webhook, &q).await
}

pub(super) async fn list_webhook_enable(
    State(s): State<AppState>,
    Path((id, webhook)): Path<(ListId, WebhookId)>,
    headers: HeaderMap,
    Form(form): Form<EnableForm>,
) -> ApiResult<Response> {
    webhook_enable(&s, &headers, &WebhookPages::list(&id), webhook, form).await
}

pub(super) async fn list_webhook_ping(
    State(s): State<AppState>,
    Path((id, webhook)): Path<(ListId, WebhookId)>,
    headers: HeaderMap,
    Form(form): Form<CsrfForm>,
) -> ApiResult<Response> {
    webhook_ping(&s, &headers, &WebhookPages::list(&id), webhook, form).await
}

pub(super) async fn list_webhook_rotate(
    State(s): State<AppState>,
    Path((id, webhook)): Path<(ListId, WebhookId)>,
    headers: HeaderMap,
    Form(form): Form<CsrfForm>,
) -> ApiResult<Response> {
    webhook_rotate(&s, &headers, &WebhookPages::list(&id), webhook, form).await
}

pub(super) async fn list_webhook_remove(
    State(s): State<AppState>,
    Path((id, webhook)): Path<(ListId, WebhookId)>,
    headers: HeaderMap,
    Form(form): Form<CsrfForm>,
) -> ApiResult<Response> {
    webhook_remove(&s, &headers, &WebhookPages::list(&id), webhook, form).await
}

pub(super) async fn site_webhooks(
    State(s): State<AppState>,
    Query(q): Query<WebhookNotice>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    webhooks_page(&s, &headers, &WebhookPages::site(), &q).await
}

pub(super) async fn site_webhook_add(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<WebhookForm>,
) -> ApiResult<Response> {
    webhook_add(&s, &headers, &WebhookPages::site(), form).await
}

pub(super) async fn site_webhook(
    State(s): State<AppState>,
    Path(webhook): Path<WebhookId>,
    Query(q): Query<WebhookNotice>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    webhook_page(&s, &headers, &WebhookPages::site(), webhook, &q).await
}

pub(super) async fn site_webhook_enable(
    State(s): State<AppState>,
    Path(webhook): Path<WebhookId>,
    headers: HeaderMap,
    Form(form): Form<EnableForm>,
) -> ApiResult<Response> {
    webhook_enable(&s, &headers, &WebhookPages::site(), webhook, form).await
}

pub(super) async fn site_webhook_ping(
    State(s): State<AppState>,
    Path(webhook): Path<WebhookId>,
    headers: HeaderMap,
    Form(form): Form<CsrfForm>,
) -> ApiResult<Response> {
    webhook_ping(&s, &headers, &WebhookPages::site(), webhook, form).await
}

pub(super) async fn site_webhook_rotate(
    State(s): State<AppState>,
    Path(webhook): Path<WebhookId>,
    headers: HeaderMap,
    Form(form): Form<CsrfForm>,
) -> ApiResult<Response> {
    webhook_rotate(&s, &headers, &WebhookPages::site(), webhook, form).await
}

pub(super) async fn site_webhook_remove(
    State(s): State<AppState>,
    Path(webhook): Path<WebhookId>,
    headers: HeaderMap,
    Form(form): Form<CsrfForm>,
) -> ApiResult<Response> {
    webhook_remove(&s, &headers, &WebhookPages::site(), webhook, form).await
}
