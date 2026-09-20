//! The server owner's domains: the index with an add form, one domain with
//! its owners, template overrides, DKIM DNS record and deletion, and the
//! domain-scope template editor.
use super::{
    ApiResult, AppState, Form, HeaderMap, Path, Query, Redirect, Response, Shell, State, html,
    inline_refusal, load, privileged, reader_language, write_session,
};
use axum::response::IntoResponse;
use listmngr_core::{Error, UserId};
use listmngr_db::web_domains::DomainSummary;
use listmngr_web::{Fact, GroupLink, Nav, SettingField, TemplateEditor, TemplateRow};
use serde::Deserialize;

const LANGUAGES: &[(&str, &str)] = &[("en", "web-language-en"), ("vi", "web-language-vi")];

fn t(language: &str, id: &str) -> String {
    listmngr_i18n::message(language, id, &[])
}

fn base(host: &str) -> String {
    format!("/web/admin/domains/{host}")
}

/// The index's query string: a notice after a redirect.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct Notice {
    #[serde(default)]
    saved: String,
    #[serde(default)]
    language: String,
}

fn notice(language: &str, q: &Notice) -> Option<String> {
    let id = match q.saved.as_str() {
        "added" => "web-domains-added",
        "deleted" => "web-domains-deleted",
        "owner" => "web-domain-owner-added",
        "owner-removed" => "web-domain-owner-removed",
        "removed" => "web-ls-saved",
        _ => return None,
    };
    Some(t(language, id))
}

/// The add-domain form as submitted.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct DomainForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    mail_host: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    alias_domain: String,
}

