#![forbid(unsafe_code)]

//! Axum REST API shared by Mailman-compatible `/3.1` and typed `/api/v1` routes.

mod api_docs;
mod archive;
mod bans;
mod bounce_config;
mod bounces;
mod compat;
mod digest;
mod header_matches;
pub mod oidc;
mod queues;
mod requests;
mod webhooks;

use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{ConnectInfo, FromRequest, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use dashmap::DashMap;
use listmngr_core::{
    Config, DeliveryMode, DeliveryStatus, Error, ListId, MemberId, MemberRole, Preferences,
    SubscriptionMode, UserId, builtin_styles,
};
use listmngr_db::{AuditContext, Database, NewList, NewMember, NewUser, TokenAuth};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use utoipa::OpenApi;
mod templates;
mod unsubscribe;
mod webui;
mod workflows;

#[derive(Debug)]
struct JsonOrForm<T>(T);

impl<S, T> FromRequest<S> for JsonOrForm<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = Response;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let json = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json"));
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let value = if json {
            serde_json::from_slice(&bytes)
        } else {
            serde_urlencoded::from_bytes(&bytes)
                .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))
        }
        .map_err(|_| StatusCode::BAD_REQUEST.into_response())?;
        Ok(Self(value))
    }
}

/// A JSON object whose form encoding may repeat a key. Repeated form keys
/// collect into an array (mailmanclient posts lists as `key=a&key=b`); JSON
/// input is taken as written.
#[derive(Debug)]
struct FormAwareObject(Value);

impl<'de> Deserialize<'de> for FormAwareObject {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = FormAwareObject;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut object = serde_json::Map::new();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    match object.get_mut(&key) {
                        Some(Value::Array(items)) => items.push(value),
                        Some(existing) => {
                            let first = std::mem::take(existing);
                            *existing = Value::Array(vec![first, value]);
                        }
                        None => {
                            object.insert(key, value);
                        }
                    }
                }
                Ok(FormAwareObject(Value::Object(object)))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct ErrorResponse {
    code: &'static str,
    correlation_id: uuid::Uuid,
    title: &'static str,
    detail: &'static str,
}

macro_rules! page_response {
    ($name:ident, $item:ty) => {
        #[derive(Debug, Serialize, utoipa::ToSchema)]
        pub struct $name {
            pub items: Vec<$item>,
            pub next_cursor: Option<String>,
            pub total: usize,
            pub start: usize,
            pub count: usize,
        }
    };
}

