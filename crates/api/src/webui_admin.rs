//! The administration index: the lists the reader owns.
use super::{
    ApiResult, AppState, BrowserPage, HeaderMap, Query, Response, Shell, State, html, load,
    privileged, reader_language,
};
use listmngr_web::Nav;

pub(super) async fn index(
    State(s): State<AppState>,
    Query(paging): Query<BrowserPage>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    privileged(&s, &session).await?;
    let rows = s.db.browser_admin_lists(&session, paging.offset()?).await?;
    let more = rows.len() > 20;
    let rows = rows
        .into_iter()
        .take(20)
        .map(|(id, name)| listmngr_web::AdminRow {
            members_href: format!("/web/lists/{id}/members"),
            settings_href: format!("/web/lists/{id}/settings"),
            id,
            name,
        })
        .collect();
    Ok(html(&listmngr_web::AdminIndex {
        shell: Shell::new(
            reader_language(&s, &headers, &session).await?,
            "web-title-admin",
            Nav::Account,
        ),
        rows,
        pagination: paging.pagination("/web/admin", more),
    }))
}
