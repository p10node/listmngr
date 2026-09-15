//! The moderator's queues: the index with counts, held posts with a rendered
//! preview and decisions one or many at a time (with a reason and a
//! forward), the sender's moderation and ban from a held post, and the
//! subscription requests queue.
use super::{
    ApiResult, AppState, BrowserPage, Form, HeaderMap, IntoResponse, Path, Query, Redirect,
    Response, Shell, State, html, inline_refusal, load, privileged, reader_language, scripted,
    write_session,
};
use listmngr_core::{Error, ListId, ModerationAction};
use listmngr_db::HeldPreview;
use listmngr_db::moderation::{HeldId, ReviewAction};
use listmngr_db::workflows::{RequestDecision, SubscriptionAction, TokenOwner};
use listmngr_web::{Held, HeldItem, Nav, RequestItem, Requests, choices};
use serde::Deserialize;

/// The moderator's pages.
pub(super) fn routes() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/web/moderation", get(index))
        .route("/web/lists/{id}/held", get(queue).post(bulk))
        .route("/web/lists/{id}/held/{held}", post(review))
        .route("/web/lists/{id}/held/{held}/sender", post(sender))
        .route("/web/lists/{id}/held/{held}/ban", post(ban))
        .route("/web/lists/{id}/requests", get(requests))
        .route("/web/lists/{id}/requests/{request}", post(decide))
}

type Options = &'static [(&'static str, &'static str)];

const DECISIONS: Options = &[
    ("defer", "web-held-defer"),
    ("accept", "web-held-accept"),
    ("reject", "web-held-reject"),
    ("discard", "web-held-discard"),
];
const SENDER_ACTIONS: Options = &[
    ("default", "web-policy-default"),
    ("defer", "web-action-defer"),
    ("accept", "web-policy-accept"),
    ("hold", "web-policy-hold"),
    ("reject", "web-policy-reject"),
    ("discard", "web-policy-discard"),
];
const REQUEST_DECISIONS: Options = &[
    ("defer", "web-held-defer"),
    ("accept", "web-requests-accept"),
    ("reject", "web-held-reject"),
    ("discard", "web-held-discard"),
];

fn held_base(list: &ListId) -> String {
    format!("/web/lists/{}/held", list.as_str())
}

fn requests_base(list: &ListId) -> String {
    format!("/web/lists/{}/requests", list.as_str())
}

fn stamp(at: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at)
        .unwrap_or_default()
        .format("%Y-%m-%d %H:%M UTC")
        .to_string()
}

fn review_action(value: &str) -> ApiResult<ReviewAction> {
    Ok(match value {
        "accept" => ReviewAction::Accept {
            max_attempts: crate::HELD_OUT_MAX_ATTEMPTS,
        },
        "reject" => ReviewAction::Reject,
        "discard" => ReviewAction::Discard,
        "defer" => ReviewAction::Defer,
        _ => return Err(Error::Validation("unsupported decision".into()).into()),
    })
}

/// `GET /web/moderation`: the lists the reader moderates, with counts.
pub(super) async fn index(
    State(s): State<AppState>,
    Query(paging): Query<BrowserPage>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    let language = reader_language(&s, &h, &session).await?;
    privileged(&s, &session).await?;
    let lists =
        s.db.browser_moderated_lists(&session, paging.offset()?)
            .await?;
    let more = lists.len() > 20;
    let rows = lists
        .into_iter()
        .take(20)
        .map(|list| listmngr_web::ModerationRow {
            href: format!("/web/lists/{}/held", list.id),
            requests_href: format!("/web/lists/{}/requests", list.id),
            label: listmngr_i18n::message(
                language,
                "web-moderation-held",
                &[("name", &list.display_name)],
            ),
            waiting: listmngr_i18n::message(
                language,
                "web-moderation-waiting",
                &[
                    ("held", &list.held.to_string()),
                    ("requests", &list.requests.to_string()),
                ],
            ),
        })
        .collect();
    Ok(html(&listmngr_web::Moderation {
        shell: Shell::new(language, "web-title-moderation", Nav::Moderation),
        rows,
        pagination: paging.pagination("/web/moderation", more),
    }))
}

