//! Mailman's `/lists/{id}/header-matches`: the list's own header rules,
//! addressed by position, on both API prefixes.
use crate::{
    ApiResult, AppState, ErrorResponse, HeaderMatchPageResponse, JsonOrForm, PageQuery,
    audit_context, authorize_list, page_response, page_window, parse_list_path, peer,
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing,
};
use listmngr_core::{Error, ListId};
use listmngr_db::{FieldEdit, HeaderMatchPatch, HeaderMatchRow};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::net::SocketAddr;

/// One rule, in the typed shape; the compatibility prefix renders the chain
/// under Mailman's `action` name and omits absent optional fields.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct HeaderMatchResponse {
    /// Lower-cased header field name.
    pub header: String,
    /// Regular expression searched in every occurrence of the header.
    pub pattern: String,
    pub position: usize,
    /// Terminal chain to jump to; `null` means the site default.
    pub chain: Option<String>,
    pub tag: Option<String>,
    pub self_link: String,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HeaderMatchInput {
    header: String,
    pattern: String,
    /// Mailman's name for the chain: `accept`, `hold`, `reject` or `discard`.
    #[serde(default)]
    action: Option<String>,
    /// The same chain under its own name; give one or the other.
    #[serde(default)]
    chain: Option<String>,
    #[serde(default)]
    tag: Option<String>,
}

/// Mailman's `find`: every given field must match; `action` and `chain`
/// name the same thing.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HeaderMatchFindInput {
    #[serde(default)]
    header: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    chain: Option<String>,
}

/// A `PATCH` names only what changes; a `PUT` must carry the header and the
/// pattern and resets what it leaves out. `null` or an empty value clears an
/// optional field.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HeaderMatchPatchInput {
    #[serde(default)]
    header: Option<String>,
    #[serde(default)]
    pattern: Option<String>,
    /// Mailman's name for the chain.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    action: Edit,
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    chain: Edit,
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    tag: Edit,
    /// New position for the row; the rows in between shift.
    #[serde(default, deserialize_with = "position_field")]
    #[schema(value_type = Option<usize>)]
    position: Option<usize>,
}

/// One optional field of an edit: absent leaves it alone, `null` or an
/// empty value clears it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
enum Edit {
    #[default]
    Keep,
    Clear,
    Set(String),
}

impl Edit {
    /// The repository's edit; a reset (`PUT`) clears what was left out.
    fn apply(self, reset: bool) -> FieldEdit {
        match self {
            Self::Keep if reset => FieldEdit::Clear,
            Self::Keep => FieldEdit::Keep,
            Self::Clear => FieldEdit::Clear,
            Self::Set(value) => FieldEdit::Set(value),
        }
    }

    fn value(&self) -> Option<&str> {
        match self {
            Self::Set(value) => Some(value),
            Self::Keep | Self::Clear => None,
        }
    }
}

impl<'de> Deserialize<'de> for Edit {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Option::<String>::deserialize(deserializer)?
            .filter(|value| !value.trim().is_empty())
            .map_or(Self::Clear, Self::Set))
    }
}

/// A position arrives as a JSON number or a form string.
fn position_field<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<usize>, D::Error> {
    let invalid = || serde::de::Error::custom("position must be a non-negative integer");
    match Value::deserialize(deserializer)? {
        Value::Null => Ok(None),
        Value::Number(number) => number
            .as_u64()
            .and_then(|number| usize::try_from(number).ok())
            .map(Some)
            .ok_or_else(invalid),
        Value::String(text) => parse_position(text.trim()).map(Some).ok_or_else(invalid),
        _ => Err(invalid()),
    }
}

/// Digits only, as Mailman's `{position:int}` route segment.
fn parse_position(raw: &str) -> Option<usize> {
    (!raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| raw.parse().ok())
        .flatten()
}

fn path_position(raw: &str) -> ApiResult<usize> {
    parse_position(raw).ok_or_else(|| Error::NotFound(format!("header match {raw}")).into())
}

/// Mailman lower-cases the header name; an empty optional value is absent.
fn normalize_header(header: &str) -> String {
    header.trim().to_ascii_lowercase()
}