page_response!(StringPageResponse, String);
page_response!(BanPageResponse, bans::BanResponse);
page_response!(HeaderMatchPageResponse, header_matches::HeaderMatchResponse);
page_response!(WebhookPageResponse, webhooks::WebhookResponse);
page_response!(PluginPageResponse, PluginResponse);
page_response!(DeliveryPageResponse, webhooks::DeliveryResponse);
page_response!(QueuePageResponse, queues::QueueResponse);
page_response!(RequestPageResponse, requests::RequestResponse);
page_response!(BouncePageResponse, listmngr_db::bounces::BounceEvent);
page_response!(CatalogPageResponse, CatalogEntry);
page_response!(DomainPageResponse, listmngr_core::Domain);
page_response!(MailingListPageResponse, listmngr_core::MailingList);
page_response!(UserPageResponse, listmngr_core::User);
page_response!(ArchiverPageResponse, ArchiverResponse);
page_response!(TemplatePageResponse, listmngr_db::Template);
page_response!(MemberPageResponse, listmngr_core::Member);
page_response!(AddressPageResponse, listmngr_core::Address);

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CatalogEntry {
    pub name: String,
    pub phase: String,
    pub executable: bool,
    pub status: String,
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams, utoipa::ToSchema)]
#[into_params(parameter_in = Query)]
pub struct PageQuery {
    /// Opaque offset cursor returned by the previous native page.
    pub cursor: Option<String>,
    /// One-based compatibility page number. Cannot be combined with cursor.
    pub page: Option<usize>,
    /// Maximum entries to return (1..=100, default 50).
    pub count: Option<usize>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SystemVersionsResponse {
    pub listmngr_version: String,
    pub api_version: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ConfigurationResponse {
    pub sections: std::collections::HashMap<String, String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UriResponse {
    pub self_link: Option<String>,
    pub posting_address: Option<String>,
    pub bounces_address: Option<String>,
    pub join_address: Option<String>,
    pub leave_address: Option<String>,
    pub owner_address: Option<String>,
    pub request_address: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
// Mirrors independent persisted configuration switches on MailingList.
#[allow(clippy::struct_excessive_bools)]
pub struct ListConfigResponse {
    pub default_member_action: Option<listmngr_core::ModerationAction>,
    pub default_nonmember_action: Option<listmngr_core::ModerationAction>,
    pub display_name: String,
    pub description: String,
    pub info: String,
    pub subject_prefix: String,
    pub advertised: bool,
    pub preferred_language: String,
    pub anonymous_list: bool,
    pub send_welcome_message: bool,
    pub send_goodbye_message: bool,
    pub process_bounces: bool,
    #[schema(default = true)]
    pub bounce_notify_owner_on_disable: bool,
    #[schema(default = false)]
    pub bounce_notify_owner_on_bounce_increment: bool,
    #[schema(default = true)]
    pub bounce_notify_owner_on_removal: bool,
    #[schema(minimum = 0, maximum = 100, default = 3)]
    pub bounce_you_are_disabled_warnings: u32,
    #[schema(minimum = 0, maximum = 36500, default = 7)]
    pub bounce_you_are_disabled_warnings_interval: u32,
    #[schema(minimum = 1, maximum = 3650, default = 7)]
    pub bounce_info_stale_after: u32,
    #[schema(exclusive_minimum = 0, maximum = 1_000_000, default = 5)]
    pub bounce_score_threshold: f64,
    #[serde(flatten)]
    pub dmarc: listmngr_core::DmarcSettings,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_post_at: Option<chrono::DateTime<chrono::Utc>>,
    pub post_id: i64,
    pub volume: i32,
    pub next_digest_number: i64,
    pub digest_last_sent_at: Option<chrono::DateTime<chrono::Utc>>,
    pub emergency: bool,
    /// Original post size limit in KiB; zero disables the per-list limit.
    pub max_message_size: u32,
    /// Hold at or above this visible To/Cc mailbox count; zero disables.
    #[schema(minimum = 0, maximum = 2_147_483_647)]
    pub max_num_recipients: u32,
    pub archive_policy: listmngr_core::ArchivePolicy,
    pub archive_rendering_mode: listmngr_core::ArchiveRenderingMode,
    pub style_name: String,
    /// Hold short posts that look like email commands.
    #[schema(default = true)]
    pub administrivia: bool,
    /// Hold posts whose visible To/Cc names neither the list nor an alias.
    #[schema(default = true)]
    pub require_explicit_destination: bool,
    /// Exact addresses or `^`-anchored regexes counted as explicit destinations.
    pub acceptable_aliases: Vec<String>,
    /// Legacy nonmember action lists; exact addresses or `^`-anchored regexes.
    pub accept_these_nonmembers: Vec<String>,
    pub hold_these_nonmembers: Vec<String>,
    pub reject_these_nonmembers: Vec<String>,
    pub discard_these_nonmembers: Vec<String>,
    /// Name of the handler pipeline an accepted post runs.
    #[schema(default = "default-posting-pipeline")]
    pub posting_pipeline: String,
    /// Tell the poster when their post is held for moderation.
    #[schema(default = true)]
    pub respond_to_post_requests: bool,
    /// Tell owners and moderators immediately when a post is held.
    #[schema(default = true)]
    pub admin_immed_notify: bool,
    /// Tell owners and moderators when a member subscribes or unsubscribes.
    #[schema(default = false)]
    pub admin_notify_mchanges: bool,
    #[serde(flatten)]
    pub alter_messages: listmngr_core::AlterMessages,
    #[serde(flatten)]
    pub member_policy: listmngr_core::MemberPolicy,
    #[serde(flatten)]
    pub automatic_responses: listmngr_core::AutomaticResponses,
    /// Where bounces that match no member are forwarded.
    #[schema(default = "administrators")]
    pub forward_unrecognized_bounces_to: listmngr_core::UnrecognizedBounceDisposition,
    /// Run the topic matcher and add `X-Topics` to matching posts.
    pub topics_enabled: bool,
    /// Leading header-like body lines scanned; negative means all, zero none.
    #[schema(default = 5, minimum = -1, maximum = 10_000)]
    pub topics_bodylines_limit: i32,
    /// listmngr extension (Mailman keeps topics out of its REST API).
    pub topics: Vec<listmngr_core::Topic>,
    pub mail_host: String,
    pub list_name: String,
    pub fqdn_listname: String,
    pub list_id: ListId,
    pub posting_address: String,
    pub bounces_address: String,
    pub join_address: String,
    pub leave_address: String,
    pub owner_address: String,
    pub request_address: String,
    pub no_reply_address: String,
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ListConfigInput {
    pub default_member_action: Option<listmngr_core::ModerationAction>,
    pub default_nonmember_action: Option<listmngr_core::ModerationAction>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub info: Option<String>,
    pub subject_prefix: Option<String>,
    pub advertised: Option<bool>,
    pub preferred_language: Option<String>,
    pub anonymous_list: Option<bool>,
    /// Opt-in built-in private welcome for new Member subscriptions; default false.
    pub send_welcome_message: Option<bool>,
    /// Opt-in built-in private goodbye for actual Member removal; default false.
    pub send_goodbye_message: Option<bool>,
    pub process_bounces: Option<bool>,
    #[schema(default = true)]
    pub bounce_notify_owner_on_disable: Option<bool>,
    #[schema(default = false)]
    pub bounce_notify_owner_on_bounce_increment: Option<bool>,
    #[schema(default = true)]
    pub bounce_notify_owner_on_removal: Option<bool>,
    #[schema(minimum = 0, maximum = 100, default = 3)]
    pub bounce_you_are_disabled_warnings: Option<u32>,
    #[schema(minimum = 0, maximum = 36500, default = 7)]
    pub bounce_you_are_disabled_warnings_interval: Option<u32>,
    #[schema(minimum = 1, maximum = 3650, default = 7)]
    pub bounce_info_stale_after: Option<u32>,
    #[schema(exclusive_minimum = 0, maximum = 1_000_000, default = 5)]
    pub bounce_score_threshold: Option<f64>,
    /// `munge_from` requires unconditional=true; conditional DNS is unsupported.
    pub dmarc_mitigate_action: Option<listmngr_core::DmarcMitigateAction>,
    pub dmarc_mitigate_unconditionally: Option<bool>,
    pub next_digest_number: Option<i64>,
    pub emergency: Option<bool>,
    /// Nonnegative KiB, at most 2147483647; zero disables the per-list limit.
    pub max_message_size: Option<u32>,
    /// Hold at or above this visible To/Cc mailbox count; zero disables.
    #[schema(minimum = 0, maximum = 2_147_483_647)]
    pub max_num_recipients: Option<u32>,
    pub archive_policy: Option<listmngr_core::ArchivePolicy>,
    pub archive_rendering_mode: Option<listmngr_core::ArchiveRenderingMode>,
    /// Hold short posts that look like email commands; default true.
    pub administrivia: Option<bool>,
    /// Hold posts whose visible To/Cc names neither the list nor an alias; default true.
    pub require_explicit_destination: Option<bool>,
    /// Exact addresses or `^`-anchored regexes counted as explicit destinations.
    pub acceptable_aliases: Option<Vec<String>>,
    /// Legacy nonmember action lists; exact addresses or `^`-anchored regexes.
    pub accept_these_nonmembers: Option<Vec<String>>,
    pub hold_these_nonmembers: Option<Vec<String>>,
    pub reject_these_nonmembers: Option<Vec<String>>,
    pub discard_these_nonmembers: Option<Vec<String>>,
    /// Write-only `Approved:` posting key, stored as Argon2id; empty clears it.
    #[schema(write_only)]
    pub moderator_password: Option<String>,
    /// Name of a registered handler pipeline; see `/system/pipelines`.
    pub posting_pipeline: Option<String>,
    /// Tell the poster when their post is held; default true.
    pub respond_to_post_requests: Option<bool>,
    /// Tell owners and moderators immediately when a post is held; default true.
    pub admin_immed_notify: Option<bool>,
    /// Tell owners and moderators when a member subscribes or unsubscribes; default false.
    pub admin_notify_mchanges: Option<bool>,
    pub filter_content: Option<bool>,
    /// MIME types (`type` or `type/subtype`) removed by content filtering.
    pub filter_types: Option<Vec<String>>,
    /// MIME types kept by content filtering; empty keeps everything not filtered.
    pub pass_types: Option<Vec<String>>,
    /// File-name extensions removed by content filtering.
    pub filter_extensions: Option<Vec<String>>,
    /// File-name extensions kept by content filtering.
    pub pass_extensions: Option<Vec<String>>,
    #[schema(default = true)]
    pub collapse_alternatives: Option<bool>,
    pub convert_html_to_plaintext: Option<bool>,
    #[schema(default = "discard")]
    pub filter_action: Option<listmngr_core::FilterAction>,
    #[schema(default = true)]
    pub include_rfc2369_headers: Option<bool>,
    #[schema(default = true)]
    pub allow_list_posts: Option<bool>,
    #[schema(default = "no_munging")]
    pub reply_goes_to_list: Option<listmngr_core::ReplyToMunging>,
    /// Mailbox for the explicit `Reply-To` policies; empty clears it.
    pub reply_to_address: Option<String>,
    pub first_strip_reply_to: Option<bool>,
    #[schema(default = "none")]
    pub personalize: Option<listmngr_core::Personalization>,
    #[schema(default = true)]
    pub include_sender_header: Option<bool>,
    #[schema(default = "confirm")]
    pub subscription_policy: Option<listmngr_core::SubscriptionPolicy>,
    #[schema(default = "confirm")]
    pub unsubscription_policy: Option<listmngr_core::SubscriptionPolicy>,
    #[schema(default = "moderators")]
    pub member_roster_visibility: Option<listmngr_core::RosterVisibility>,
    /// Exact addresses or `^`-anchored regexes treated as DMARC-protected.
    pub dmarc_addresses: Option<Vec<String>>,
    /// Text added to the hold notice for DMARC holds; at most 64 KiB.
    pub dmarc_moderation_notice: Option<String>,
    /// Outer text of a DMARC-wrapped post; at most 64 KiB.
    pub dmarc_wrapped_message_text: Option<String>,
    #[schema(default = "administrators")]
    pub forward_unrecognized_bounces_to: Option<listmngr_core::UnrecognizedBounceDisposition>,
    /// Mailman's Usenet gateway: gate posts to the linked newsgroup.
    #[schema(default = false)]
    pub gateway_to_news: Option<bool>,
    /// Gate the linked newsgroup's articles to the list.
    #[schema(default = false)]
    pub gateway_to_mail: Option<bool>,
    /// The newsgroup the list gateways with; empty for none.
    #[schema(default = "")]
    pub linked_newsgroup: Option<String>,
    /// Keep the subject prefix on posts gated to the newsgroup.
    #[schema(default = true)]
    pub nntp_prefix_subject_too: Option<bool>,
    #[schema(default = "none")]
    pub newsgroup_moderation: Option<listmngr_core::NewsgroupModeration>,
    #[schema(default = true)]
    pub digests_enabled: Option<bool>,
    /// KiB of pending posts that trigger an issue; 0 never does.
    #[schema(default = 30.0, minimum = 0.0)]
    pub digest_size_threshold: Option<f64>,
    #[schema(default = true)]
    pub digest_send_periodic: Option<bool>,
    #[schema(default = "monthly")]
    pub digest_volume_frequency: Option<listmngr_core::DigestFrequency>,
    #[schema(default = "none")]
    pub autorespond_owner: Option<listmngr_core::ResponseAction>,
    /// Reply body for mail to the owner address; empty uses the built-in text.
    pub autoresponse_owner_text: Option<String>,
    #[schema(default = "none")]
    pub autorespond_postings: Option<listmngr_core::ResponseAction>,
    pub autoresponse_postings_text: Option<String>,
    #[schema(default = "none")]
    pub autorespond_requests: Option<listmngr_core::ResponseAction>,
    pub autoresponse_request_text: Option<String>,
    /// Days before the same writer is answered again; 0 answers every message.
    #[schema(default = 90, minimum = 0, maximum = 3650)]
    pub autoresponse_grace_period: Option<i32>,
    pub topics_enabled: Option<bool>,
    #[schema(default = 5, minimum = -1, maximum = 10_000)]
    pub topics_bodylines_limit: Option<i32>,
    /// JSON only: a list of `{name, pattern, description}` (listmngr extension).
    pub topics: Option<Vec<listmngr_core::Topic>>,
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberPatchInput {
    pub display_name: Option<String>,
    pub delivery_mode: Option<DeliveryMode>,
    pub delivery_status: Option<DeliveryStatus>,
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UserPatchInput {
    pub display_name: Option<String>,
    pub locale: Option<String>,
    pub timezone: Option<String>,
    pub is_server_owner: Option<bool>,
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyMutationInput {}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ProcessedResponse {
    pub processed: usize,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct LoginResponse {
    pub success: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AddressUserResponse {
    pub user_id: Option<UserId>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub enum ListConfigAttributeValue {
    Text(String),
    Boolean(bool),
    Integer(i64),
    Number(f64),
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ArchiverResponse {
    pub name: String,
    pub enabled: bool,
}

struct SecurityAddon;
impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::header::Header;
        use utoipa::openapi::path::Operation;
        use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
        use utoipa::openapi::{Object, RefOr, Type};

        fn add_retry_after(operation: &mut Option<Operation>) {
            let Some(operation) = operation else {
                return;
            };
            let Some(RefOr::T(response)) = operation.responses.responses.get_mut("429") else {
                return;
            };
            let mut seconds = Object::with_type(Type::Integer);
            seconds.minimum = Some(1.into());
            response
                .headers
                .insert("Retry-After".into(), Header::new(seconds));
        }

        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearerAuth",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("lm_<uuid>_<secret>")
                        .build(),
                ),
            );
        }

        for item in openapi.paths.paths.values_mut() {
            add_retry_after(&mut item.get);
            add_retry_after(&mut item.post);
            add_retry_after(&mut item.put);
            add_retry_after(&mut item.patch);
            add_retry_after(&mut item.delete);
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    info(title = "listmngr API", version = "0.1.0"),
    paths(
        bans::list,
        bounces::list,
        bans::get,
        bans::create,
        bans::delete,
        bans::site_list,
        bans::site_get,
        bans::site_create,
        bans::site_delete,
        digest::get,
        digest::post,
        webhooks::list,
        webhooks::create,
        webhooks::get,
        webhooks::patch,
        webhooks::delete,
        webhooks::rotate,
        webhooks::ping,
        webhooks::deliveries,
        header_matches::list,
        header_matches::create,
        header_matches::find,
        header_matches::clear,
        header_matches::get,
        header_matches::patch,
        header_matches::put,
        header_matches::delete,
        queues::list,
        queues::get,
        queues::job,
        queues::inject,
        requests::list,
        requests::count,
        requests::get,
        requests::decide,
        system_versions,
        system_config,
        system_config_section,
        system_preferences,
        system_pipelines,
        system_chains,
        plugins,
        domains_list,
        domains_create,
        domains_get,
        domains_delete,
        domain_lists,
        domain_owners,
        lists_list,
        lists_create,
        styles,
        lists_get,
        lists_delete,
        list_config,
        list_config_put,
        list_config_patch,
        list_config_attr,
        list_config_attr_put,
        list_config_attr_patch,
        list_archivers,
        templates::list_uris,
        templates::patch_list_uris,
        templates::put_list_uris,
        templates::delete_list_uris,
        templates::get_list_uri,
        templates::set_list_uri,
        templates::delete_list_uri,
        templates::domain_uris,
        templates::patch_domain_uris,
        templates::put_domain_uris,
        templates::delete_domain_uris,
        templates::get_domain_uri,
        templates::set_domain_uri,
        templates::delete_domain_uri,
        templates::site_uris,
        templates::patch_site_uris,
        templates::put_site_uris,
        templates::delete_site_uris,
        templates::get_site_uri,
        templates::set_site_uri,
        templates::delete_site_uri,
        templates::put_list_template_body,
        templates::delete_list_template_body,
        list_templates,
        roster,
        list_member,
        list_member_delete,
        list_held,
        list_held_count,
        list_held_get,
        list_held_moderate,
        roster_mass_unsubscribe,
        lists_find,
        list_archivers_set,
        list_owner,
        list_owner_delete,
        list_moderator,
        list_moderator_delete,
        list_nonmember,
        list_nonmember_delete,
        user_preferred_address,
        user_preferred_address_set,
        user_preferred_address_unset,
        address_delete,
        members_list,
        members_create,
        members_mass,
        members_find,
        members_get,
        member_patch,
        members_delete,
        member_preferences,
        member_preferences_put,
        member_preferences_patch,
        member_all_preferences,
        users_list,
        users_create,
        users_get,
        users_patch,
        users_delete,
        user_addresses,
        user_address_link,
        user_preferences,
        user_preferences_put,
        user_preferences_patch,
        user_all_preferences,
        user_login,
        address_get,
        address_verify,
        address_unverify,
        address_user,
        address_link,
        address_unlink,
        address_memberships,
        address_preferences,
        address_preferences_put,
        address_preferences_patch,
        address_all_preferences,
        owners
    ),
    components(schemas(
        ErrorResponse, StringPageResponse, CatalogPageResponse, CatalogEntry, PageQuery,
        RequestPageResponse, requests::RequestResponse, requests::DecisionInput,
        digest::DigestResponse, digest::DigestActionInput, digest::DigestActionResponse,
        QueuePageResponse, queues::QueueResponse, queues::JobResponse, queues::InjectInput,
        HeaderMatchPageResponse, header_matches::HeaderMatchResponse, header_matches::HeaderMatchInput, header_matches::HeaderMatchFindInput, header_matches::HeaderMatchPatchInput,
        PluginPageResponse, PluginResponse,
        WebhookPageResponse, DeliveryPageResponse, webhooks::WebhookResponse, webhooks::WebhookInput, webhooks::WebhookPatchInput, webhooks::DeliveryResponse,
        DomainPageResponse, MailingListPageResponse,
        UserPageResponse, ArchiverPageResponse, TemplatePageResponse, MemberPageResponse,
        AddressPageResponse, SystemVersionsResponse, ConfigurationResponse, UriResponse,
        ListConfigResponse, ListConfigInput, ListConfigAttributeValue, MemberPatchInput,
        UserPatchInput, EmptyMutationInput, ProcessedResponse, LoginResponse, AddressUserResponse,
        ArchiverResponse, templates::TemplateUriResponse, templates::TemplateUriPageResponse, templates::TemplateBodyInput, templates::TemplateUrisInput, templates::TemplateUriInput,
        DomainInput, ListQuery, ListInput, MemberInput, ConfirmationInput, WorkflowInput,
        MassMemberRow, MassMemberInput, FindInput, LoginInput, LinkInput, UserLink,
        Preferences, listmngr_core::Domain, listmngr_core::MailingList, listmngr_core::Member,
        listmngr_core::User, listmngr_core::Address, listmngr_db::NewUser,
        listmngr_db::NewList, listmngr_db::NewMember, listmngr_db::MemberMassResult,
        listmngr_db::Template,
        HeldMessageResponse, HeldMessagePageResponse, CountResponse, ModerateInput
    )),
    modifiers(&SecurityAddon)
)]
struct ApiDoc;

#[derive(Debug, Clone)]
pub struct AppState {
    db: Database,
    config: Config,
    pre_auth_rate: Arc<RateLimiter>,
    post_auth_rate: Arc<RateLimiter>,
    web_login_rate: Arc<RateLimiter>,
    flavor: ApiFlavor,
    /// `[[web.oidc]]`: the identity providers the login page offers.
    oidc: Arc<oidc::Providers>,
    /// `[mta] incoming`: the map writer regenerated after list changes.
    mta_maps: Option<Arc<listmngr_mail::mta::MapWriter>>,
    /// The queue-depth part of `/metrics`, refreshed at most every few
    /// seconds so an unauthenticated scrape cannot hammer the database.
    queue_metrics: Arc<std::sync::Mutex<Option<(Instant, String)>>>,
    /// `[archive] gravatar`: avatars fetched through this server, cached
    /// for an hour by the sender's hash.
    avatars: Arc<DashMap<String, (Instant, String, bytes::Bytes)>>,
    /// The archive's search index, opened on first use once it exists;
    /// the archive page searches the database until then.
    search: Arc<std::sync::Mutex<Option<Arc<listmngr_archive::search::SearchIndex>>>>,
}

impl AppState {
    /// The search index when `[archive]` is on and the runner or
    /// `listmngr archive reindex` has created it.
    fn search_index(&self) -> Option<Arc<listmngr_archive::search::SearchIndex>> {
        if !self.config.archive.enabled {
            return None;
        }
        let mut slot = self.search.lock().ok()?;
        if let Some(index) = slot.as_ref() {
            let index = index.clone();
            drop(slot);
            return Some(index);
        }
        let path = std::path::Path::new(&self.config.archive.index_path);
        if !listmngr_archive::search::SearchIndex::exists(path) {
            drop(slot);
            return None;
        }
        let opened = listmngr_archive::search::SearchIndex::open(path);
        let index = match opened {
            Ok(index) => Arc::new(index),
            Err(error) => {
                drop(slot);
                tracing::warn!(%error, "search index unavailable; searching the database");
                return None;
            }
        };
        *slot = Some(index.clone());
        drop(slot);
        Some(index)
    }
}

const QUEUE_METRICS_TTL: Duration = Duration::from_secs(5);

/// Queue depth as Prometheus gauges, from the same query `queue stats` uses.
async fn queue_metrics(s: &AppState) -> String {
    use std::fmt::Write as _;
    if let Some((at, text)) = s.queue_metrics.lock().expect("metrics cache").as_ref()
        && at.elapsed() < QUEUE_METRICS_TTL
    {
        return text.clone();
    }
    let stats = match s
        .db
        .mail_queue()
        .stats(chrono::Utc::now().timestamp_millis())
        .await
    {
        Ok(stats) => stats,
        Err(error) => {
            tracing::warn!(%error, "queue metrics unavailable");
            return String::new();
        }
    };
    let mut text = String::from(
        "# HELP listmngr_queue_jobs Jobs per queue and state.\n# TYPE listmngr_queue_jobs gauge\n",
    );
    for (queue, states) in &stats.queues {
        for (state, jobs) in states {
            let _ = writeln!(
                text,
                "listmngr_queue_jobs{{queue=\"{queue}\",state=\"{state}\"}} {jobs}"
            );
        }
    }
    let _ = write!(
        text,
        "# HELP listmngr_queue_shunted_jobs Jobs parked in the shunt queue.\n# TYPE listmngr_queue_shunted_jobs gauge\nlistmngr_queue_shunted_jobs {}\n# HELP listmngr_queue_oldest_ready_age_seconds Seconds the oldest ready job has waited past its due time.\n# TYPE listmngr_queue_oldest_ready_age_seconds gauge\nlistmngr_queue_oldest_ready_age_seconds {}\n",
        stats.shunted,
        stats.oldest_ready_age_secs.unwrap_or(0)
    );
    *s.queue_metrics.lock().expect("metrics cache") = Some((Instant::now(), text.clone()));
    text
}

/// Mailman regenerates the MTA's maps when a list is created or removed.
/// The list change is already committed; a failed publish is logged and
/// `listmngr aliases regen` repairs it.
async fn refresh_mta_maps(s: &AppState) {
    let Some(writer) = s.mta_maps.clone() else {
        return;
    };
    match publish_maps(writer, &s.db).await {
        Ok(generation) => {
            tracing::info!(generation = %generation.display(), "MTA maps regenerated");
        }
        Err(error) => {
            tracing::warn!(%error, "MTA maps not regenerated; run `listmngr aliases regen`");
        }
    }
}

async fn publish_maps(
    writer: Arc<listmngr_mail::mta::MapWriter>,
    db: &Database,
) -> Result<std::path::PathBuf, String> {
    let lists: Vec<listmngr_core::ListId> = db
        .lists()
        .list(None)
        .await
        .map_err(|error| format!("list read failed: {error}"))?
        .into_iter()
        .map(|list| list.id)
        .collect();
    tokio::task::spawn_blocking(move || writer.publish(&lists))
        .await
        .map_err(|error| format!("publish task failed: {error}"))?
        .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Copy)]
enum ApiFlavor {
    Compat31,
    V1,
}

#[derive(Debug)]
struct RateLimiter {
    limit: u32,
    window: Duration,
    hits: DashMap<String, (Instant, u32)>,
}
impl RateLimiter {
    fn from_config(spec: &str) -> Self {
        let (limit, unit) = spec.split_once('/').unwrap_or_else(|| {
            panic!("invalid security.rate_limit.api `{spec}`: expected COUNT/WINDOW")
        });
        let limit = limit
            .parse::<u32>()
            .ok()
            .filter(|limit| *limit > 0)
            .unwrap_or_else(|| {
                panic!("invalid security.rate_limit.api `{spec}`: count must be positive")
            });
        let window = match unit {
            "s" | "sec" | "second" => Duration::from_secs(1),
            "m" | "min" | "minute" => Duration::from_secs(60),
            "h" | "hour" => Duration::from_secs(60 * 60),
            "d" | "day" => Duration::from_secs(24 * 60 * 60),
            _ => panic!("invalid security.rate_limit.api `{spec}`: unsupported window"),
        };
        Self {
            limit,
            window,
            hits: DashMap::new(),
        }
    }

    fn check(&self, key: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut hit = self.hits.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(hit.0) >= self.window {
            *hit = (now, 0);
        }
        hit.1 += 1;
        if hit.1 <= self.limit {
            Ok(())
        } else {
            Err(self
                .window
                .saturating_sub(now.duration_since(hit.0))
                .as_secs()
                .max(1))
        }
    }
}

#[derive(Debug)]
struct ApiError(Error);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let retry_after = match &self.0 {
            Error::RateLimited { retry_after } => Some(*retry_after),
            _ => None,
        };
        let status = match &self.0 {
            Error::Authentication => StatusCode::UNAUTHORIZED,
            Error::Forbidden(_) => StatusCode::FORBIDDEN,
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Error::Validation(_) | Error::InvalidListId(_) => StatusCode::BAD_REQUEST,
            Error::Config(_) | Error::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let code = match &self.0 {
            Error::Authentication => "authentication",
            Error::Forbidden(_) => "forbidden",
            Error::NotFound(_) => "not_found",
            Error::Conflict(_) => "conflict",
            Error::RateLimited { .. } => "rate_limited",
            Error::Validation(_) | Error::InvalidListId(_) => "validation",
            Error::Config(_) | Error::Database(_) => "internal",
        };
        let correlation_id = uuid::Uuid::now_v7();
        let title = status.canonical_reason().unwrap_or("request failed");
        let detail = if status.is_server_error() {
            // The client gets the id alone; the log carries the cause an
            // administrator will be asked about (an sqlx or configuration
            // message, never a credential).
            tracing::error!(%correlation_id, error = %self.0, "request failed");
            "request failed; quote the correlation_id to an administrator"
        } else {
            title
        };
        let mut response = (
            status,
            Json(ErrorResponse {
                code,
                correlation_id,
                title,
                detail,
            }),
        )
            .into_response();
        if let Some(retry_after) = retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, retry_after.into());
        }
        response
    }
}
impl From<Error> for ApiError {
    fn from(value: Error) -> Self {
        Self(value)
    }
}
type ApiResult<T> = Result<T, ApiError>;

pub fn router(db: Database, config: Config) -> Router {
    // `[site]` is one source of truth for the browser surface and the mail
    // the site sends: the public origin (passkeys, archive links, mailed
    // URLs) and the site's name and owner address.
    let db = db
        .with_base_url(&config.site.base_url)
        .with_webhooks(config.webhooks.signing_key(), config.webhooks.allow_http)
        .with_mail_archive_address(&config.archive.archivers.mail_archive_address)
        .with_site(&config.site.name, &config.site.site_owner);
    let pre_auth_rate = RateLimiter::from_config(
        config
            .security
            .rate_limit
            .api_pre_auth
            .as_deref()
            .unwrap_or(&config.security.rate_limit.api),
    );
    let post_auth_rate = RateLimiter::from_config(&config.security.rate_limit.api);
    // `security.rate_limit.login`: one bucket for every password check the
    // browser surface performs, so an attacker cannot buy Argon2 time.
    let web_login_rate = RateLimiter::from_config(&config.security.rate_limit.login);
    // `Config::load` validated `[mta]`; a bad value here can only come from
    // a caller-built config and must not take the API down.
    let mta_maps = listmngr_mail::mta::MapWriter::from_config(&config.mta)
        .unwrap_or_else(|error| {
            tracing::error!(%error, "MTA map generation disabled");
            None
        })
        .map(Arc::new);
    let oidc = Arc::new(oidc::Providers::from_config(&config.web.oidc));
    let state = AppState {
        db,
        config,
        pre_auth_rate: Arc::new(pre_auth_rate),
        post_auth_rate: Arc::new(post_auth_rate),
        web_login_rate: Arc::new(web_login_rate),
        flavor: ApiFlavor::V1,
        oidc,
        mta_maps,
        queue_metrics: Arc::new(std::sync::Mutex::new(None)),
        avatars: Arc::new(DashMap::new()),
        search: Arc::new(std::sync::Mutex::new(None)),
    };
    let mut compat_state = state.clone();
    compat_state.flavor = ApiFlavor::Compat31;
    Router::new()
        .merge(archive::routes())
        .route("/healthz", get(health))
        .merge(workflows::routes())
        .route("/readyz", get(ready))
        .route("/metrics", get(metrics))
        .route("/openapi.json", get(openapi))
        .route("/api/docs", get(api_documentation))
        .nest("/3.1", phase_one_routes().with_state(compat_state))
        .nest(
            "/api/v1",
            phase_one_routes()
                .layer(middleware::from_fn(typed_etag))
                .with_state(state.clone()),
        )
        .merge(webui::routes())
        .merge(unsubscribe::routes())
        .merge(compat::routes())
        .layer(middleware::from_fn(trace_request))
        .with_state(state)
}

async fn typed_etag(request: Request, next: Next) -> Response {
    let is_get = request.method() == Method::GET;
    let response = next.run(request).await;
    if !is_get || !response.status().is_success() || response.headers().contains_key(header::ETAG) {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, usize::MAX).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let digest = base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(&bytes));
    let value = HeaderValue::from_str(&format!("\"sha256-{digest}\""))
        .expect("base64 SHA-256 is a valid entity tag");
    parts.headers.insert(header::ETAG, value);
    Response::from_parts(parts, Body::from(bytes))
}

async fn trace_request(request: Request, next: Next) -> Response {
    let request_id = uuid::Uuid::now_v7();
    let method = request.method().clone();
    // Query strings can contain browser confirmation credentials.
    let uri = request.uri().path().to_owned();
    tracing::info!(%request_id, %method, %uri, "HTTP request started");
    let mut response = next.run(request).await;
    tracing::info!(%request_id, %method, %uri, status = %response.status(), "HTTP request completed");
    response.headers_mut().insert(
        header::HeaderName::from_static("x-request-id"),
        request_id
            .to_string()
            .parse()
            .expect("UUID is a header value"),
    );
    response
}

fn phase_one_routes() -> Router<AppState> {
    Router::new()
        .merge(templates::routes())
        .route("/system/versions", get(system_versions))
        .route("/system/configuration", get(system_config))
        .route(
            "/system/configuration/{section}",
            get(system_config_section),
        )
        .route("/system/preferences", get(system_preferences))
        .route("/system/pipelines", get(system_pipelines))
        .route("/system/chains", get(system_chains))
        .route("/plugins", get(plugins))
        .route("/domains", get(domains_list).post(domains_create))
        .route("/domains/{host}", get(domains_get).delete(domains_delete))
        .route("/domains/{host}/lists", get(domain_lists))
        .route("/domains/{host}/owners", get(domain_owners))
        .route("/lists", get(lists_list).post(lists_create))
        .route("/lists/styles", get(styles))
        .route("/lists/find", post(lists_find))
        .route("/lists/{id}", get(lists_get).delete(lists_delete))
        .route(
            "/lists/{id}/config",
            get(list_config)
                .put(list_config_put)
                .patch(list_config_patch),
        )
        .route(
            "/lists/{id}/config/{attr}",
            get(list_config_attr)
                .put(list_config_attr_put)
                .patch(list_config_attr_patch),
        )
        .route(
            "/lists/{id}/archivers",
            get(list_archivers).patch(list_archivers_set),
        )
        .merge(bans::routes())
        .merge(webhooks::routes())
        .route("/lists/{id}/bounces", get(bounces::list))
        .route("/lists/{id}/templates", get(list_templates))
        .route(
            "/lists/{id}/roster/{role}",
            get(roster).delete(roster_mass_unsubscribe),
        )
        .route(
            "/lists/{id}/owner/{email}",
            get(list_owner).delete(list_owner_delete),
        )
        .route(
            "/lists/{id}/moderator/{email}",
            get(list_moderator).delete(list_moderator_delete),
        )
        .route(
            "/lists/{id}/nonmember/{email}",
            get(list_nonmember).delete(list_nonmember_delete),
        )
        .route(
            "/lists/{id}/member/{email}",
            get(list_member).delete(list_member_delete),
        )
        .merge(requests::routes())
        .merge(header_matches::routes())
        .merge(digest::routes())
        .merge(queues::routes())
        .route("/lists/{id}/held", get(list_held))
        .route("/lists/{id}/held/count", get(list_held_count))
        .route(
            "/lists/{id}/held/{held_id}",
            get(list_held_get).post(list_held_moderate),
        )
        .merge(people_routes())
}

/// The membership, user and address routes of the compatibility surface.
fn people_routes() -> Router<AppState> {
    Router::new()
        .route("/members", get(members_list).post(members_create))
        .route("/members/mass", post(members_mass))
        .route("/members/find", post(members_find))
        .route(
            "/members/{id}",
            get(members_get).patch(member_patch).delete(members_delete),
        )
        .route(
            "/members/{id}/preferences",
            get(member_preferences)
                .put(member_preferences_put)
                .patch(member_preferences_patch),
        )
        .route("/members/{id}/all/preferences", get(member_all_preferences))
        .route("/users", get(users_list).post(users_create))
        .route(
            "/users/{id}",
            get(users_get).patch(users_patch).delete(users_delete),
        )
        .route(
            "/users/{id}/addresses",
            get(user_addresses).post(user_address_link),
        )
        .route(
            "/users/{id}/preferences",
            get(user_preferences)
                .put(user_preferences_put)
                .patch(user_preferences_patch),
        )
        .route("/users/{id}/all/preferences", get(user_all_preferences))
        .route(
            "/users/{id}/preferred_address",
            get(user_preferred_address)
                .post(user_preferred_address_set)
                .delete(user_preferred_address_unset),
        )
        .route("/users/{id}/login", post(user_login))
        .route(
            "/addresses/{email}",
            get(address_get).delete(address_delete),
        )
        .route("/addresses/{email}/verify", post(address_verify))
        .route("/addresses/{email}/unverify", post(address_unverify))
        .route(
            "/addresses/{email}/user",
            get(address_user).post(address_link).delete(address_unlink),
        )
        .route("/addresses/{email}/memberships", get(address_memberships))
        .route(
            "/addresses/{email}/preferences",
            get(address_preferences)
                .put(address_preferences_put)
                .patch(address_preferences_patch),
        )
        .route(
            "/addresses/{email}/all/preferences",
            get(address_all_preferences),
        )
        .route("/owners", get(owners))
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status":"ok"})))
}
async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query("SELECT 1").execute(state.db.pool()).await {
        Ok(_) => (StatusCode::OK, Json(json!({"status":"ready"}))),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status":"not-ready"})),
        ),
    }
}
async fn metrics(State(s): State<AppState>) -> impl IntoResponse {
    let mut text = String::from(
        "# HELP listmngr_up Service readiness.\n# TYPE listmngr_up gauge\nlistmngr_up 1\n",
    );
    text.push_str(&listmngr_core::metrics::global().render());
    text.push_str(&queue_metrics(&s).await);
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], text)
}
async fn openapi() -> Json<Value> {
    Json(serde_json::to_value(ApiDoc::openapi()).expect("OpenAPI serializes"))
}
async fn api_documentation() -> Response {
    api_docs::render(&serde_json::to_value(ApiDoc::openapi()).expect("OpenAPI serializes"))
}