#[derive(Deserialize)]
pub(super) struct HeldQuery {
    #[serde(default)]
    page: u32,
    #[serde(default)]
    done: Option<usize>,
    #[serde(default)]
    skipped: Option<usize>,
    #[serde(default)]
    saved: String,
}

fn item(language: &str, list: &ListId, preview: HeldPreview) -> HeldItem {
    let id = preview.held.id.0.to_string();
    let base = held_base(list);
    let standing = match &preview.sender_row {
        Some((role, action)) => listmngr_i18n::message(
            language,
            "web-held-sender-standing",
            &[
                ("role", role),
                (
                    "action",
                    &action.map_or_else(|| "default".to_owned(), |a| a.to_string()),
                ),
            ],
        ),
        None => listmngr_i18n::message(language, "web-held-sender-unknown", &[]),
    };
    let selected = preview.sender_row.as_ref().map_or_else(
        || "default".to_owned(),
        |(_, action)| action.map_or_else(|| "default".to_owned(), |a| a.to_string()),
    );
    let sender = preview.held.sender.trim().to_owned();
    HeldItem {
        rule_href: format!(
            "/web/lists/{}/settings/header-matches?{}",
            list.as_str(),
            serde_urlencoded::to_string([
                ("header", "From"),
                ("pattern", &format!("^{}$", regex_escape(&sender))),
            ])
            .unwrap_or_default()
        ),
        sender_action: format!("{base}/{id}/sender"),
        ban_action: (!preview.sender_banned).then(|| format!("{base}/{id}/ban")),
        sender_standing: standing,
        sender_choices: choices(language, SENDER_ACTIONS, Some(&selected)),
        action: format!("{base}/{id}"),
        held_at: stamp(preview.held.hold_date),
        subject: preview.held.subject,
        sender,
        reason: preview.held.reason,
        from: preview.from,
        to: preview.to,
        date: preview.date,
        body: preview.body,
        attachments: preview.attachments,
        source: preview.raw,
        forward_to: String::new(),
        forward_error: None,
        id,
    }
}

/// Escape a literal for a header rule's regular expression.
fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 4);
    for c in text.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[allow(clippy::too_many_arguments)]
async fn render_queue(
    s: &AppState,
    language: &str,
    session: &listmngr_db::web_sessions::WebSession,
    list: &ListId,
    page: u32,
    notice: Option<String>,
    error: Option<String>,
    refused: Option<(HeldId, String, String)>,
) -> ApiResult<Response> {
    let offset = BrowserPage { page }.offset()?;
    let previews = s.db.browser_held_queue(session, list, offset).await?;
    let more = previews.len() > 20;
    let items = previews
        .into_iter()
        .take(20)
        .map(|preview| {
            let mut item = item(language, list, preview);
            if let Some((id, forward_to, message)) = &refused
                && item.id == id.0.to_string()
            {
                item.forward_to.clone_from(forward_to);
                item.forward_error = Some(message.clone());
            }
            item
        })
        .collect();
    Ok(scripted(html(&Held {
        shell: Shell::new(language, "web-title-held", Nav::Moderation)
            .with_script("/web/moderation.js"),
        csrf: session.csrf.clone(),
        bulk_action: held_base(list),
        requests_href: requests_base(list),
        items,
        decisions: choices(language, DECISIONS, None),
        sender_actions: choices(language, SENDER_ACTIONS, None),
        notice,
        error,
        pagination: BrowserPage { page }.pagination(&held_base(list), more),
    })))
}