fn optional(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

/// `action` and `chain` name the same thing; both at once must agree.
fn chain_from(action: Option<String>, chain: Option<String>) -> ApiResult<Option<String>> {
    match (optional(action), optional(chain)) {
        (Some(action), Some(chain)) if action != chain => {
            Err(Error::Validation("action and chain name different chains".into()).into())
        }
        (action, chain) => Ok(action.or(chain)),
    }
}

/// The two edit verbs share one patch; `PUT` fills every field.
fn patch_from(input: HeaderMatchPatchInput, replace: bool) -> ApiResult<HeaderMatchPatch> {
    if replace && (input.header.is_none() || input.pattern.is_none()) {
        return Err(Error::Validation("header and pattern are required".into()).into());
    }
    let chain = match (&input.action, &input.chain) {
        (Edit::Keep, Edit::Keep) => Edit::Keep.apply(replace),
        (action, chain) => chain_from(
            action.value().map(str::to_owned),
            chain.value().map(str::to_owned),
        )?
        .map_or(FieldEdit::Clear, FieldEdit::Set),
    };
    Ok(HeaderMatchPatch {
        header: input.header.as_deref().map(normalize_header),
        pattern: input.pattern,
        chain,
        tag: input.tag.apply(replace),
        position: input.position,
    })
}

fn link(state: &AppState, id: &ListId, position: usize) -> String {
    let prefix = match state.flavor {
        crate::ApiFlavor::V1 => "/api/v1",
        crate::ApiFlavor::Compat31 => "/3.1",
    };
    format!("{prefix}/lists/{id}/header-matches/{position}")
}

fn value(state: &AppState, id: &ListId, position: usize, row: &HeaderMatchRow) -> Value {
    let self_link = link(state, id, position);
    match state.flavor {
        crate::ApiFlavor::V1 => json!(HeaderMatchResponse {
            header: row.header.clone(),
            pattern: row.pattern.clone(),
            position,
            chain: row.chain.clone(),
            tag: row.tag.clone(),
            self_link,
        }),
        crate::ApiFlavor::Compat31 => {
            let mut entry = json!({
                "header": row.header,
                "pattern": row.pattern,
                "position": position,
                "self_link": self_link,
                "http_etag": "phase1"
            });
            let object = entry.as_object_mut().expect("entry object");
            if let Some(chain) = &row.chain {
                object.insert("action".into(), json!(chain));
            }
            if let Some(tag) = &row.tag {
                object.insert("tag".into(), json!(tag));
            }
            entry
        }
    }
}

/// The list-scoped header match routes, mounted under both API prefixes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/lists/{id}/header-matches",
            routing::get(list).post(create).delete(clear),
        )
        .route("/lists/{id}/header-matches/find", routing::post(find))
        .route(
            "/lists/{id}/header-matches/{position}",
            routing::get(get).patch(patch).put(put).delete(delete),
        )
}

#[utoipa::path(get, path = "/api/v1/lists/{id}/header-matches", params(("id" = String, Path), PageQuery),
    responses((status = 200, description = "The list's header rules in position order", body = HeaderMatchPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn list(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    let rows = s.db.header_matches().list(&id).await?;
    let (start, end) = page_window(rows.len(), &q)?;
    let entries: Vec<Value> = rows[start..end]
        .iter()
        .enumerate()
        .map(|(offset, row)| value(&s, &id, start + offset, row))
        .collect();
    Ok(Json(page_response(s.flavor, &entries, start, rows.len())))
}

#[utoipa::path(post, path = "/api/v1/lists/{id}/header-matches", params(("id" = String, Path)),
    request_body(content((HeaderMatchInput = "application/json"), (HeaderMatchInput = "application/x-www-form-urlencoded"))),
    responses((status = 201, description = "Rule appended at the last position", body = HeaderMatchResponse), (status = 400, description = "Invalid rule or one with the same header and pattern exists", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn create(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(input): JsonOrForm<HeaderMatchInput>,
) -> ApiResult<Response> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    let row = HeaderMatchRow {
        header: normalize_header(&input.header),
        pattern: input.pattern,
        chain: chain_from(input.action, input.chain)?,
        tag: optional(input.tag),
    };
    let position =
        s.db.header_matches()
            .append(&id, row.clone(), &audit_context(&auth, addr))
            .await?;
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, link(&s, &id, position))],
        Json(value(&s, &id, position, &row)),
    )
        .into_response())
}

