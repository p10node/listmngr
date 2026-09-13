//! Serializes member-search navigation without accepting a return URL.
use super::{ApiResult, BrowserPage, ListId, escape};
use serde::Deserialize;
use std::fmt::Write as _;

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
    pub fn hidden(&self) -> String {
        format!(
            "<input type=\"hidden\" name=\"q\" value=\"{}\"><input type=\"hidden\" name=\"page\" value=\"{}\">",
            escape(&self.q),
            self.page
        )
    }
    pub fn links(&self, list: &ListId, more: bool) -> String {
        let mut html = String::from("<nav aria-label=\"Pagination\">");
        let next = if more && self.page < 10_000 {
            Some(self.page + 1)
        } else {
            None
        };
        for (number, label) in [
            (self.page.checked_sub(1), "Previous page"),
            (next, "Next page"),
        ] {
            if let Some(number) = number {
                write!(
                    &mut html,
                    "<a href=\"{}\">{label}</a> ",
                    escape(&self.url(list, number))
                )
                .expect("HTML");
            }
        }
        html.push_str("</nav>");
        html
    }
}