/// `GET /web/lists/{id}/held`.
pub(super) async fn queue(
    State(s): State<AppState>,
    Path(list): Path<ListId>,
    Query(q): Query<HeldQuery>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    let language = reader_language(&s, &h, &session).await?;
    privileged(&s, &session).await?;
    let notice = match (q.done, q.saved.as_str()) {
        (Some(done), _) => Some(listmngr_i18n::message(
            language,
            "web-held-done",
            &[
                ("done", &done.to_string()),
                ("skipped", &q.skipped.unwrap_or(0).to_string()),
            ],
        )),
        (None, "sender") => Some(listmngr_i18n::message(
            language,
            "web-held-sender-saved",
            &[],
        )),
        (None, "ban") => Some(listmngr_i18n::message(language, "web-held-ban-done", &[])),
        _ => None,
    };
    render_queue(&s, language, &session, &list, q.page, notice, None, None).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Review {
    #[serde(default)]
    csrf: String,
    action: String,
    #[serde(default)]
    comment: String,
    #[serde(default)]
    forward_to: String,
}

fn forward(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

/// `POST /web/lists/{id}/held/{held}`: one decision.
pub(super) async fn review(
    State(s): State<AppState>,
    Path((list, held)): Path<(ListId, uuid::Uuid)>,
    h: HeaderMap,
    Form(f): Form<Review>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &f.csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let held = HeldId(held);
    if f.comment.len() > 2000 {
        return Err(Error::Validation("comment too long".into()).into());
    }
    let action = review_action(&f.action)?;
    match s
        .db
        .browser_review_forwarding(
            &session,
            &list,
            held,
            &action,
            &f.comment,
            forward(&f.forward_to),
        )
        .await
    {
        Ok(()) => Ok(Redirect::to(&format!("{}?done=1", held_base(&list))).into_response()),
        Err(Error::Validation(message)) if message.contains("forward") => {
            let text = listmngr_i18n::message(language, "web-held-forward-refused", &[]);
            Ok(inline_refusal(
                render_queue(
                    &s,
                    language,
                    &session,
                    &list,
                    0,
                    None,
                    None,
                    Some((held, f.forward_to.clone(), text)),
                )
                .await?,
            ))
        }
        Err(other) => Err(other.into()),
    }
}

/// `POST /web/lists/{id}/held`: one decision for the selected posts.
pub(super) async fn bulk(
    State(s): State<AppState>,
    Path(list): Path<ListId>,
    h: HeaderMap,
    body: String,
) -> ApiResult<Response> {
    let pairs: Vec<(String, String)> =
        serde_urlencoded::from_str(&body).map_err(|_| Error::Validation("form".into()))?;
    let field = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    };
    let session = write_session(&s, &h, &field("csrf")).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let ids: Vec<HeldId> = pairs
        .iter()
        .filter(|(key, _)| key == "held")
        .map(|(_, value)| value.parse::<uuid::Uuid>().map(HeldId))
        .collect::<Result<_, _>>()
        .map_err(|_| Error::Validation("held".into()))?;
    if ids.len() > 200 {
        return Err(Error::Validation("too many held messages at once".into()).into());
    }
    let comment = field("comment");
    if comment.len() > 2000 {
        return Err(Error::Validation("comment too long".into()).into());
    }
    let action = review_action(&field("action"))?;
    let forward_to = field("forward_to");
    if ids.is_empty() {
        let text = listmngr_i18n::message(language, "web-held-none-selected", &[]);
        return Ok(inline_refusal(
            render_queue(&s, language, &session, &list, 0, None, Some(text), None).await?,
        ));
    }
    let (done, skipped) =
        s.db.browser_review_many(
            &session,
            &list,
            &ids,
            &action,
            &comment,
            forward(&forward_to),
            super::now(),
        )
        .await?;
    Ok(Redirect::to(&format!(
        "{}?done={done}&skipped={skipped}",
        held_base(&list)
    ))
    .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SenderForm {
    #[serde(default)]
    csrf: String,
    action: String,
}

/// `POST /web/lists/{id}/held/{held}/sender`: moderate the sender.
pub(super) async fn sender(
    State(s): State<AppState>,
    Path((list, held)): Path<(ListId, uuid::Uuid)>,
    h: HeaderMap,
    Form(f): Form<SenderForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &f.csrf).await?;
    privileged(&s, &session).await?;
    let action = if f.action == "default" {
        None
    } else {
        Some(f.action.parse::<ModerationAction>()?)
    };
    s.db.browser_moderate_sender(&session, &list, HeldId(held), action)
        .await?;
    Ok(Redirect::to(&format!("{}?saved=sender", held_base(&list))).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Csrf {
    #[serde(default)]
    csrf: String,
}

/// `POST /web/lists/{id}/held/{held}/ban`: ban the sender.
pub(super) async fn ban(
    State(s): State<AppState>,
    Path((list, held)): Path<(ListId, uuid::Uuid)>,
    h: HeaderMap,
    Form(f): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &f.csrf).await?;
    privileged(&s, &session).await?;
    s.db.browser_ban_sender(&session, &list, HeldId(held))
        .await?;
    Ok(Redirect::to(&format!("{}?saved=ban", held_base(&list))).into_response())
}

// ----- subscription requests --------------------------------------------------

#[derive(Deserialize)]
pub(super) struct RequestsQuery {
    #[serde(default)]
    saved: String,
}

/// `GET /web/lists/{id}/requests`.
pub(super) async fn requests(
    State(s): State<AppState>,
    Path(list): Path<ListId>,
    Query(q): Query<RequestsQuery>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    let language = reader_language(&s, &h, &session).await?;
    privileged(&s, &session).await?;
    let items =
        s.db.browser_requests(&session, &list)
            .await?
            .into_iter()
            .map(|request| RequestItem {
                decide_action: format!("{}/{}", requests_base(&list), request.id),
                action: listmngr_i18n::message(
                    language,
                    match request.action {
                        SubscriptionAction::Join => "web-requests-join",
                        SubscriptionAction::Leave => "web-requests-leave",
                    },
                    &[],
                ),
                waiting: listmngr_i18n::message(
                    language,
                    match request.token_owner {
                        TokenOwner::Subscriber => "web-requests-waiting-address",
                        TokenOwner::Moderator => "web-requests-waiting-moderator",
                    },
                    &[],
                ),
                requested_at: stamp(request.requested_at),
                email: request.email,
                display_name: request.display_name,
            })
            .collect();
    let notice =
        (q.saved == "1").then(|| listmngr_i18n::message(language, "web-requests-done", &[]));
    Ok(html(&Requests {
        shell: Shell::new(language, "web-title-requests", Nav::Moderation),
        csrf: session.csrf.clone(),
        held_href: held_base(&list),
        items,
        decisions: choices(language, REQUEST_DECISIONS, None),
        notice,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DecideForm {
    #[serde(default)]
    csrf: String,
    decision: String,
    #[serde(default)]
    reason: String,
}

/// `POST /web/lists/{id}/requests/{request}`.
pub(super) async fn decide(
    State(s): State<AppState>,
    Path((list, request)): Path<(ListId, String)>,
    h: HeaderMap,
    Form(f): Form<DecideForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &f.csrf).await?;
    privileged(&s, &session).await?;
    if f.reason.len() > 2000 {
        return Err(Error::Validation("reason too long".into()).into());
    }
    let decision = match f.decision.as_str() {
        "accept" => RequestDecision::Accept,
        "reject" => RequestDecision::Reject,
        "discard" => RequestDecision::Discard,
        "defer" => RequestDecision::Defer,
        _ => return Err(Error::Validation("unsupported decision".into()).into()),
    };
    s.db.browser_decide_request(&session, &list, &request, decision, &f.reason)
        .await?;
    Ok(Redirect::to(&format!("{}?saved=1", requests_base(&list))).into_response())
}