fn render_index(
    language: &str,
    csrf: &str,
    rows: Vec<DomainSummary>,
    form: &DomainForm,
    host_error: Option<&str>,
    notice: Option<String>,
) -> Response {
    let field = |name: &'static str, value: &str, help: &str, error: Option<&str>| SettingField {
        name: name.to_owned(),
        label: t(language, &format!("web-domains-{}", name.replace('_', "-"))),
        help: if help.is_empty() {
            String::new()
        } else {
            t(language, help)
        },
        kind: "text".into(),
        value: value.to_owned(),
        choices: Vec::new(),
        error: error.map(|id| t(language, id)),
    };
    html(&listmngr_web::Domains {
        shell: Shell::new(language, "web-title-domains", Nav::Account),
        csrf: csrf.to_owned(),
        rows: rows
            .into_iter()
            .map(|row| listmngr_web::DomainRow {
                href: base(&row.domain.mail_host),
                owners: row
                    .owners
                    .iter()
                    .map(|owner| {
                        if owner.email.is_empty() {
                            owner.display_name.clone()
                        } else {
                            owner.email.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                mail_host: row.domain.mail_host,
                description: row.domain.description,
                alias: row.domain.alias_domain.unwrap_or_default(),
                lists: row.lists,
            })
            .collect(),
        fields: vec![
            field(
                "mail_host",
                &form.mail_host,
                "web-domains-mail-host-help",
                host_error,
            ),
            field("description", &form.description, "", None),
            field(
                "alias_domain",
                &form.alias_domain,
                "web-domains-alias-domain-help",
                None,
            ),
        ],
        error: None,
        notice,
    })
}

pub(super) async fn index(
    State(s): State<AppState>,
    Query(q): Query<Notice>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let rows = s.db.browser_domains(&session).await?;
    Ok(render_index(
        language,
        &session.csrf,
        rows,
        &DomainForm::default(),
        None,
        notice(language, &q),
    ))
}

pub(super) async fn create(
    State(s): State<AppState>,
    h: HeaderMap,
    Form(form): Form<DomainForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &form.csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let alias = form.alias_domain.trim();
    let alias = (!alias.is_empty()).then_some(alias);
    if alias.is_some_and(|alias| listmngr_core::normalize_domain(alias).is_err()) {
        let rows = s.db.browser_domains(&session).await?;
        return Ok(inline_refusal(render_index(
            language,
            &session.csrf,
            rows,
            &form,
            Some("web-domains-bad-host"),
            None,
        )));
    }
    let result =
        s.db.browser_domain_create(&session, &form.mail_host, form.description.trim(), alias)
            .await;
    let refusal = match result {
        Ok(domain) => {
            return Ok(
                Redirect::to(&format!("{}?saved=added", base(&domain.mail_host))).into_response(),
            );
        }
        Err(Error::Validation(_)) => "web-domains-bad-host",
        Err(Error::Conflict(_)) => "web-domains-exists",
        Err(error) => return Err(error.into()),
    };
    let rows = s.db.browser_domains(&session).await?;
    Ok(inline_refusal(render_index(
        language,
        &session.csrf,
        rows,
        &form,
        Some(refusal),
        None,
    )))
}

/// The domain page's query string: a notice after a redirect.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct DomainQuery {
    #[serde(default)]
    saved: String,
}

/// The DKIM keys published for `host`, and the ARC sealing key when it is
/// the sealing domain: both are `<selector>._domainkey.<domain>` records.
fn dkim_records(s: &AppState, language: &str, host: &str) -> Vec<Fact> {
    let arc = &s.config.mta.arc;
    let sealing = (arc.enabled && arc.domain == host)
        .then(|| {
            arc.private_key_file
                .as_ref()
                .map(|key| listmngr_core::DkimSigningConfig {
                    domain: arc.domain.clone(),
                    selector: arc.selector.clone(),
                    private_key_file: key.clone(),
                })
        })
        .flatten();
    s.config
        .mta
        .dkim_signing
        .iter()
        .filter(|entry| entry.domain == host)
        .chain(sealing.as_ref())
        .map(|entry| {
            listmngr_mail::dkim::SigningKeys::dns_record(entry).map_or_else(
                |_| Fact {
                    label: format!("{}._domainkey.{}", entry.selector, entry.domain),
                    value: t(language, "web-domain-dkim-unreadable"),
                },
                |(name, txt)| Fact {
                    label: name,
                    value: txt,
                },
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn render_domain(
    s: &AppState,
    language: &str,
    csrf: &str,
    summary: DomainSummary,
    owner_email: &str,
    owner_error: Option<&str>,
    delete_error: Option<&str>,
    notice: Option<String>,
) -> ApiResult<Response> {
    let host = summary.domain.mail_host.clone();
    let stored = s.db.browser_domain_templates_unchecked(&host).await?;
    let templates = listmngr_mail::templates::NAMES
        .iter()
        .filter(|name| !name.starts_with("site:"))
        .map(|name| TemplateRow {
            name: (*name).to_owned(),
            href: format!("{}/templates/{name}", base(&host)),
            languages: stored
                .iter()
                .filter(|(stored_name, _)| stored_name == name)
                .map(|(_, language)| language.clone())
                .collect(),
        })
        .collect();
    Ok(html(&listmngr_web::DomainPage {
        shell: Shell::titled(language, host.clone(), Nav::Account),
        csrf: csrf.to_owned(),
        facts: vec![
            Fact {
                label: t(language, "web-domains-description"),
                value: summary.domain.description.clone(),
            },
            Fact {
                label: t(language, "web-domains-alias"),
                value: summary.domain.alias_domain.clone().unwrap_or_default(),
            },
            Fact {
                label: t(language, "web-domains-lists"),
                value: summary.lists.to_string(),
            },
            Fact {
                label: t(language, "web-domain-created"),
                value: summary.domain.created_at.to_rfc3339(),
            },
        ],
        owners: summary
            .owners
            .iter()
            .map(|owner| listmngr_web::OwnerRow {
                display_name: owner.display_name.clone(),
                email: owner.email.clone(),
                href: format!("/web/admin/users/{}", owner.user),
                remove_action: format!("{}/owners/{}/remove", base(&host), owner.user),
            })
            .collect(),
        owner_action: format!("{}/owners", base(&host)),
        owner_error: owner_error.map(|id| t(language, id)),
        owner_email: owner_email.to_owned(),
        templates,
        dkim: dkim_records(s, language, &host),
        delete_action: format!("{}/delete", base(&host)),
        delete_error: delete_error.map(|id| t(language, id)),
        notice,
        lists_href: format!("/web?domain={host}&show=all"),
        mail_host: host,
    }))
}

pub(super) async fn domain(
    State(s): State<AppState>,
    Path(host): Path<String>,
    Query(q): Query<DomainQuery>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let summary = s.db.browser_domain(&session, &host).await?;
    let notice = notice(
        language,
        &Notice {
            saved: q.saved,
            language: String::new(),
        },
    );
    render_domain(&s, language, &session.csrf, summary, "", None, None, notice).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnerForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    email: String,
}

pub(super) async fn owner_add(
    State(s): State<AppState>,
    Path(host): Path<String>,
    h: HeaderMap,
    Form(form): Form<OwnerForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &form.csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let summary = s.db.browser_domain(&session, &host).await?;
    let refusal = match s
        .db
        .browser_domain_owner_add(&session, &host, &form.email)
        .await
    {
        Ok(_) => {
            return Ok(
                Redirect::to(&format!("{}?saved=owner", base(&summary.domain.mail_host)))
                    .into_response(),
            );
        }
        Err(Error::NotFound(_) | Error::Validation(_)) => "web-domain-owner-unknown",
        Err(Error::Conflict(_)) => "web-domain-owner-already",
        Err(error) => return Err(error.into()),
    };
    Ok(inline_refusal(
        render_domain(
            &s,
            language,
            &session.csrf,
            summary,
            &form.email,
            Some(refusal),
            None,
            None,
        )
        .await?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Csrf {
    #[serde(default)]
    csrf: String,
}

pub(super) async fn owner_remove(
    State(s): State<AppState>,
    Path((host, user)): Path<(String, UserId)>,
    h: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &form.csrf).await?;
    privileged(&s, &session).await?;
    s.db.browser_domain_owner_remove(&session, &host, user)
        .await?;
    Ok(Redirect::to(&format!("{}?saved=owner-removed", base(&host))).into_response())
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
    Path(host): Path<String>,
    h: HeaderMap,
    Form(form): Form<DeleteForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &form.csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let summary = s.db.browser_domain(&session, &host).await?;
    let refusal = if form.confirm.trim().to_ascii_lowercase() == summary.domain.mail_host {
        match s.db.browser_domain_delete(&session, &host).await {
            Ok(()) => return Ok(Redirect::to("/web/admin/domains?saved=deleted").into_response()),
            Err(Error::Conflict(_)) => "web-domain-delete-lists",
            Err(error) => return Err(error.into()),
        }
    } else {
        "web-domain-delete-mismatch"
    };
    Ok(inline_refusal(
        render_domain(
            &s,
            language,
            &session.csrf,
            summary,
            "",
            None,
            Some(refusal),
            None,
        )
        .await?,
    ))
}

// ----- the domain-scope template editor ------------------------------------

fn groups(language: &str, host: &str) -> Vec<GroupLink> {
    vec![
        GroupLink {
            href: "/web/admin/domains".into(),
            label: t(language, "web-title-domains"),
            current: false,
        },
        GroupLink {
            href: base(host),
            label: host.to_owned(),
            current: true,
        },
    ]
}

#[allow(clippy::too_many_arguments)]
fn render_editor(
    language: &str,
    csrf: &str,
    host: &str,
    name: &str,
    view: &listmngr_db::TemplateView,
    template_language: &str,
    body: String,
    preview: Option<String>,
    error: Option<String>,
) -> Response {
    let values = listmngr_mail::templates::Placeholders::new()
        .set("domain", host)
        .set("site_name", "Example Lists")
        .set("listname", "example")
        .set("list_id", format!("example.{host}"))
        .set("display_name", "Example")
        .set("fqdn_listname", format!("example@{host}"))
        .set("user_email", "member@example.org")
        .set("user_name", "A. Member")
        .set("token", "0123456789abcdef");
    let placeholders = listmngr_mail::templates::PLACEHOLDER_NAMES
        .iter()
        .map(|name| Fact {
            label: (*name).to_owned(),
            value: values.get(name).unwrap_or_default().to_owned(),
        })
        .collect();
    html(&TemplateEditor {
        shell: Shell::new(language, "web-title-domains", Nav::Account),
        groups: groups(language, host),
        name: name.to_owned(),
        action: format!("{}/templates/{name}", base(host)),
        catalogue_href: base(host),
        csrf: csrf.to_owned(),
        languages: listmngr_web::choices(language, LANGUAGES, Some(template_language)),
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
    Path((host, name)): Path<(String, String)>,
    Query(q): Query<Notice>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let template_language = if q.language.is_empty() {
        "en"
    } else {
        q.language.as_str()
    };
    if !LANGUAGES.iter().any(|(code, _)| *code == template_language) {
        return Err(Error::Validation("language".into()).into());
    }
    let view =
        s.db.browser_domain_template(&session, &host, &name, template_language)
            .await?;
    let body = view
        .stored
        .clone()
        .unwrap_or_else(|| view.effective.clone());
    Ok(render_editor(
        language,
        &session.csrf,
        &host,
        &name,
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
    Path((host, name)): Path<(String, String)>,
    h: HeaderMap,
    Form(form): Form<TemplateForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &form.csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    if !LANGUAGES.iter().any(|(code, _)| *code == form.language) {
        return Err(Error::Validation("language".into()).into());
    }
    let view =
        s.db.browser_domain_template(&session, &host, &name, &form.language)
            .await?;
    if form.preview == "1" {
        let values = listmngr_mail::templates::Placeholders::new()
            .set("domain", host.as_str())
            .set("site_name", "Example Lists");
        let rendered = listmngr_mail::templates::expand(&form.body, &values);
        return Ok(render_editor(
            language,
            &session.csrf,
            &host,
            &name,
            &view,
            &form.language,
            form.body,
            Some(rendered),
            None,
        ));
    }
    match s
        .db
        .browser_domain_template_set(&session, &host, &name, &form.language, &form.body)
        .await
    {
        Ok(()) => Ok(Redirect::to(&format!(
            "{}/templates/{name}?language={}",
            base(&host),
            form.language
        ))
        .into_response()),
        Err(Error::Validation(message)) => Ok(inline_refusal(render_editor(
            language,
            &session.csrf,
            &host,
            &name,
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
    Path((host, name)): Path<(String, String)>,
    h: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &form.csrf).await?;
    privileged(&s, &session).await?;
    s.db.browser_domain_template_delete(&session, &host, &name)
        .await?;
    Ok(Redirect::to(&format!("{}?saved=removed", base(&host))).into_response())
}