async fn authenticate_for_authorization(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
) -> ApiResult<TokenAuth> {
    let peer_key = format!("socket-ip:{}", addr.ip());
    state
        .pre_auth_rate
        .check(&peer_key)
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError(Error::Authentication))?;
    let token = if let Some(token) = value.strip_prefix("Bearer ") {
        token.to_owned()
    } else if let Some(encoded) = value.strip_prefix("Basic ") {
        if !matches!(state.flavor, ApiFlavor::Compat31) || !state.config.api.compat_basic_auth {
            return Err(ApiError(Error::Authentication));
        }
        let ip = addr.ip();
        if !state
            .config
            .api
            .compat_basic_auth_allow
            .iter()
            .any(|network| network.contains(&ip))
        {
            return Err(ApiError(Error::Authentication));
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| ApiError(Error::Authentication))?;
        let decoded = String::from_utf8(decoded).map_err(|_| ApiError(Error::Authentication))?;
        let (id, secret) = decoded
            .split_once(':')
            .ok_or(ApiError(Error::Authentication))?;
        format!("lm_{id}_{secret}")
    } else {
        return Err(ApiError(Error::Authentication));
    };
    let auth = state.db.tokens().authenticate_without_usage(&token).await?;
    let account_key = format!("account:{}:token:{}", auth.user_id, auth.id);
    state
        .post_auth_rate
        .check(&account_key)
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    if !auth.has_scope(scope) {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    Ok(auth)
}

async fn finish_authorization(state: &AppState, auth: TokenAuth) -> ApiResult<TokenAuth> {
    state.db.tokens().mark_used(auth.id).await?;
    Ok(auth)
}

async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    finish_authorization(state, auth).await
}

const fn audit_context(auth: &TokenAuth, addr: SocketAddr) -> AuditContext {
    AuditContext::new(Some(auth.user_id), Some(auth.id), Some(addr.ip()))
}

const fn is_unbound(auth: &TokenAuth) -> bool {
    auth.list_id.is_none() && auth.domain_id.is_none()
}

async fn auth_allows_list(state: &AppState, auth: &TokenAuth, list: &ListId) -> ApiResult<bool> {
    let domain = state.db.domains().get(list.mail_host()).await?;
    Ok(auth.allows_list(list, domain.id))
}

async fn filter_members_for_auth(
    state: &AppState,
    auth: &TokenAuth,
    members: Vec<listmngr_core::Member>,
) -> ApiResult<Vec<listmngr_core::Member>> {
    let mut allowed = Vec::new();
    for member in members {
        if auth_allows_list(state, auth, &member.list_id).await? {
            allowed.push(member);
        }
    }
    Ok(allowed)
}