#[utoipa::path(post, path = "/api/v1/lists/{id}/header-matches/find", params(("id" = String, Path), PageQuery),
    request_body(content((HeaderMatchFindInput = "application/json"), (HeaderMatchFindInput = "application/x-www-form-urlencoded")), description = "Fields a rule must match; none matches every rule"),
    responses((status = 200, description = "Matching rules with their positions", body = HeaderMatchPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn find(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(input): JsonOrForm<HeaderMatchFindInput>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    let header = optional(input.header).map(|header| normalize_header(&header));
    let tag = optional(input.tag);
    let chain = chain_from(input.action, input.chain)?;
    if let Some(chain) = &chain
        && !listmngr_db::header_matches::TARGET_CHAINS.contains(&chain.as_str())
    {
        return Err(Error::Validation(
            "action must be one of accept, hold, reject, discard".into(),
        )
        .into());
    }
    let matching: Vec<(usize, HeaderMatchRow)> =
        s.db.header_matches()
            .list(&id)
            .await?
            .into_iter()
            .enumerate()
            .filter(|(_, row)| {
                header.as_ref().is_none_or(|header| &row.header == header)
                    && tag.as_ref().is_none_or(|tag| row.tag.as_ref() == Some(tag))
                    && chain
                        .as_ref()
                        .is_none_or(|chain| row.chain.as_ref() == Some(chain))
            })
            .collect();
    let (start, end) = page_window(matching.len(), &q)?;
    let entries: Vec<Value> = matching[start..end]
        .iter()
        .map(|(position, row)| value(&s, &id, *position, row))
        .collect();
    Ok(Json(page_response(
        s.flavor,
        &entries,
        start,
        matching.len(),
    )))
}

#[utoipa::path(delete, path = "/api/v1/lists/{id}/header-matches", params(("id" = String, Path)),
    responses((status = 204, description = "Every rule removed"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn clear(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    s.db.header_matches()
        .clear(&id, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/v1/lists/{id}/header-matches/{position}", params(("id" = String, Path), ("position" = String, Path, description = "Zero-based position")),
    responses((status = 200, description = "The rule at that position", body = HeaderMatchResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List or rule missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn get(
    State(s): State<AppState>,
    Path((id, position)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    let position = path_position(&position)?;
    let row = s.db.header_matches().get(&id, position).await?;
    Ok(Json(value(&s, &id, position, &row)))
}

#[utoipa::path(patch, path = "/api/v1/lists/{id}/header-matches/{position}", params(("id" = String, Path), ("position" = String, Path, description = "Zero-based position")),
    request_body(content((HeaderMatchPatchInput = "application/json"), (HeaderMatchPatchInput = "application/x-www-form-urlencoded")), description = "Fields to change; `position` moves the rule"),
    responses((status = 204, description = "Rule changed"), (status = 400, description = "Invalid field, duplicate rule or position past the end", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List or rule missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn patch(
    State(s): State<AppState>,
    Path((id, position)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(input): JsonOrForm<HeaderMatchPatchInput>,
) -> ApiResult<StatusCode> {
    edit(&s, &id, &position, &h, c, input, false).await
}

#[utoipa::path(put, path = "/api/v1/lists/{id}/header-matches/{position}", params(("id" = String, Path), ("position" = String, Path, description = "Zero-based position")),
    request_body(content((HeaderMatchPatchInput = "application/json"), (HeaderMatchPatchInput = "application/x-www-form-urlencoded")), description = "The whole rule: header and pattern required, chain and tag reset when absent"),
    responses((status = 204, description = "Rule replaced"), (status = 400, description = "Missing header or pattern, invalid field, duplicate rule or position past the end", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List or rule missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn put(
    State(s): State<AppState>,
    Path((id, position)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(input): JsonOrForm<HeaderMatchPatchInput>,
) -> ApiResult<StatusCode> {
    edit(&s, &id, &position, &h, c, input, true).await
}

async fn edit(
    s: &AppState,
    id: &str,
    position: &str,
    h: &HeaderMap,
    c: ConnectInfo<SocketAddr>,
    input: HeaderMatchPatchInput,
    replace: bool,
) -> ApiResult<StatusCode> {
    let id = parse_list_path(s.flavor, id)?;
    let addr = peer(c);
    let auth = authorize_list(s, h, addr, "lists:write", &id).await?;
    let position = path_position(position)?;
    let patch = patch_from(input, replace)?;
    s.db.header_matches()
        .update(&id, position, patch, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/v1/lists/{id}/header-matches/{position}", params(("id" = String, Path), ("position" = String, Path, description = "Zero-based position")),
    responses((status = 204, description = "Rule removed; later rules move up"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List or rule missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn delete(
    State(s): State<AppState>,
    Path((id, position)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    let position = path_position(&position)?;
    s.db.header_matches()
        .remove(&id, position, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
