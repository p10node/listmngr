//! `GET …/archive/export.mbox` and `…/archive/export.mbox.gz`: a whole
//! archive, one thread (`?thread=`) or one month (`?month=YYYY-MM`) as
//! mboxrd, streamed page by page under the archive's policy for the
//! reader, gzipped on the fly for the `.gz` address.
use crate::{ApiResult, AppState};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use listmngr_core::{Error, ListId};
use listmngr_db::archive::browse::month_bounds;
use listmngr_db::archive::import::ExportSelection;
use serde::Deserialize;
use std::io::Write as _;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExportQuery {
    #[serde(default)]
    thread: String,
    #[serde(default)]
    month: String,
}

/// Messages per database page while streaming.
const PAGE: i64 = 200;

/// `YYYY-MM` as the month's bounds.
fn parse_month(value: &str) -> ApiResult<(i64, i64)> {
    let (year, month) = value
        .split_once('-')
        .ok_or_else(|| Error::Validation("archive month".into()))?;
    let year: i32 = year
        .parse()
        .map_err(|_| Error::Validation("archive month".into()))?;
    let month: u32 = month
        .parse()
        .map_err(|_| Error::Validation("archive month".into()))?;
    if !(1970..=9999).contains(&year) {
        return Err(Error::Validation("archive month".into()).into());
    }
    Ok(month_bounds(year, month)?)
}

/// The selection and the file name's middle part.
fn selection(query: &ExportQuery) -> ApiResult<(ExportSelection, String)> {
    if !query.thread.is_empty() && !query.month.is_empty() {
        return Err(Error::Validation("one of thread or month".into()).into());
    }
    if !query.thread.is_empty() {
        if query.thread.len() > 200 {
            return Err(Error::Validation("archive thread bounds".into()).into());
        }
        let safe: String = query
            .thread
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(40)
            .collect();
        return Ok((
            ExportSelection::Thread(query.thread.clone()),
            format!("thread-{safe}"),
        ));
    }
    if !query.month.is_empty() {
        let (from_ms, until_ms) = parse_month(&query.month)?;
        return Ok((
            ExportSelection::Between { from_ms, until_ms },
            query.month.clone(),
        ));
    }
    Ok((ExportSelection::All, "all".to_owned()))
}

pub(super) async fn export_mbox(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(query): Query<ExportQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    export(&s, &id, &query, &headers, false).await
}

pub(super) async fn export_gzip(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(query): Query<ExportQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    export(&s, &id, &query, &headers, true).await
}

async fn export(
    s: &AppState,
    id: &ListId,
    query: &ExportQuery,
    headers: &HeaderMap,
    gzip: bool,
) -> ApiResult<Response> {
    let (selection, label) = selection(query)?;
    let session = super::archive::archive_session(s, headers).await?;
    s.db.archive()
        .browser_export_authorize(id, session.as_ref())
        .await?;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(4);
    let db = s.db.clone();
    let list = id.clone();
    tokio::spawn(async move {
        let mut encoder =
            gzip.then(|| flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()));
        let mut after: Option<(i64, String)> = None;
        loop {
            let rows = match db
                .archive()
                .export_rows(&list, &selection, after.as_ref(), PAGE)
                .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    let _ = tx.send(Err(std::io::Error::other(error.to_string()))).await;
                    return;
                }
            };
            let Some(newest) = rows.last() else {
                break;
            };
            after = Some((newest.created_at, newest.hash.clone()));
            let mut plain = Vec::new();
            for row in &rows {
                // Writing into a Vec cannot fail.
                let _ = listmngr_archive::mbox::write_message(&mut plain, &row.raw);
            }
            let chunk = match encoder.as_mut() {
                Some(encoder) => {
                    let _ = encoder.write_all(&plain);
                    std::mem::take(encoder.get_mut())
                }
                None => plain,
            };
            if !chunk.is_empty() && tx.send(Ok(chunk.into())).await.is_err() {
                return;
            }
        }
        if let Some(encoder) = encoder
            && let Ok(tail) = encoder.finish()
        {
            let _ = tx.send(Ok(tail.into())).await;
        }
    });
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let (content_type, extension) = if gzip {
        ("application/gzip", "mbox.gz")
    } else {
        ("application/mbox", "mbox")
    };
    let filename = format!("{}-{label}.{extension}", id.as_str());
    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}