async fn auth_allows_user(state: &AppState, auth: &TokenAuth, user: UserId) -> ApiResult<bool> {
    if is_unbound(auth) {
        return Ok(true);
    }
    let list_ids: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT m.list_id FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.user_id=$1",
    )
    .bind(user.to_string())
    .fetch_all(state.db.pool())
    .await
    .map_err(|error| ApiError(Error::Database(error.to_string())))?;
    for value in list_ids {
        let list: ListId = value.parse()?;
        if auth_allows_list(state, auth, &list).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn auth_allows_address(state: &AppState, auth: &TokenAuth, email: &str) -> ApiResult<bool> {
    if is_unbound(auth) {
        return Ok(true);
    }
    let members = state.db.members().find(email).await?;
    Ok(!filter_members_for_auth(state, auth, members)
        .await?
        .is_empty())
}

async fn authorize_user(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    user: UserId,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    if (scope == "users:write" && (auth.list_id.is_some() || auth.domain_id.is_some()))
        || !auth_allows_user(state, &auth, user).await?
    {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    finish_authorization(state, auth).await
}

async fn authorize_address(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    email: &str,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    if (scope == "users:write" && (auth.list_id.is_some() || auth.domain_id.is_some()))
        || !auth_allows_address(state, &auth, email).await?
    {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    finish_authorization(state, auth).await
}

async fn authorize_user_and_address(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    user: UserId,
    email: &str,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    if (scope == "users:write" && (auth.list_id.is_some() || auth.domain_id.is_some()))
        || !auth_allows_user(state, &auth, user).await?
        || !auth_allows_address(state, &auth, email).await?
    {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    finish_authorization(state, auth).await
}

async fn authorize_admin(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    if !auth.scopes.contains("admin") || auth.list_id.is_some() || auth.domain_id.is_some() {
        return Err(ApiError(Error::Forbidden("admin".into())));
    }
    finish_authorization(state, auth).await
}

async fn authorize_domain(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    host: &str,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    let domain = state.db.domains().get(host).await?;
    let list_matches = auth
        .list_id
        .as_ref()
        .is_none_or(|list| list.mail_host() == domain.mail_host);
    if !auth.allows_domain(domain.id) || !list_matches {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    finish_authorization(state, auth).await
}
async fn authorize_list(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    list: &ListId,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    let domain = state.db.domains().get(list.mail_host()).await?;
    if !auth.allows_list(list, domain.id) {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    finish_authorization(state, auth).await
}
async fn authorize_member(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    member_id: MemberId,
) -> ApiResult<(TokenAuth, listmngr_core::Member)> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    let member = state.db.members().get(member_id).await?;
    let domain = state.db.domains().get(member.list_id.mail_host()).await?;
    if !auth.allows_list(&member.list_id, domain.id) {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    Ok((finish_authorization(state, auth).await?, member))
}
const fn peer(connect: ConnectInfo<SocketAddr>) -> SocketAddr {
    connect.0
}
fn paged<T: Serialize>(flavor: ApiFlavor, entries: T, query: &PageQuery) -> Result<Value, Error> {
    let entries = serde_json::to_value(entries).expect("page entries serialize");
    let entries = entries
        .as_array()
        .ok_or_else(|| Error::Validation("page entries must be an array".into()))?;
    let total = entries.len();
    let (start, end) = page_window(total, query)?;
    Ok(page_response(flavor, &entries[start..end], start, total))
}

fn page_window(total: usize, query: &PageQuery) -> Result<(usize, usize), Error> {
    let count = query.count.unwrap_or(50);
    if !(1..=100).contains(&count) {
        return Err(Error::Validation("count must be between 1 and 100".into()));
    }
    if query.cursor.is_some() && query.page.is_some() {
        return Err(Error::Validation(
            "cursor and page cannot be combined".into(),
        ));
    }
    let requested_start = if let Some(cursor) = &query.cursor {
        cursor
            .parse::<usize>()
            .map_err(|_| Error::Validation("invalid cursor".into()))?
    } else if let Some(page) = query.page {
        if page == 0 {
            return Err(Error::Validation("page is one-based".into()));
        }
        (page - 1).saturating_mul(count)
    } else {
        0
    };
    let start = requested_start.min(total);
    let end = start.saturating_add(count).min(total);
    Ok((start, end))
}

fn page_response(flavor: ApiFlavor, selected: &[Value], start: usize, total: usize) -> Value {
    let end = start.saturating_add(selected.len());
    let returned = selected.len();
    let next_cursor = (end < total).then(|| end.to_string());
    match flavor {
        ApiFlavor::Compat31 => json!({
            "entries": selected,
            "start": start,
            "count": returned,
            "total_size": total,
            "http_etag": "phase1"
        }),
        ApiFlavor::V1 => json!({
            "items": selected,
            "next_cursor": next_cursor,
            "total": total,
            "start": start,
            "count": returned
        }),
    }
}

/// A user as `mailmanclient` reads it: on the compatibility flavour the
/// `self_link`, `user_id`, `created_on` and a `password` that is always
/// `null` — the hash Mailman hands out is never shown here.
fn user_value(flavor: ApiFlavor, user: &listmngr_core::User) -> Value {
    let mut value = serde_json::to_value(user).expect("user serializes");
    if matches!(flavor, ApiFlavor::Compat31) {
        let object = value.as_object_mut().expect("user is an object");
        object.insert("self_link".into(), json!(format!("/3.1/users/{}", user.id)));
        object.insert("user_id".into(), json!(user.id.to_string()));
        object.insert("created_on".into(), json!(user.created_at));
        object.insert("password".into(), Value::Null);
    }
    value
}

/// An address as `mailmanclient` reads it: `self_link`, `verified`, and
/// the owner's `user` link on the compatibility flavour.
fn address_value(flavor: ApiFlavor, address: &listmngr_core::Address) -> Value {
    let mut value = serde_json::to_value(address).expect("address serializes");
    if matches!(flavor, ApiFlavor::Compat31) {
        let object = value.as_object_mut().expect("address is an object");
        object.insert(
            "self_link".into(),
            json!(format!("/3.1/addresses/{}", address.email)),
        );
        object.insert("verified".into(), json!(address.verified_on.is_some()));
        if let Some(user) = address.user_id {
            object.insert("user".into(), json!(format!("/3.1/users/{user}")));
        }
    }
    value
}

/// The user a `/users/{id}` segment names: the id, or on the
/// compatibility flavour an address of the account, as Mailman allows.
async fn resolve_user_id(s: &AppState, segment: &str) -> Result<UserId, Error> {
    if let Ok(id) = segment.parse::<UserId>() {
        return Ok(id);
    }
    if matches!(s.flavor, ApiFlavor::Compat31) && segment.contains('@') {
        return Ok(s.db.users().get_by_email(segment).await?.id);
    }
    Err(Error::Validation("user id".into()))
}

fn domain_value(flavor: ApiFlavor, domain: &listmngr_core::Domain) -> Value {
    let mut value = serde_json::to_value(domain).expect("domain serializes");
    if matches!(flavor, ApiFlavor::Compat31) {
        let object = value.as_object_mut().expect("domain is an object");
        object.insert(
            "self_link".into(),
            json!(format!("/3.1/domains/{}", domain.mail_host)),
        );
        // Mailman keeps no description as `null`; the doctest prints `None`.
        if domain.description.is_empty() {
            object.insert("description".into(), Value::Null);
        }
    }
    value
}

#[utoipa::path(
    get,
    path = "/api/v1/system/versions",
    responses((status = 200, description = "Successful operation", body = SystemVersionsResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_versions(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    Ok(Json(
        json!({"listmngr_version":env!("CARGO_PKG_VERSION"),"api_version":"3.1"}),
    ))
}
#[utoipa::path(
    get,
    path = "/api/v1/system/configuration",
    responses((status = 200, description = "Successful operation", body = ConfigurationResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_config(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    let value = s.config.redacted_json();
    if matches!(s.flavor, ApiFlavor::Compat31) {
        // Mailman's shape: the section names; each section is its own
        // resource.
        let sections: Vec<&String> = value
            .as_object()
            .map(|object| object.keys().collect())
            .unwrap_or_default();
        return Ok(Json(
            json!({"sections": sections, "self_link": "/3.1/system/configuration"}),
        ));
    }
    Ok(Json(value))
}
#[utoipa::path(
    get,
    path = "/api/v1/system/configuration/{section}",
    params(("section" = String, Path, description = "section path parameter")),
    responses((status = 200, description = "Successful operation", body = ConfigurationResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_config_section(
    State(s): State<AppState>,
    Path(section): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    let value = s.config.redacted_json();
    Ok(Json(
        value
            .get(&section)
            .cloned()
            .ok_or(ApiError(Error::NotFound(section)))?,
    ))
}
#[utoipa::path(
    get,
    path = "/api/v1/system/preferences",
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_preferences(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    Ok(Json(
        serde_json::to_value(Preferences::system_defaults(s.config.site.default_language))
            .expect("serialize"),
    ))
}
/// What one of the build's plugins adds.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PluginResponse {
    pub name: String,
    pub version: String,
    pub rules: Vec<String>,
    pub links: Vec<String>,
    pub handlers: Vec<String>,
    pub pipelines: Vec<String>,
    pub archivers: Vec<String>,
}

#[utoipa::path(
    get,
    path = "/api/v1/plugins",
    params(PageQuery),
    responses((status = 200, description = "The build's plugins and what each adds; none by default", body = PluginPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn plugins(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    let owned = |names: Vec<&'static str>| names.into_iter().map(str::to_owned).collect();
    let entries: Vec<PluginResponse> = listmngr_pipeline::plugins::describe()
        .into_iter()
        .map(|plugin| PluginResponse {
            name: plugin.name.to_owned(),
            version: plugin.version.to_owned(),
            rules: owned(plugin.rules),
            links: owned(plugin.links),
            handlers: owned(plugin.handlers),
            pipelines: owned(plugin.pipelines),
            archivers: owned(plugin.archivers),
        })
        .collect();
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/system/pipelines",
    params(PageQuery),
    responses((status = 200, description = "Successful operation", body = CatalogPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_pipelines(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    let registry = listmngr_mail::handlers::builtin_registry();
    if matches!(s.flavor, ApiFlavor::Compat31) {
        // Mailman's shape: the pipeline names alone.
        let mut names: Vec<&str> = registry
            .pipelines()
            .map(listmngr_pipeline::handlers::Pipeline::name)
            .collect();
        names.sort_unstable();
        return Ok(Json(
            json!({"pipelines": names, "self_link": "/3.1/system/pipelines"}),
        ));
    }
    let entries: Vec<Value> = registry
        .pipelines()
        .map(|pipeline| {
            json!({
                "name": pipeline.name(),
                "phase": "phase2",
                "executable": registry.is_executable(pipeline),
                "status": if registry.is_executable(pipeline) { "engine" } else { "declared" },
                "handlers": pipeline.handlers(),
            })
        })
        .collect();
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/system/chains",
    params(PageQuery),
    responses((status = 200, description = "Successful operation", body = CatalogPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_chains(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    if matches!(s.flavor, ApiFlavor::Compat31) {
        // Mailman's shape: the chain names alone.
        let mut names: Vec<&str> = listmngr_pipeline::builtin()
            .chains()
            .map(listmngr_pipeline::Chain::name)
            .collect();
        names.sort_unstable();
        return Ok(Json(
            json!({"chains": names, "self_link": "/3.1/system/chains"}),
        ));
    }
    let entries: Vec<Value> = listmngr_pipeline::builtin()
        .chains()
        .map(chain_entry)
        .collect();
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}

/// Project one chain as the engine actually holds it, so operators can see the
/// real link order rather than a name list. A chain that is declared but has no
/// links yet reports `executable: false`.
fn chain_entry(chain: &listmngr_pipeline::Chain) -> Value {
    use listmngr_pipeline::{ChainKind, Link};
    let (status, rules) = match chain.kind() {
        ChainKind::Terminal(_) => ("terminal", Vec::new()),
        ChainKind::Moderation => ("moderation", Vec::new()),
        ChainKind::HeaderMatch => ("header-match", Vec::new()),
        ChainKind::DmarcMitigation => ("dmarc-mitigation", Vec::new()),
        ChainKind::Links(links) if links.is_empty() => ("declared", Vec::new()),
        ChainKind::Links(links) => ("engine", links.iter().map(Link::rule).collect()),
    };
    json!({
        "name": chain.name(),
        "phase": "phase2",
        "executable": chain.is_executable(),
        "status": status,
        "rules": rules
    })
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct DomainInput {
    mail_host: String,
    #[serde(default)]
    description: String,
    alias_domain: Option<String>,
}
#[utoipa::path(
    post,
    path = "/api/v1/domains",
    request_body(content((DomainInput = "application/json"), (DomainInput = "application/x-www-form-urlencoded"))),
    responses((status = 201, description = "Successful operation", body = listmngr_core::Domain), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domains_create(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<DomainInput>,
) -> ApiResult<Response> {
    let addr = peer(c);
    let auth = authenticate_for_authorization(&s, &h, addr, "lists:write").await?;
    if !is_unbound(&auth) {
        return Err(ApiError(Error::Forbidden("lists:write".into())));
    }
    let auth = finish_authorization(&s, auth).await?;
    let domain =
        s.db.domains()
            .create_with_context(
                &v.mail_host,
                &v.description,
                v.alias_domain.as_deref(),
                &audit_context(&auth, addr),
            )
            .await?;
    let value = domain_value(s.flavor, &domain);
    let response = match s.flavor {
        ApiFlavor::Compat31 => (
            StatusCode::CREATED,
            [(
                header::LOCATION,
                format!("/3.1/domains/{}", domain.mail_host),
            )],
            Json(value),
        )
            .into_response(),
        ApiFlavor::V1 => (StatusCode::CREATED, Json(value)).into_response(),
    };
    Ok(response)
}
#[utoipa::path(
    get,
    path = "/api/v1/domains",
    params(PageQuery),
    responses((status = 200, description = "Successful operation", body = DomainPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domains_list(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let auth = authenticate_for_authorization(&s, &h, peer(c), "lists:read").await?;
    let domains =
        s.db.domains()
            .list()
            .await?
            .into_iter()
            .filter(|domain| {
                auth.allows_domain(domain.id)
                    && auth
                        .list_id
                        .as_ref()
                        .is_none_or(|list| list.mail_host() == domain.mail_host)
            })
            .map(|domain| domain_value(s.flavor, &domain))
            .collect::<Vec<_>>();
    finish_authorization(&s, auth).await?;
    Ok(Json(paged(s.flavor, domains, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/domains/{host}",
    params(("host" = String, Path, description = "host path parameter")),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Domain), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domains_get(
    State(s): State<AppState>,
    Path(host): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_domain(&s, &h, peer(c), "lists:read", &host).await?;
    Ok(Json(domain_value(
        s.flavor,
        &s.db.domains().get(&host).await?,
    )))
}
#[utoipa::path(
    delete,
    path = "/api/v1/domains/{host}",
    params(("host" = String, Path, description = "host path parameter")),
    responses((status = 204, description = "Deleted"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domains_delete(
    State(s): State<AppState>,
    Path(host): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let addr = peer(c);
    let auth = authorize_domain(&s, &h, addr, "lists:write", &host).await?;
    s.db.domains()
        .delete_with_context(&host, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    get,
    path = "/api/v1/domains/{host}/lists",
    params(PageQuery, ("host" = String, Path, description = "host path parameter"), ("advertised" = Option<bool>, Query, description = "Filter by advertised status")),
    responses((status = 200, description = "Successful operation", body = MailingListPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domain_lists(
    State(s): State<AppState>,
    Path(host): Path<String>,
    Query(page_query): Query<PageQuery>,
    Query(q): Query<ListQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let auth = authenticate_for_authorization(&s, &h, peer(c), "lists:read").await?;
    let domain = s.db.domains().get(&host).await?;
    if !auth.allows_domain(domain.id) {
        return Err(ApiError(Error::Forbidden("lists:read".into())));
    }
    let mut lists = Vec::new();
    for list in s.db.lists().by_domain(&host).await? {
        // `?advertised=true` narrows a domain's lists as it does `/lists`.
        if q.advertised.is_some_and(|wanted| list.advertised != wanted) {
            continue;
        }
        if auth.allows_list(&list.id, domain.id) {
            lists.push(list_value(s.flavor, &list));
        }
    }
    finish_authorization(&s, auth).await?;
    Ok(Json(paged(s.flavor, lists, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/domains/{host}/owners",
    params(PageQuery, ("host" = String, Path, description = "host path parameter")),
    responses((status = 200, description = "Successful operation", body = UserPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domain_owners(
    State(s): State<AppState>,
    Path(host): Path<String>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_domain(&s, &h, peer(c), "lists:read", &host).await?;
    Ok(Json(paged(
        s.flavor,
        s.db.domains().owners(&host).await?,
        &page_query,
    )?))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct ListQuery {
    advertised: Option<bool>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct FindListsInput {
    subscriber: String,
    role: Option<MemberRole>,
    mail_host: Option<String>,
    count: Option<usize>,
    page: Option<usize>,
}

#[utoipa::path(
    post,
    path = "/api/v1/lists/find",
    request_body(content((FindListsInput = "application/json"), (FindListsInput = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = MailingListPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
/// Mailman's `lists/find`: the lists on which an address holds a role
/// (any role when none is given), optionally within one mail host.
async fn lists_find(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<FindListsInput>,
) -> ApiResult<Json<Value>> {
    let auth = authenticate_for_authorization(&s, &h, peer(c), "lists:read").await?;
    let memberships = s.db.members().find(&v.subscriber).await?;
    let mut seen = std::collections::BTreeSet::new();
    let mut lists = Vec::new();
    for member in memberships {
        if v.role.is_some_and(|role| member.role != role)
            || !seen.insert(member.list_id.to_string())
        {
            continue;
        }
        let list = s.db.lists().get(&member.list_id).await?;
        if v.mail_host
            .as_deref()
            .is_some_and(|host| list.id.mail_host() != host)
        {
            continue;
        }
        let domain = s.db.domains().get(list.id.mail_host()).await?;
        if auth.allows_list(&list.id, domain.id) {
            lists.push(list_value(s.flavor, &list));
        }
    }
    finish_authorization(&s, auth).await?;
    let page_query = PageQuery {
        cursor: None,
        page: v.page,
        count: v.count,
    };
    Ok(Json(paged(s.flavor, lists, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/lists",
    params(PageQuery, ("advertised" = Option<bool>, Query, description = "Filter by advertised status")),
    responses((status = 200, description = "Successful operation", body = MailingListPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn lists_list(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let auth = authenticate_for_authorization(&s, &h, peer(c), "lists:read").await?;
    let mut lists = Vec::new();
    for list in s.db.lists().list(q.advertised).await? {
        if auth_allows_list(&s, &auth, &list.id).await? {
            lists.push(list_value(s.flavor, &list));
        }
    }
    finish_authorization(&s, auth).await?;
    Ok(Json(paged(s.flavor, lists, &page_query)?))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct ListInput {
    list_id: Option<ListId>,
    fqdn_listname: Option<String>,
    display_name: Option<String>,
    style: Option<String>,
    style_name: Option<String>,
}

impl ListInput {
    fn into_new_list(self) -> Result<NewList, Error> {
        let list_id = match (self.list_id, self.fqdn_listname) {
            (Some(list_id), _) => list_id,
            (None, Some(fqdn)) => {
                let (name, host) = fqdn
                    .split_once('@')
                    .ok_or_else(|| Error::InvalidListId(fqdn.clone()))?;
                format!("{name}.{host}").parse()?
            }
            (None, None) => return Err(Error::Validation("list_id is required".into())),
        };
        // Mailman's default is the list name with its first letter upper-cased
        // (`str.capitalize()`): `test-1` becomes `Test-1`.
        let display_name = self.display_name.unwrap_or_else(|| {
            let mut chars = list_id.list_name().chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        });
        Ok(NewList {
            list_id,
            display_name,
            style: self
                .style
                .or(self.style_name)
                .unwrap_or_else(|| "legacy-default".into()),
        })
    }
}

fn list_value(flavor: ApiFlavor, list: &listmngr_core::MailingList) -> Value {
    let mut value = serde_json::to_value(list).expect("list serializes");
    if matches!(flavor, ApiFlavor::Compat31) {
        let object = value.as_object_mut().expect("list is an object");
        object.insert("list_id".into(), json!(list.id));
        object.insert("fqdn_listname".into(), json!(list.fqdn_listname()));
        object.insert("list_name".into(), json!(list.id.list_name()));
        object.insert("mail_host".into(), json!(list.id.mail_host()));
        object.insert("self_link".into(), json!(format!("/3.1/lists/{}", list.id)));
    }
    value
}

fn parse_list_path(flavor: ApiFlavor, value: &str) -> Result<ListId, Error> {
    if matches!(flavor, ApiFlavor::Compat31) {
        if let Some((name, host)) = value.split_once('@') {
            return format!("{name}.{host}").parse();
        }
    }
    value.parse()
}

#[utoipa::path(
    post,
    path = "/api/v1/lists",
    request_body(content((ListInput = "application/json"), (ListInput = "application/x-www-form-urlencoded"))),
    responses((status = 201, description = "Successful operation", body = listmngr_core::MailingList), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn lists_create(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<ListInput>,
) -> ApiResult<Response> {
    let new = v.into_new_list()?;
    let addr = peer(c);
    let auth = authenticate_for_authorization(&s, &h, addr, "lists:write").await?;
    if !auth_allows_list(&s, &auth, &new.list_id).await? {
        return Err(ApiError(Error::Forbidden("lists:write".into())));
    }
    let auth = finish_authorization(&s, auth).await?;
    let list =
        s.db.lists()
            .create_with_context(new, &audit_context(&auth, addr))
            .await?;
    refresh_mta_maps(&s).await;
    let value = list_value(s.flavor, &list);
    let response = match s.flavor {
        ApiFlavor::Compat31 => (
            StatusCode::CREATED,
            [(header::LOCATION, format!("/3.1/lists/{}", list.id))],
            Json(value),
        )
            .into_response(),
        ApiFlavor::V1 => (StatusCode::CREATED, Json(value)).into_response(),
    };
    Ok(response)
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = listmngr_core::MailingList), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn lists_get(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    Ok(Json(list_value(s.flavor, &s.db.lists().get(&id).await?)))
}
#[utoipa::path(
    delete,
    path = "/api/v1/lists/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 204, description = "Deleted"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn lists_delete(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    s.db.lists()
        .delete_with_context(&id, &audit_context(&auth, addr))
        .await?;
    refresh_mta_maps(&s).await;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/styles",
    params(PageQuery),
    responses((status = 200, description = "Successful operation", body = StringPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn styles(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "lists:read").await?;
    let styles = builtin_styles();
    if matches!(s.flavor, ApiFlavor::Compat31) {
        // Mailman 3.3's shape: the styles with their descriptions, the
        // default, and the bare names for older clients.
        return Ok(Json(json!({
            "styles": styles.iter().map(|style| json!({"name": style.name(), "description": style.description()})).collect::<Vec<_>>(),
            "style_names": styles.iter().map(|style| style.name()).collect::<Vec<_>>(),
            "default": "legacy-default",
        })));
    }
    Ok(Json(paged(
        s.flavor,
        styles.iter().map(|v| v.name()).collect::<Vec<_>>(),
        &page_query,
    )?))
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/config",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = ListConfigResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    Ok(Json(bounce_config::project(
        s.flavor,
        list_config_value(
            &s.db.lists().get(&id).await?,
            &s.config.mailman.noreply_address,
        ),
    )))
}

fn list_config_value(list: &listmngr_core::MailingList, noreply_local_part: &str) -> Value {
    let mut value = serde_json::to_value(list).expect("list config serializes");
    let object = value.as_object_mut().expect("list config is an object");
    object.insert("mail_host".into(), json!(list.id.mail_host()));
    object.insert("list_name".into(), json!(list.id.list_name()));
    object.insert("list_id".into(), json!(list.id));
    object.insert("fqdn_listname".into(), json!(list.id.posting_address()));
    object.insert("posting_address".into(), json!(list.id.posting_address()));
    object.insert("bounces_address".into(), json!(list.id.bounces_address()));
    object.insert("join_address".into(), json!(list.id.join_address()));
    object.insert("leave_address".into(), json!(list.id.leave_address()));
    object.insert("owner_address".into(), json!(list.id.owner_address()));
    object.insert("request_address".into(), json!(list.id.request_address()));
    object.insert(
        "no_reply_address".into(),
        json!(format!("{noreply_local_part}@{}", list.id.mail_host())),
    );
    value
}
#[utoipa::path(
    put,
    path = "/api/v1/lists/{id}/config",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content((ListConfigInput = "application/json"), (ListConfigInput = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = ListConfigResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config_put(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<FormAwareObject>,
) -> ApiResult<Json<Value>> {
    list_config_write(state, path, headers, connect, body, true).await
}
#[utoipa::path(
    patch,
    path = "/api/v1/lists/{id}/config",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content((ListConfigInput = "application/json"), (ListConfigInput = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = ListConfigResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config_patch(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<FormAwareObject>,
) -> ApiResult<Json<Value>> {
    list_config_write(state, path, headers, connect, body, false).await
}
async fn list_config_write(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(FormAwareObject(mut v)): JsonOrForm<FormAwareObject>,
    replace: bool,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    bounce_config::normalize(s.flavor, &mut v)?;
    normalize_list_config_form(&mut v, &h)?;
    let update = if replace {
        let defaults = listmngr_core::MailingList::new(id.clone(), id.list_name().to_owned());
        // Every writable setting at its default; the read-only projection
        // fields are not part of a replacement.
        let mut replacement = serde_json::to_value(&defaults).expect("serialize");
        let object = replacement
            .as_object_mut()
            .expect("replacement is an object");
        for read_only in [
            "id",
            "created_at",
            "last_post_at",
            "post_id",
            "volume",
            "digest_last_sent_at",
            "style_name",
            "usenet_watermark",
        ] {
            object.remove(read_only);
        }
        let supplied = v
            .as_object()
            .ok_or_else(|| ApiError(Error::Validation("list config must be an object".into())))?;
        object.extend(supplied.clone());
        replacement
    } else {
        v
    };
    Ok(Json(bounce_config::project(
        s.flavor,
        serde_json::to_value(
            s.db.lists()
                .update_with_context(&id, &update, &audit_context(&auth, addr))
                .await?,
        )
        .expect("serialize"),
    )))
}
// Form values are strings; keep JSON strict and convert only known numeric settings.
fn normalize_list_config_form(value: &mut Value, headers: &HeaderMap) -> ApiResult<()> {
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"))
    {
        return Ok(());
    }
    for field in [
        "dmarc_mitigate_unconditionally",
        "send_welcome_message",
        "send_goodbye_message",
        "process_bounces",
        "bounce_notify_owner_on_disable",
        "bounce_notify_owner_on_bounce_increment",
        "bounce_notify_owner_on_removal",
        "administrivia",
        "require_explicit_destination",
        "respond_to_post_requests",
        "admin_immed_notify",
        "admin_notify_mchanges",
        "digests_enabled",
        "digest_send_periodic",
        "advertised",
        "anonymous_list",
        "emergency",
        "filter_content",
        "collapse_alternatives",
        "convert_html_to_plaintext",
        "include_rfc2369_headers",
        "allow_list_posts",
        "first_strip_reply_to",
        "include_sender_header",
        "topics_enabled",
        "gateway_to_mail",
        "gateway_to_news",
        "nntp_prefix_subject_too",
    ] {
        if let Some(Value::String(text)) = value.get(field) {
            let enabled = text
                .to_ascii_lowercase()
                .parse::<bool>()
                .map_err(|_| ApiError(Error::Validation(field.into())))?;
            value[field] = json!(enabled);
        }
    }
    // List settings: one form value is a one-element list, an empty value
    // clears the list, repeated keys already arrived as an array.
    for field in [
        "acceptable_aliases",
        "accept_these_nonmembers",
        "hold_these_nonmembers",
        "reject_these_nonmembers",
        "discard_these_nonmembers",
        "filter_types",
        "pass_types",
        "filter_extensions",
        "pass_extensions",
        "dmarc_addresses",
    ] {
        if let Some(Value::String(text)) = value.get(field) {
            value[field] = if text.is_empty() {
                json!([])
            } else {
                json!([text])
            };
        }
    }
    for field in ["bounce_score_threshold", "digest_size_threshold"] {
        if let Some(Value::String(text)) = value.get(field) {
            let number = text
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .ok_or_else(|| ApiError(Error::Validation(field.into())))?;
            value[field] = json!(number);
        }
    }
    for field in [
        "max_message_size",
        "max_num_recipients",
        "bounce_info_stale_after",
        "bounce_you_are_disabled_warnings",
        "bounce_you_are_disabled_warnings_interval",
        "next_digest_number",
        "topics_bodylines_limit",
        "autoresponse_grace_period",
    ] {
        if let Some(Value::String(text)) = value.get(field) {
            let number = text
                .parse::<i64>()
                .map_err(|_| ApiError(Error::Validation(field.into())))?;
            value[field] = json!(number);
        }
    }
    Ok(())
}

#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/config/{attr}",
    params(("id" = String, Path, description = "id path parameter"), ("attr" = String, Path, description = "attr path parameter")),
    responses((status = 200, description = "Successful operation", body = ListConfigAttributeValue), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config_attr(
    State(s): State<AppState>,
    Path((id, attr)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    let v = bounce_config::project(
        s.flavor,
        list_config_value(
            &s.db.lists().get(&id).await?,
            &s.config.mailman.noreply_address,
        ),
    );
    Ok(Json(
        v.get(&attr)
            .cloned()
            .ok_or(ApiError(Error::NotFound(attr)))?,
    ))
}
#[utoipa::path(
    put,
    path = "/api/v1/lists/{id}/config/{attr}",
    params(("id" = String, Path, description = "id path parameter"), ("attr" = String, Path, description = "attr path parameter")),
    request_body(content((ListConfigAttributeValue = "application/json"), (ListConfigAttributeValue = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = listmngr_core::MailingList), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config_attr_put(
    state: State<AppState>,
    path: Path<(String, String)>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    list_config_attr_write(state, path, headers, connect, body).await
}
#[utoipa::path(
    patch,
    path = "/api/v1/lists/{id}/config/{attr}",
    params(("id" = String, Path, description = "id path parameter"), ("attr" = String, Path, description = "attr path parameter")),
    request_body(content((ListConfigAttributeValue = "application/json"), (ListConfigAttributeValue = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = listmngr_core::MailingList), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config_attr_patch(
    state: State<AppState>,
    path: Path<(String, String)>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    list_config_attr_write(state, path, headers, connect, body).await
}
async fn list_config_attr_write(
    State(s): State<AppState>,
    Path((id, attr)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    let value = if v.is_object() {
        v.get(&attr)
            .cloned()
            .ok_or(ApiError(Error::Validation(format!("missing {attr}"))))?
    } else {
        v
    };
    let mut update = json!({attr:value});
    bounce_config::normalize(s.flavor, &mut update)?;
    Ok(Json(bounce_config::project(
        s.flavor,
        serde_json::to_value(
            s.db.lists()
                .update_with_context(&id, &update, &audit_context(&auth, addr))
                .await?,
        )
        .expect("serialize"),
    )))
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/archivers",
    params(PageQuery, ("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = ArchiverPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_archivers(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    let stored = s.db.lists().archivers(&id).await?;
    if matches!(s.flavor, ApiFlavor::Compat31) {
        // Mailman's shape: every site archiver with its switch.
        let mut object = serde_json::Map::new();
        for name in archiver_names() {
            let on = stored.iter().any(|(stored, on)| stored == name && *on);
            object.insert((*name).to_owned(), json!(on));
        }
        // No `self_link`: the doctest iterates the keys as the archivers.
        return Ok(Json(Value::Object(object)));
    }
    Ok(Json(paged(s.flavor, stored, &page_query)?))
}

/// The archivers a list can switch on, as `[archive] archivers` and the
/// settings page know them.
const ARCHIVER_NAMES: [&str; 4] = ["mail-archive", "mhonarc", "prototype", "hyperkitty"];

/// The built-in archivers and the ones the build's plugins add.
fn archiver_names() -> Vec<&'static str> {
    ARCHIVER_NAMES
        .into_iter()
        .chain(
            listmngr_pipeline::plugins::describe()
                .into_iter()
                .flat_map(|plugin| plugin.archivers),
        )
        .collect()
}

#[utoipa::path(
    patch,
    path = "/api/v1/lists/{id}/archivers",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content((Object = "application/json"), (Object = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Successful operation"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
/// Mailman's archiver switches: each named archiver on or off (`True`
/// and `False` strings on a form, booleans in JSON).
async fn list_archivers_set(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<Value>,
) -> ApiResult<StatusCode> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    let object = v
        .as_object()
        .ok_or_else(|| ApiError(Error::Validation("archivers".into())))?;
    let context = audit_context(&auth, addr);
    for (name, value) in object {
        if !archiver_names().contains(&name.as_str()) {
            return Err(ApiError(Error::Validation(format!("archiver {name}"))));
        }
        let enabled = preference_bool(name, value)?;
        s.db.lists()
            .set_archiver_with_context(&id, name, enabled, &context)
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/templates",
    params(PageQuery, ("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = TemplatePageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_templates(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    Ok(Json(paged(
        s.flavor,
        s.db.lists().templates(&id).await?,
        &page_query,
    )?))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct MemberInput {
    list_id: ListId,
    subscriber: String,
    #[serde(default = "member_role")]
    role: MemberRole,
    #[serde(default)]
    display_name: String,
    #[serde(flatten)]
    confirmation: ConfirmationInput,
    #[serde(flatten)]
    workflow: WorkflowInput,
}
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
struct ConfirmationInput {
    #[serde(default, deserialize_with = "mailman_bool")]
    pre_verified: bool,
    #[serde(default, deserialize_with = "mailman_bool")]
    pre_confirmed: bool,
    #[serde(default, deserialize_with = "mailman_bool")]
    pre_approved: bool,
}

fn mailman_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct BoolVisitor;

    impl serde::de::Visitor<'_> for BoolVisitor {
        type Value = bool;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a boolean or case-insensitive boolean string")
        }

        fn visit_bool<E>(self, value: bool) -> Result<bool, E> {
            Ok(value)
        }

        fn visit_str<E>(self, value: &str) -> Result<bool, E>
        where
            E: serde::de::Error,
        {
            match value.to_ascii_lowercase().as_str() {
                "true" => Ok(true),
                "false" => Ok(false),
                _ => Err(E::invalid_value(serde::de::Unexpected::Str(value), &self)),
            }
        }
    }

    deserializer.deserialize_any(BoolVisitor)
}

async fn member_value(state: &AppState, member: &listmngr_core::Member) -> Result<Value, Error> {
    let mut value = serde_json::to_value(member).expect("member serializes");
    if matches!(state.flavor, ApiFlavor::Compat31) {
        let address = state.db.addresses().get_by_id(member.address_id).await?;
        let email = address.email;
        let object = value.as_object_mut().expect("member is an object");
        object.insert("email".into(), json!(email));
        object.insert("address".into(), json!(format!("/3.1/addresses/{email}")));
        object.insert(
            "self_link".into(),
            json!(format!("/3.1/members/{}", member.id)),
        );
        object.insert("member_id".into(), json!(member.id));
        // Mailman links the member's user; a membership by address still
        // belongs to the address's user.
        if let Some(user) = member.user_id.or(address.user_id) {
            object.insert("user".into(), json!(format!("/3.1/users/{user}")));
        }
    }
    Ok(value)
}
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
struct WorkflowInput {
    /// Mailman's clients send booleans as `True`/`False` strings on forms.
    #[serde(default, deserialize_with = "mailman_bool")]
    invitation: bool,
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct MassMemberRow {
    subscriber: String,
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct MassMemberInput {
    operation: String,
    list_id: ListId,
    members: Vec<MassMemberRow>,
}
#[utoipa::path(
    post,
    path = "/api/v1/members/mass",
    request_body(content = MassMemberInput, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = ProcessedResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn members_mass(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<MassMemberInput>,
) -> ApiResult<impl IntoResponse> {
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "members:write", &v.list_id).await?;
    if !matches!(v.operation.as_str(), "subscribe" | "unsubscribe" | "sync") {
        return Err(ApiError(Error::Validation(
            "operation must be subscribe, unsubscribe, or sync".into(),
        )));
    }

    let emails = v
        .members
        .iter()
        .map(|row| row.subscriber.clone())
        .collect::<Vec<_>>();
    let count =
        s.db.members()
            .mass_with_context(
                &v.list_id,
                &v.operation,
                &emails,
                &audit_context(&auth, addr),
            )
            .await?;
    Ok((StatusCode::OK, Json(json!({"processed":count}))))
}
const fn member_role() -> MemberRole {
    MemberRole::Member
}
#[utoipa::path(
    post,
    path = "/api/v1/members",
    request_body(content((MemberInput = "application/json"), (MemberInput = "application/x-www-form-urlencoded"))),
    responses((status = 201, description = "Successful operation", body = listmngr_core::Member), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn members_create(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<MemberInput>,
) -> ApiResult<Response> {
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "members:write", &v.list_id).await?;
    // mailmanclient's administrative role methods omit subscription workflow
    // flags. Role assignment does not itself verify ownership of an address.
    // Owner, moderator and nonmember are role records, not subscriptions:
    // no workflow, no confirmation flags, exactly as mailmanclient's
    // administrative helpers post them.
    let administrative_role = !matches!(v.role, MemberRole::Member);
    if administrative_role {
        if v.workflow.invitation {
            return Err(ApiError(Error::Validation(
                "an invitation subscribes a member, not an owner or moderator".into(),
            )));
        }
    } else {
        // Mailman's registrar: the list's policy decides what is still
        // missing and each flag supplies one of those steps in advance.
        let outcome =
            s.db.workflows()
                .subscribe(
                    &listmngr_db::workflows::AdminSubscription {
                        list: &v.list_id,
                        email: &v.subscriber,
                        display_name: &v.display_name,
                        pre_verified: v.confirmation.pre_verified,
                        pre_confirmed: v.confirmation.pre_confirmed,
                        pre_approved: v.confirmation.pre_approved,
                        invitation: v.workflow.invitation,
                    },
                    &audit_context(&auth, addr),
                    chrono::Utc::now().timestamp_millis(),
                )
                .await?;
        if let listmngr_db::workflows::SubscriptionOutcome::Held { token, token_owner } = outcome {
            let token_owner = match token_owner {
                listmngr_db::workflows::TokenOwner::Subscriber => "subscriber",
                listmngr_db::workflows::TokenOwner::Moderator => "moderator",
            };
            return Ok((
                StatusCode::ACCEPTED,
                Json(json!({
                    "token": token,
                    "token_owner": token_owner,
                    "http_etag": format!("{:x}", Sha256::digest(&token)),
                })),
            )
                .into_response());
        }
        let member = subscribed_member(&s, &v.list_id, &v.subscriber).await?;
        let value = member_value(&s, &member).await?;
        return Ok(created_member(s.flavor, &member, value));
    }
    let member =
        s.db.members()
            .subscribe_with_context(
                NewMember {
                    list_id: v.list_id,
                    email: v.subscriber.clone(),
                    role: v.role,
                    subscription_mode: SubscriptionMode::AsAddress,
                    display_name: v.display_name,
                },
                v.confirmation.pre_verified,
                &audit_context(&auth, addr),
            )
            .await?;
    let value = member_value(&s, &member).await?;
    Ok(created_member(s.flavor, &member, value))
}

/// The member a just-completed subscription created.
async fn subscribed_member(
    s: &AppState,
    list: &ListId,
    email: &str,
) -> ApiResult<listmngr_core::Member> {
    s.db.members()
        .find(email)
        .await?
        .into_iter()
        .find(|member| member.list_id == *list && member.role == MemberRole::Member)
        .ok_or_else(|| ApiError(Error::NotFound("member".into())))
}

fn created_member(flavor: ApiFlavor, member: &listmngr_core::Member, value: Value) -> Response {
    match flavor {
        ApiFlavor::Compat31 => (
            StatusCode::CREATED,
            [(header::LOCATION, format!("/3.1/members/{}", member.id))],
            Json(value),
        )
            .into_response(),
        ApiFlavor::V1 => (StatusCode::CREATED, Json(value)).into_response(),
    }
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/roster/{role}",
    params(PageQuery, ("id" = String, Path, description = "id path parameter"), ("role" = String, Path, description = "role path parameter")),
    responses((status = 200, description = "Successful operation", body = MemberPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn roster(
    State(s): State<AppState>,
    Path((id, role)): Path<(String, String)>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "members:read", &id).await?;
    let members = s.db.members().roster(&id, role.parse()?).await?;
    let mut entries = Vec::with_capacity(members.len());
    for member in members {
        entries.push(member_value(&s, &member).await?);
    }
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}
/// The addresses a mass unsubscribe names: `emails` repeated on a form,
/// as mailmanclient sends it, or a JSON list.
fn mass_unsubscribe_emails(headers: &HeaderMap, body: &[u8]) -> Result<Vec<String>, Error> {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));
    if json {
        #[derive(Deserialize)]
        struct Emails {
            emails: Vec<String>,
        }
        return serde_json::from_slice::<Emails>(body)
            .map(|input| input.emails)
            .map_err(|error| Error::Validation(error.to_string()));
    }
    let pairs: Vec<(String, String)> =
        serde_urlencoded::from_bytes(body).map_err(|error| Error::Validation(error.to_string()))?;
    Ok(pairs
        .into_iter()
        .filter(|(key, _)| key == "emails")
        .map(|(_, value)| value)
        .collect())
}

#[utoipa::path(
    delete,
    path = "/api/v1/lists/{id}/roster/{role}",
    params(("id" = String, Path, description = "id path parameter"), ("role" = String, Path, description = "role path parameter")),
    request_body(content = String, content_type = "application/x-www-form-urlencoded"),
    responses((status = 200, description = "Each address mapped to whether it was unsubscribed", body = Object), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
/// Mailman's mass unsubscribe: `DELETE …/roster/member` with `emails`,
/// answering each address with whether it was a member and is now gone.
async fn roster_mass_unsubscribe(
    State(s): State<AppState>,
    Path((id, role)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    body: String,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    let role: MemberRole = role.parse()?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "members:write", &id).await?;
    let emails = mass_unsubscribe_emails(&h, body.as_bytes())?;
    if emails.is_empty() || emails.len() > 1000 {
        return Err(ApiError(Error::Validation("emails".into())));
    }
    let context = audit_context(&auth, addr);
    let mut outcome = serde_json::Map::new();
    for email in emails {
        let member =
            s.db.members()
                .find(&email)
                .await?
                .into_iter()
                .find(|member| member.list_id == id && member.role == role);
        let removed = match member {
            Some(member) => {
                s.db.members()
                    .delete_with_context(member.id, &context)
                    .await?;
                true
            }
            None => false,
        };
        outcome.insert(email, json!(removed));
    }
    Ok(Json(Value::Object(outcome)))
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/member/{email}",
    params(("id" = String, Path, description = "id path parameter"), ("email" = String, Path, description = "email path parameter")),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Member), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_member(
    State(s): State<AppState>,
    Path((id, email)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    role_membership(&s, &h, c, &id, MemberRole::Member, &email).await
}

/// Mailman's `lists/{id}/{role}/{email}`: the address's membership in one
/// role on the list.
async fn role_membership(
    s: &AppState,
    h: &HeaderMap,
    c: ConnectInfo<SocketAddr>,
    id: &str,
    role: MemberRole,
    email: &str,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, id)?;
    authorize_list(s, h, peer(c), "members:read", &id).await?;
    let values = s.db.members().find(email).await?;
    let member = values
        .iter()
        .find(|member| member.list_id == id && member.role == role)
        .ok_or_else(|| ApiError(Error::NotFound(email.to_owned())))?;
    Ok(Json(member_value(s, member).await?))
}

/// Mailman's `DELETE lists/{id}/{role}/{email}` for the administrative
/// roles: the owner, moderator or nonmember record goes at once.
async fn role_membership_delete(
    s: &AppState,
    h: &HeaderMap,
    c: ConnectInfo<SocketAddr>,
    id: &str,
    role: MemberRole,
    email: &str,
) -> ApiResult<StatusCode> {
    let id = parse_list_path(s.flavor, id)?;
    let addr = peer(c);
    let auth = authorize_list(s, h, addr, "members:write", &id).await?;
    let values = s.db.members().find(email).await?;
    let member = values
        .iter()
        .find(|member| member.list_id == id && member.role == role)
        .ok_or_else(|| ApiError(Error::NotFound(email.to_owned())))?;
    s.db.members()
        .delete_with_context(member.id, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

macro_rules! role_routes {
    ($get:ident, $delete:ident, $role:expr, $path:literal) => {
        #[utoipa::path(get, path = $path, params(("id" = String, Path, description = "id path parameter"), ("email" = String, Path, description = "email path parameter")), responses((status = 200, description = "Successful operation", body = listmngr_core::Member), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
        async fn $get(
            State(s): State<AppState>,
            Path((id, email)): Path<(String, String)>,
            h: HeaderMap,
            c: ConnectInfo<SocketAddr>,
        ) -> ApiResult<Json<Value>> {
            role_membership(&s, &h, c, &id, $role, &email).await
        }
        #[utoipa::path(delete, path = $path, params(("id" = String, Path, description = "id path parameter"), ("email" = String, Path, description = "email path parameter")), responses((status = 204, description = "Successful operation"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
        async fn $delete(
            State(s): State<AppState>,
            Path((id, email)): Path<(String, String)>,
            h: HeaderMap,
            c: ConnectInfo<SocketAddr>,
        ) -> ApiResult<StatusCode> {
            role_membership_delete(&s, &h, c, &id, $role, &email).await
        }
    };
}
role_routes!(
    list_owner,
    list_owner_delete,
    MemberRole::Owner,
    "/api/v1/lists/{id}/owner/{email}"
);
role_routes!(
    list_moderator,
    list_moderator_delete,
    MemberRole::Moderator,
    "/api/v1/lists/{id}/moderator/{email}"
);
role_routes!(
    list_nonmember,
    list_nonmember_delete,
    MemberRole::Nonmember,
    "/api/v1/lists/{id}/nonmember/{email}"
);

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct UnsubscribeInput {
    #[serde(default, deserialize_with = "mailman_bool")]
    pre_confirmed: bool,
    #[serde(default, deserialize_with = "mailman_bool")]
    pre_approved: bool,
}

#[utoipa::path(
    delete,
    path = "/api/v1/lists/{id}/member/{email}",
    params(("id" = String, Path), ("email" = String, Path)),
    request_body(content((UnsubscribeInput = "application/json"), (UnsubscribeInput = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Confirmed administrative removal"), (status = 400, description = "Both pre_confirmed and pre_approved must be true", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_member_delete(
    State(s): State<AppState>,
    Path((id, email)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(input): JsonOrForm<UnsubscribeInput>,
) -> ApiResult<StatusCode> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "members:write", &id).await?;
    if !input.pre_confirmed || !input.pre_approved {
        return Err(ApiError(Error::Validation("only pre_confirmed=true and pre_approved=true administrative unsubscribe is supported here; use the public leave confirmation workflow otherwise".into())));
    }
    let members = s.db.members().find(&email).await?;
    let member = members
        .iter()
        .find(|member| member.list_id == id && member.role == MemberRole::Member)
        .ok_or_else(|| ApiError(Error::NotFound(email)))?;
    s.db.members()
        .delete_with_context(member.id, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

const HELD_OUT_MAX_ATTEMPTS: i64 = 8;

/// Held-message wire shape for `mailmanclient`'s `HeldMessage`.
///
/// Fields: `hold_date`, `message_id`, `msg`, `reason`, `request_id`,
/// `self_link`, `sender`, `subject`, `type`. Documented separately since the
/// flavor-specific runtime JSON is built by hand (see `held_entry_value`),
/// like `list_value`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct HeldMessageResponse {
    pub hold_date: String,
    pub message_id: String,
    pub msg: String,
    pub reason: String,
    pub request_id: String,
    pub self_link: String,
    pub sender: String,
    pub subject: String,
    pub r#type: String,
}
page_response!(HeldMessagePageResponse, HeldMessageResponse);
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct CountResponse {
    count: usize,
}

async fn held_entry_value(
    s: &AppState,
    list: &listmngr_core::MailingList,
    held: &listmngr_db::moderation::HeldMessage,
) -> ApiResult<Value> {
    let message = s.db.mail_queue().message(held.message_id).await?;
    let hold_date = chrono::DateTime::from_timestamp_millis(held.hold_date)
        .map(|value| value.to_rfc3339())
        .unwrap_or_default();
    let prefix = match s.flavor {
        ApiFlavor::Compat31 => "/3.1",
        ApiFlavor::V1 => "/api/v1",
    };
    Ok(json!({
        "hold_date": hold_date,
        "message_id": message.external_id,
        // A lossy UTF-8 preview of the exact stored bytes; the database
        // (`mail_queue().message`) keeps the authoritative exact bytes.
        "msg": String::from_utf8_lossy(&message.raw),
        "reason": held.reason,
        "request_id": held.id.0.to_string(),
        "self_link": format!("{prefix}/lists/{}/held/{}", list.id, held.id.0),
        "sender": held.sender,
        "subject": held.subject,
        "type": "held_message",
    }))
}

fn parse_held_id(value: &str) -> ApiResult<listmngr_db::moderation::HeldId> {
    value
        .parse::<uuid::Uuid>()
        .map(listmngr_db::moderation::HeldId)
        .map_err(|_| ApiError(Error::Validation("held message id".into())))
}

#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/held",
    params(PageQuery, ("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = HeldMessagePageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_held(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "moderation", &id).await?;
    let list = s.db.lists().get(&id).await?;
    let total = s.db.moderation().count_pending(&id).await?;
    let (start, end) = page_window(total, &page_query)?;
    let held = if start == end {
        Vec::new()
    } else {
        s.db.moderation()
            .pending_page(&id, start, end - start)
            .await?
    };
    let mut entries = Vec::with_capacity(held.len());
    for item in &held {
        entries.push(held_entry_value(&s, &list, item).await?);
    }
    Ok(Json(page_response(s.flavor, &entries, start, total)))
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/held/count",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = CountResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_held_count(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "moderation", &id).await?;
    let count = s.db.moderation().count_pending(&id).await?;
    Ok(Json(json!({ "count": count })))
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/held/{held_id}",
    params(("id" = String, Path, description = "id path parameter"), ("held_id" = String, Path, description = "held_id path parameter")),
    responses((status = 200, description = "Successful operation", body = HeldMessageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_held_get(
    State(s): State<AppState>,
    Path((id, held_id)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "moderation", &id).await?;
    let held = s.db.moderation().get(parse_held_id(&held_id)?).await?;
    if held.list_id != id {
        return Err(ApiError(Error::NotFound("held message".into())));
    }
    let list = s.db.lists().get(&id).await?;
    Ok(Json(held_entry_value(&s, &list, &held).await?))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct ModerateInput {
    action: String,
    comment: Option<String>,
    /// Mailman's `forward`: also send a copy of the held post to `forward_to`.
    #[serde(default, deserialize_with = "mailman_bool")]
    forward: bool,
    /// Where the copy goes; required when `forward` is true.
    #[serde(default)]
    forward_to: Option<String>,
}
#[utoipa::path(
    post,
    path = "/api/v1/lists/{id}/held/{held_id}",
    params(("id" = String, Path, description = "id path parameter"), ("held_id" = String, Path, description = "held_id path parameter")),
    request_body(content((ModerateInput = "application/json"), (ModerateInput = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Deleted"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_held_moderate(
    State(s): State<AppState>,
    Path((id, held_id)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<ModerateInput>,
) -> ApiResult<StatusCode> {
    use listmngr_db::moderation::ReviewAction;
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "moderation", &id).await?;
    let held_id = parse_held_id(&held_id)?;
    let held = s.db.moderation().get(held_id).await?;
    if held.list_id != id {
        return Err(ApiError(Error::NotFound("held message".into())));
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    let reason = v.comment.unwrap_or_default();
    let action = match v.action.as_str() {
        "accept" => ReviewAction::Accept {
            max_attempts: HELD_OUT_MAX_ATTEMPTS,
        },
        "reject" => ReviewAction::Reject,
        "discard" => ReviewAction::Discard,
        "defer" => ReviewAction::Defer,
        other => {
            return Err(ApiError(Error::Validation(format!(
                "moderation action {other:?} is not implemented"
            ))));
        }
    };
    let forward_to = match (v.forward, v.forward_to.as_deref()) {
        (false, _) => None,
        (true, Some(address)) if !address.trim().is_empty() => Some(address),
        (true, _) => {
            return Err(ApiError(Error::Validation(
                "forward requires forward_to".into(),
            )));
        }
    };
    s.db.moderation()
        .review_forwarding(
            held_id,
            &audit_context(&auth, addr),
            &action,
            &reason,
            forward_to,
            now_ms,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    get,
    path = "/api/v1/members/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Member), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn members_get(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    let (_, member) = authorize_member(&s, &h, peer(c), "members:read", id).await?;
    Ok(Json(member_value(&s, &member).await?))
}
#[utoipa::path(
    delete,
    path = "/api/v1/members/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 204, description = "Deleted"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn members_delete(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let id = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    let addr = peer(c);
    let (auth, _) = authorize_member(&s, &h, addr, "members:write", id).await?;
    s.db.members()
        .delete_with_context(id, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    patch,
    path = "/api/v1/members/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content((MemberPatchInput = "application/json"), (MemberPatchInput = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Member), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn member_patch(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    let id: MemberId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    let addr = peer(c);
    let (auth, _) = authorize_member(&s, &h, addr, "members:write", id).await?;
    let member =
        s.db.members()
            .update_with_context(id, &v, &audit_context(&auth, addr))
            .await?;
    let preferences = s.db.preferences().get(member.preferences_id).await?;
    let mut value = serde_json::to_value(member).expect("serialize");
    let preference_value = serde_json::to_value(preferences).expect("serialize");
    if let (Some(target), Some(source)) = (value.as_object_mut(), preference_value.as_object()) {
        target.extend(source.clone());
    }
    Ok(Json(value))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct FindInput {
    subscriber: Option<String>,
    substring: Option<String>,
    list_id: Option<ListId>,
    role: Option<MemberRole>,
}
#[utoipa::path(
    post,
    path = "/api/v1/members/find",
    params(PageQuery),
    request_body(content((FindInput = "application/json"), (FindInput = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = MemberPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn members_find(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<FindInput>,
) -> ApiResult<Json<Value>> {
    let auth = authenticate_for_authorization(&s, &h, peer(c), "members:read").await?;
    let found = match (&v.subscriber, &v.substring) {
        (Some(subscriber), None) if !subscriber.trim().is_empty() => {
            s.db.members().find(subscriber).await?
        }
        (None, Some(substring)) if !substring.trim().is_empty() => {
            s.db.members().find_substring(substring).await?
        }
        _ => {
            return Err(ApiError(Error::Validation(
                "provide exactly one of subscriber or substring".into(),
            )));
        }
    };
    let members = filter_members_for_auth(&s, &auth, found)
        .await?
        .into_iter()
        .filter(|member| {
            v.list_id
                .as_ref()
                .is_none_or(|list| &member.list_id == list)
        })
        .filter(|member| v.role.is_none_or(|role| member.role == role))
        .collect::<Vec<_>>();
    finish_authorization(&s, auth).await?;
    let mut entries = Vec::with_capacity(members.len());
    for member in members {
        entries.push(member_value(&s, &member).await?);
    }
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/members",
    params(PageQuery),
    responses((status = 200, description = "Successful operation", body = MemberPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
/// Every membership the caller may see, by list and then in subscription
/// order — Mailman's `client.members`.
async fn members_list(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let auth = authenticate_for_authorization(&s, &h, peer(c), "members:read").await?;
    let members = filter_members_for_auth(&s, &auth, s.db.members().all().await?).await?;
    finish_authorization(&s, auth).await?;
    let mut entries = Vec::with_capacity(members.len());
    for member in members {
        entries.push(member_value(&s, &member).await?);
    }
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/members/{id}/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn member_preferences(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id: MemberId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    let (_, member) = authorize_member(&s, &h, peer(c), "members:read", id).await?;
    Ok(Json(
        serde_json::to_value(s.db.preferences().get(member.preferences_id).await?)
            .expect("serialize"),
    ))
}
#[utoipa::path(
    get,
    path = "/api/v1/members/{id}/all/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn member_all_preferences(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id: MemberId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    authorize_member(&s, &h, peer(c), "members:read", id).await?;
    Ok(Json(
        serde_json::to_value(
            s.db.preferences()
                .resolve_member(id, &s.config.site.default_language)
                .await?,
        )
        .expect("serialize"),
    ))
}
/// A preference boolean as JSON or as the `True`/`False` string a form
/// carries.
fn preference_bool(key: &str, value: &Value) -> Result<bool, Error> {
    match value {
        Value::Bool(flag) => Ok(*flag),
        Value::String(text) => match text.to_ascii_lowercase().as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(Error::Validation(key.to_owned())),
        },
        _ => Err(Error::Validation(key.to_owned())),
    }
}

fn preferences_update(
    current: Preferences,
    value: &Value,
    replace: bool,
) -> Result<Preferences, Error> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::Validation("preferences must be an object".into()))?;
    let mut result = if replace {
        Preferences::default()
    } else {
        current
    };
    for (key, value) in object {
        match key.as_str() {
            "acknowledge_posts" => {
                result.acknowledge_posts = if value.is_null() {
                    None
                } else {
                    Some(preference_bool(key, value)?)
                };
            }
            "hide_address" => {
                result.hide_address = if value.is_null() {
                    None
                } else {
                    Some(preference_bool(key, value)?)
                };
            }
            "preferred_language" => {
                result.preferred_language = if value.is_null() {
                    None
                } else {
                    Some(
                        value
                            .as_str()
                            .filter(|language| !language.trim().is_empty())
                            .ok_or_else(|| Error::Validation(key.clone()))?
                            .to_owned(),
                    )
                };
            }
            "receive_list_copy" => {
                result.receive_list_copy = if value.is_null() {
                    None
                } else {
                    Some(preference_bool(key, value)?)
                };
            }
            "receive_own_postings" => {
                result.receive_own_postings = if value.is_null() {
                    None
                } else {
                    Some(preference_bool(key, value)?)
                };
            }
            "delivery_mode" => {
                result.delivery_mode = if value.is_null() {
                    None
                } else {
                    Some(
                        value
                            .as_str()
                            .ok_or_else(|| Error::Validation(key.clone()))?
                            .parse::<DeliveryMode>()?,
                    )
                };
            }
            "delivery_status" => {
                result.delivery_status = if value.is_null() {
                    None
                } else {
                    Some(
                        value
                            .as_str()
                            .ok_or_else(|| Error::Validation(key.clone()))?
                            .parse::<DeliveryStatus>()?,
                    )
                };
            }
            _ => {
                return Err(Error::Validation(format!(
                    "read-only or unknown preference: {key}"
                )));
            }
        }
    }
    Ok(result)
}

#[utoipa::path(
    put,
    path = "/api/v1/members/{id}/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content((Preferences = "application/json"), (Preferences = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn member_preferences_put(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    member_preferences_write(state, path, headers, connect, body, true).await
}
#[utoipa::path(
    patch,
    path = "/api/v1/members/{id}/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content((Preferences = "application/json"), (Preferences = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn member_preferences_patch(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    member_preferences_write(state, path, headers, connect, body, false).await
}
async fn member_preferences_write(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<Value>,
    replace: bool,
) -> ApiResult<Json<Value>> {
    let id: MemberId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    let addr = peer(c);
    let (auth, member) = authorize_member(&s, &h, addr, "members:write", id).await?;
    let current = s.db.preferences().get(member.preferences_id).await?;
    let updated = preferences_update(current, &v, replace)?;
    s.db.preferences()
        .set_member_with_context(id, updated, &audit_context(&auth, addr))
        .await?;
    Ok(Json(
        serde_json::to_value(s.db.preferences().get(member.preferences_id).await?)
            .expect("serialize"),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/users",
    params(PageQuery),
    responses((status = 200, description = "Successful operation", body = UserPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_list(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let auth = authenticate_for_authorization(&s, &h, peer(c), "system:read").await?;
    let mut users = Vec::new();
    for user in s.db.users().list().await? {
        if auth_allows_user(&s, &auth, user.id).await? {
            users.push(user_value(s.flavor, &user));
        }
    }
    finish_authorization(&s, auth).await?;
    Ok(Json(paged(s.flavor, users, &page_query)?))
}
#[utoipa::path(
    post,
    path = "/api/v1/users",
    request_body(content((listmngr_db::NewUser = "application/json"), (listmngr_db::NewUser = "application/x-www-form-urlencoded"))),
    responses((status = 201, description = "Successful operation", body = listmngr_core::User), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_create(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<NewUser>,
) -> ApiResult<Response> {
    let addr = peer(c);
    let auth = authorize_admin(&s, &h, addr, "users:write").await?;
    let user =
        s.db.users()
            .create_with_context(v, &audit_context(&auth, addr))
            .await?;
    let value = user_value(s.flavor, &user);
    Ok(match s.flavor {
        ApiFlavor::Compat31 => (
            StatusCode::CREATED,
            [(header::LOCATION, format!("/3.1/users/{}", user.id))],
            Json(value),
        )
            .into_response(),
        ApiFlavor::V1 => (StatusCode::CREATED, Json(value)).into_response(),
    })
}
#[utoipa::path(
    get,
    path = "/api/v1/users/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = listmngr_core::User), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_get(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = resolve_user_id(&s, &id).await?;
    authorize_user(&s, &h, peer(c), "system:read", id).await?;
    Ok(Json(user_value(s.flavor, &s.db.users().get(id).await?)))
}
#[utoipa::path(
    delete,
    path = "/api/v1/users/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 204, description = "Deleted"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_delete(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let id = resolve_user_id(&s, &id).await?;
    let addr = peer(c);
    let auth = authorize_user(&s, &h, addr, "users:write", id).await?;
    s.db.users()
        .delete_with_context(id, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    patch,
    path = "/api/v1/users/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content((UserPatchInput = "application/json"), (UserPatchInput = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = listmngr_core::User), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_patch(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(mut v): JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    let id = resolve_user_id(&s, &id).await?;
    let addr = peer(c);
    let auth = authorize_user(&s, &h, addr, "users:write", id).await?;
    let context = audit_context(&auth, addr);
    // `mailmanclient` saves its writable properties on a form:
    // `cleartext_password` sets the password, and booleans arrive as
    // `True`/`False` strings.
    if let Some(object) = v.as_object_mut() {
        if let Some(password) = object.remove("cleartext_password") {
            let password = password
                .as_str()
                .ok_or_else(|| ApiError(Error::Validation("cleartext_password".into())))?;
            s.db.users()
                .set_password_with_context(id, password, &context)
                .await?;
        }
        if let Some(Value::String(flag)) = object.get("is_server_owner") {
            let flag = match flag.to_ascii_lowercase().as_str() {
                "true" => true,
                "false" => false,
                _ => return Err(ApiError(Error::Validation("is_server_owner".into()))),
            };
            object.insert("is_server_owner".into(), json!(flag));
        }
        if object.is_empty() {
            return Ok(Json(user_value(s.flavor, &s.db.users().get(id).await?)));
        }
    }
    Ok(Json(user_value(
        s.flavor,
        &s.db.users().update_with_context(id, &v, &context).await?,
    )))
}
#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_preferences(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id: UserId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    authorize_user(&s, &h, peer(c), "system:read", id).await?;
    Ok(Json(
        serde_json::to_value(s.db.preferences().get_user(id).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/all/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_all_preferences(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id: UserId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    authorize_user(&s, &h, peer(c), "system:read", id).await?;
    Ok(Json(
        serde_json::to_value(
            s.db.preferences()
                .resolve_user(id, &s.config.site.default_language)
                .await?,
        )
        .expect("serialize"),
    ))
}
#[utoipa::path(
    put,
    path = "/api/v1/users/{id}/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content((Preferences = "application/json"), (Preferences = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_preferences_put(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    user_preferences_write(state, path, headers, connect, body, true).await
}
#[utoipa::path(
    patch,
    path = "/api/v1/users/{id}/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content((Preferences = "application/json"), (Preferences = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_preferences_patch(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    user_preferences_write(state, path, headers, connect, body, false).await
}
async fn user_preferences_write(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<Value>,
    replace: bool,
) -> ApiResult<Json<Value>> {
    let id = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    let addr = peer(c);
    let auth = authorize_user(&s, &h, addr, "users:write", id).await?;
    let updated = preferences_update(s.db.preferences().get_user(id).await?, &v, replace)?;
    s.db.preferences()
        .set_user_with_context(id, updated.clone(), &audit_context(&auth, addr))
        .await?;
    Ok(Json(serde_json::to_value(updated).expect("serialize")))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct LoginInput {
    password: String,
}
#[utoipa::path(
    post,
    path = "/api/v1/users/{id}/login",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content = LoginInput, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = LoginResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_login(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<LoginInput>,
) -> ApiResult<Json<Value>> {
    let id = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    authorize_user(&s, &h, peer(c), "users:write", id).await?;
    let valid = s.db.users().verify_password(id, &v.password).await?;
    Ok(Json(json!({"success":valid})))
}
#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/addresses",
    params(PageQuery, ("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = AddressPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_addresses(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = resolve_user_id(&s, &id).await?;
    let auth = authenticate_for_authorization(&s, &h, peer(c), "system:read").await?;
    if !auth_allows_user(&s, &auth, id).await? {
        return Err(ApiError(Error::Forbidden("system:read".into())));
    }
    s.db.users().get(id).await?;
    let mut entries = Vec::new();
    for address in s.db.addresses().by_user(id).await? {
        if auth_allows_address(&s, &auth, &address.email).await? {
            entries.push(address_value(s.flavor, &address));
        }
    }
    finish_authorization(&s, auth).await?;
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct LinkInput {
    email: String,
    /// Mailman's `absorb_existing`: take the address from another account
    /// (`1`, `true` or `True` on a form).
    #[serde(default)]
    absorb_existing: Option<Value>,
}

impl LinkInput {
    fn absorbs(&self) -> bool {
        match &self.absorb_existing {
            Some(Value::Bool(flag)) => *flag,
            Some(Value::Number(number)) => number.as_i64() == Some(1),
            Some(Value::String(text)) => matches!(text.to_ascii_lowercase().as_str(), "1" | "true"),
            _ => false,
        }
    }
}
#[utoipa::path(
    post,
    path = "/api/v1/users/{id}/addresses",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content((LinkInput = "application/json"), (LinkInput = "application/x-www-form-urlencoded"))),
    responses((status = 201, description = "Successful operation", body = listmngr_core::Address), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_address_link(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<LinkInput>,
) -> ApiResult<Response> {
    let id = resolve_user_id(&s, &id).await?;
    let addr = peer(c);
    let auth = authorize_user_and_address(&s, &h, addr, "users:write", id, &v.email).await?;
    let context = audit_context(&auth, addr);
    let address = match s.flavor {
        // Mailman creates an unknown address for the account and adopts
        // one nobody owns; the native flavour keeps its relink semantics.
        ApiFlavor::Compat31 => {
            s.db.addresses()
                .add_to_user_with_context(&v.email, id, v.absorbs(), &context)
                .await?
        }
        ApiFlavor::V1 => {
            s.db.addresses()
                .link_with_context(&v.email, Some(id), &context)
                .await?
        }
    };
    let value = address_value(s.flavor, &address);
    Ok(match s.flavor {
        ApiFlavor::Compat31 => (
            StatusCode::CREATED,
            [(
                header::LOCATION,
                format!("/3.1/addresses/{}", address.email),
            )],
            Json(value),
        )
            .into_response(),
        ApiFlavor::V1 => Json(value).into_response(),
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/preferred_address",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Address), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
/// The account's preferred address; 404 while none is set, as Mailman.
async fn user_preferred_address(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = resolve_user_id(&s, &id).await?;
    authorize_user(&s, &h, peer(c), "system:read", id).await?;
    let user = s.db.users().get(id).await?;
    let preferred = user
        .preferred_address_id
        .ok_or_else(|| ApiError(Error::NotFound("preferred address".into())))?;
    Ok(Json(address_value(
        s.flavor,
        &s.db.addresses().get_by_id(preferred).await?,
    )))
}

#[utoipa::path(
    post,
    path = "/api/v1/users/{id}/preferred_address",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content((LinkInput = "application/json"), (LinkInput = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Successful operation"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_preferred_address_set(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<LinkInput>,
) -> ApiResult<StatusCode> {
    let id = resolve_user_id(&s, &id).await?;
    let addr = peer(c);
    let auth = authorize_user_and_address(&s, &h, addr, "users:write", id, &v.email).await?;
    s.db.users()
        .set_preferred_address_with_context(id, Some(&v.email), &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/v1/users/{id}/preferred_address",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 204, description = "Successful operation"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_preferred_address_unset(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let id = resolve_user_id(&s, &id).await?;
    let addr = peer(c);
    let auth = authorize_user(&s, &h, addr, "users:write", id).await?;
    s.db.users()
        .set_preferred_address_with_context(id, None, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/v1/addresses/{email}",
    params(("email" = String, Path, description = "email path parameter")),
    responses((status = 204, description = "Successful operation"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
/// Mailman's address deletion; an address with memberships is a conflict.
async fn address_delete(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let addr = peer(c);
    let auth = authorize_address(&s, &h, addr, "users:write", &email).await?;
    s.db.addresses()
        .delete_with_context(&email, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/addresses/{email}",
    params(("email" = String, Path, description = "email path parameter")),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Address), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_get(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_address(&s, &h, peer(c), "system:read", &email).await?;
    Ok(Json(address_value(
        s.flavor,
        &s.db.addresses().get(&email).await?,
    )))
}
#[utoipa::path(
    post,
    path = "/api/v1/addresses/{email}/verify",
    params(("email" = String, Path, description = "email path parameter")),
    request_body(content = EmptyMutationInput, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Address), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_verify(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    _body: Option<Json<EmptyMutationInput>>,
) -> ApiResult<Json<Value>> {
    let addr = peer(c);
    let auth = authorize_address(&s, &h, addr, "users:write", &email).await?;
    Ok(Json(
        serde_json::to_value(
            s.db.addresses()
                .verify_with_context(&email, true, &audit_context(&auth, addr))
                .await?,
        )
        .expect("serialize"),
    ))
}
#[utoipa::path(
    post,
    path = "/api/v1/addresses/{email}/unverify",
    params(("email" = String, Path, description = "email path parameter")),
    request_body(content = EmptyMutationInput, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Address), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_unverify(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    _body: Option<Json<EmptyMutationInput>>,
) -> ApiResult<Json<Value>> {
    let addr = peer(c);
    let auth = authorize_address(&s, &h, addr, "users:write", &email).await?;
    Ok(Json(
        serde_json::to_value(
            s.db.addresses()
                .verify_with_context(&email, false, &audit_context(&auth, addr))
                .await?,
        )
        .expect("serialize"),
    ))
}
#[utoipa::path(
    get,
    path = "/api/v1/addresses/{email}/user",
    params(("email" = String, Path, description = "email path parameter")),
    responses((status = 200, description = "Successful operation", body = AddressUserResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_user(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_address(&s, &h, peer(c), "system:read", &email).await?;
    let address = s.db.addresses().get(&email).await?;
    Ok(Json(json!({"user_id":address.user_id})))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct UserLink {
    user_id: UserId,
}
#[utoipa::path(
    post,
    path = "/api/v1/addresses/{email}/user",
    params(("email" = String, Path, description = "email path parameter")),
    request_body(content = UserLink, content_type = "application/json"),
    responses((status = 201, description = "Successful operation", body = listmngr_core::Address), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_link(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<UserLink>,
) -> ApiResult<Json<Value>> {
    let addr = peer(c);
    let auth = authorize_user_and_address(&s, &h, addr, "users:write", v.user_id, &email).await?;
    Ok(Json(
        serde_json::to_value(
            s.db.addresses()
                .link_with_context(&email, Some(v.user_id), &audit_context(&auth, addr))
                .await?,
        )
        .expect("serialize"),
    ))
}
#[utoipa::path(
    delete,
    path = "/api/v1/addresses/{email}/user",
    params(("email" = String, Path, description = "email path parameter")),
    responses((status = 204, description = "Deleted"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_unlink(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let addr = peer(c);
    let auth = authorize_address(&s, &h, addr, "users:write", &email).await?;
    s.db.addresses()
        .link_with_context(&email, None, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    get,
    path = "/api/v1/addresses/{email}/memberships",
    params(PageQuery, ("email" = String, Path, description = "email path parameter")),
    responses((status = 200, description = "Successful operation", body = MemberPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_memberships(
    State(s): State<AppState>,
    Path(email): Path<String>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let auth = authorize_address(&s, &h, peer(c), "members:read", &email).await?;
    let members = filter_members_for_auth(&s, &auth, s.db.members().find(&email).await?).await?;
    let mut entries = Vec::with_capacity(members.len());
    for member in members {
        entries.push(member_value(&s, &member).await?);
    }
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/addresses/{email}/preferences",
    params(("email" = String, Path, description = "email path parameter")),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_preferences(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_address(&s, &h, peer(c), "system:read", &email).await?;
    Ok(Json(
        serde_json::to_value(s.db.preferences().get_address(&email).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    get,
    path = "/api/v1/addresses/{email}/all/preferences",
    params(("email" = String, Path, description = "email path parameter")),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_all_preferences(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_address(&s, &h, peer(c), "system:read", &email).await?;
    Ok(Json(
        serde_json::to_value(
            s.db.preferences()
                .resolve_address(&email, &s.config.site.default_language)
                .await?,
        )
        .expect("serialize"),
    ))
}
#[utoipa::path(
    put,
    path = "/api/v1/addresses/{email}/preferences",
    params(("email" = String, Path, description = "email path parameter")),
    request_body(content((Preferences = "application/json"), (Preferences = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_preferences_put(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    address_preferences_write(state, path, headers, connect, body, true).await
}
#[utoipa::path(
    patch,
    path = "/api/v1/addresses/{email}/preferences",
    params(("email" = String, Path, description = "email path parameter")),
    request_body(content((Preferences = "application/json"), (Preferences = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_preferences_patch(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    address_preferences_write(state, path, headers, connect, body, false).await
}
async fn address_preferences_write(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<Value>,
    replace: bool,
) -> ApiResult<Json<Value>> {
    let addr = peer(c);
    let auth = authorize_address(&s, &h, addr, "users:write", &email).await?;
    let updated = preferences_update(s.db.preferences().get_address(&email).await?, &v, replace)?;
    s.db.preferences()
        .set_address_with_context(&email, updated.clone(), &audit_context(&auth, addr))
        .await?;
    Ok(Json(serde_json::to_value(updated).expect("serialize")))
}
#[utoipa::path(
    get,
    path = "/api/v1/owners",
    params(PageQuery),
    responses((status = 200, description = "Successful operation", body = UserPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn owners(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_admin(&s, &h, peer(c), "system:read").await?;
    let owners =
        s.db.users()
            .list()
            .await?
            .into_iter()
            .filter(|u| u.is_server_owner)
            .collect::<Vec<_>>();
    Ok(Json(paged(s.flavor, owners, &page_query)?))
}
