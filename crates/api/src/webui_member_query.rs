//! Serializes member-search navigation without accepting a return URL.
use super::{ApiResult, BrowserPage, ListId};
use listmngr_web::Pagination;
use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(in crate::webui) struct MemberQuery {
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub q: String,
}
impl MemberQuery {
    pub fn offset(&self) -> ApiResult<i64> {
        if self.q.len() > 320 || self.q.chars().any(char::is_control) {
            return Err(listmngr_core::Error::Validation("invalid member search".into()).into());
        }
        BrowserPage { page: self.page }.offset()
    }
    pub fn url(&self, list: &ListId, page: u32) -> String {
        let base = format!("/web/lists/{list}/members");
        if self.q.is_empty() && page == 0 {
            return base;
        }
        format!(
            "{base}?{}",
            serde_urlencoded::to_string([("q", self.q.clone()), ("page", page.to_string())])
                .expect("string query")
        )
    }
    pub fn pagination(&self, list: &ListId, more: bool) -> Pagination {
        Pagination {
            previous: self.page.checked_sub(1).map(|page| self.url(list, page)),
            next: (more && self.page < BrowserPage::LAST).then(|| self.url(list, self.page + 1)),
        }
    }
}
