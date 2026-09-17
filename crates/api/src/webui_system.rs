//! The server owner's system page — versions, the redacted configuration,
//! queue and runner status, MTA map status — and the audit log viewer.
use super::{
    ApiResult, AppState, BrowserPage, HeaderMap, Query, Response, Shell, State, html, load,
    privileged, reader_language,
};
use listmngr_core::Error;
use listmngr_db::web_system::AuditFilter;
use listmngr_web::{Fact, Nav, Pagination};
use serde::Deserialize;

fn t(language: &str, id: &str) -> String {
    listmngr_i18n::message(language, id, &[])
}

/// A value shown on the page: bounded, one line.
fn shown(value: &serde_json::Value) -> String {
    let text = match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    if text.chars().count() > 512 {
        let mut cut: String = text.chars().take(512).collect();
        cut.push('…');
        cut
    } else {
        text
    }
}

/// The redacted configuration as dotted keys, arrays as one JSON value.
fn flatten(prefix: &str, value: &serde_json::Value, out: &mut Vec<Fact>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, inner) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&path, inner, out);
            }
        }
        other => out.push(Fact {
            label: prefix.to_owned(),
            value: shown(other),
        }),
    }
}

/// The MTA map writer's status: what is configured and which generation
/// the `current` link names, if any.
fn mta_facts(s: &AppState, language: &str) -> (Vec<Fact>, Option<String>) {
    let Some(writer) = s.mta_maps.as_ref() else {
        return (Vec::new(), Some(t(language, "web-system-mta-none")));
    };
    let current = writer.directory.join("current");
    let generation = std::fs::read_link(&current).ok().map(|target| {
        let name = target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let published = std::fs::metadata(&current)
            .and_then(|metadata| metadata.modified())
            .map(|time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339())
            .unwrap_or_default();
        format!("{name} ({published})")
    });
    (
        vec![
            Fact {
                label: t(language, "web-system-mta-kind"),
                value: s.config.mta.incoming.clone(),
            },
            Fact {
                label: t(language, "web-system-mta-directory"),
                value: writer.directory.display().to_string(),
            },
            Fact {
                label: t(language, "web-system-mta-target"),
                value: writer.lmtp_target.postfix_transport(),
            },
            Fact {
                label: t(language, "web-system-mta-generation"),
                value: generation.unwrap_or_else(|| t(language, "web-system-mta-no-generation")),
            },
        ],
        None,
    )
}

pub(super) async fn index(State(s): State<AppState>, h: HeaderMap) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let status = s.db.browser_runner_status(&session).await?;
    let mut config = Vec::new();
    flatten("", &s.config.redacted_json(), &mut config);
    let (mta, mta_note) = mta_facts(&s, language);
    let queues = listmngr_db::mail_queue::Queue::ALL
        .iter()
        .map(|queue| {
            let by_state = status.stats.queues.get(queue.name());
            let count = |state: &str| {
                by_state
                    .and_then(|counts| counts.get(state))
                    .copied()
                    .unwrap_or(0)
            };
            listmngr_web::QueueRow {
                name: queue.name().to_owned(),
                ready: count("ready"),
                leased: count("leased"),
                done: count("done"),
                shunted: count("shunted"),
            }
        })
        .collect();
    Ok(html(&listmngr_web::SystemPage {
        shell: Shell::new(language, "web-title-system", Nav::Account),
        versions: vec![
            Fact {
                label: t(language, "web-system-version"),
                value: env!("CARGO_PKG_VERSION").to_owned(),
            },
            Fact {
                label: t(language, "web-system-api-version"),
                value: "3.1".into(),
            },
            Fact {
                label: t(language, "web-system-database"),
                value: s.config.database_backend().to_owned(),
            },
        ],
        config,
        queues,
        shunted: status.stats.shunted,
        oldest_ready: status.stats.oldest_ready_age_secs.map_or_else(
            || t(language, "web-system-none-ready"),
            |seconds| {
                listmngr_i18n::message(
                    language,
                    "web-system-oldest-ready",
                    &[("count", &seconds.to_string())],
                )
            },
        ),
        runners: status
            .runners
            .into_iter()
            .map(|(runner, jobs)| Fact {
                label: runner,
                value: jobs.to_string(),
            })
            .collect(),
        mta,
        mta_note,
        audit_href: "/web/admin/system/audit".into(),
    }))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct AuditQuery {
    #[serde(default)]
    page: u32,
    #[serde(default)]
    action: String,
    #[serde(default)]
    target: String,
}

pub(super) async fn audit(
    State(s): State<AppState>,
    Query(f): Query<AuditQuery>,
    h: HeaderMap,
) -> ApiResult<Response> {
    if f.action.len() > 100 || f.target.len() > 320 {
        return Err(Error::Validation("audit filter".into()).into());
    }
    let session = load(&s, &h).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let paging = BrowserPage { page: f.page };
    let rows =
        s.db.browser_audit(
            &session,
            &AuditFilter {
                action: &f.action,
                target: &f.target,
                offset: paging.offset()?,
            },
        )
        .await?;
    let more = rows.len() > 20;
    let system = t(language, "web-audit-system-actor");
    let rows = rows
        .into_iter()
        .take(20)
        .map(|entry| listmngr_web::AuditRow {
            at: entry.at.to_rfc3339(),
            actor: match (entry.actor_user_id, entry.actor_token_id) {
                (Some(user), _) => user.to_string(),
                (None, Some(token)) => format!("token {token}"),
                (None, None) => system.clone(),
            },
            ip: entry.ip.map(|ip| ip.to_string()).unwrap_or_default(),
            action: entry.action,
            target: format!("{} {}", entry.target_type, entry.target_id),
            diff: shown(&entry.diff),
        })
        .collect();
    let mut filters: Vec<(&str, &str)> = Vec::new();
    if !f.action.is_empty() {
        filters.push(("action", &f.action));
    }
    if !f.target.is_empty() {
        filters.push(("target", &f.target));
    }
    let filters = serde_urlencoded::to_string(&filters)
        .map_err(|_| Error::Validation("audit filter".into()))?;
    Ok(html(&listmngr_web::AuditPage {
        shell: Shell::new(language, "web-title-audit", Nav::Account),
        action: f.action.clone(),
        target: f.target.clone(),
        rows,
        pagination: Pagination::filtered(
            "/web/admin/system/audit",
            &filters,
            f.page,
            more,
            BrowserPage::LAST,
        ),
    }))
}
