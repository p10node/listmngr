//! An S3-compatible object store over `reqwest`, signed with
//! [`sigv4`](super::sigv4): `PUT`, `GET`, `HEAD`, `DELETE` of one object
//! and `ListObjectsV2` with its continuation tokens. Path-style
//! addressing (`endpoint/bucket/key`) against a configured endpoint,
//! virtual-host style (`bucket.s3.region.amazonaws.com/key`) otherwise.
use super::sigv4::{self, Credentials};
use chrono::{DateTime, Utc};
use listmngr_core::{Error, Result};
use reqwest::{Method, StatusCode};
use std::time::Duration;

/// How long one request may take, connection included.
const TIMEOUT: Duration = Duration::from_secs(30);

/// What `[message_store]` says about the bucket.
#[derive(Clone, Debug)]
pub struct S3Settings {
    pub bucket: String,
    pub region: String,
    /// `http(s)://host[:port]`, no path; `None` for AWS itself.
    pub endpoint: Option<String>,
    /// Put before every key, `messages/` for instance; may be empty.
    pub prefix: String,
    pub credentials: Credentials,
}

/// The client: the settings, the origin requests go to, the `host`
/// header they are signed with, and the path the bucket lives at.
#[derive(Debug)]
pub struct S3 {
    client: reqwest::Client,
    settings: S3Settings,
    origin: String,
    host: String,
    bucket_path: String,
}

fn storage(message: impl std::fmt::Display) -> Error {
    Error::Database(format!("message store: {message}"))
}

impl S3 {
    pub fn new(settings: S3Settings) -> Result<Self> {
        let (origin, host, bucket_path) = if let Some(endpoint) = settings.endpoint.as_deref() {
            let endpoint = endpoint.trim_end_matches('/');
            let (scheme, authority) = endpoint
                .split_once("://")
                .filter(|(scheme, authority)| {
                    matches!(*scheme, "http" | "https") && !authority.is_empty()
                })
                .ok_or_else(|| storage("s3_endpoint must be http(s)://host[:port]"))?;
            let default_port = if scheme == "https" { ":443" } else { ":80" };
            let host = authority.strip_suffix(default_port).unwrap_or(authority);
            (
                endpoint.to_owned(),
                host.to_owned(),
                format!("/{}", settings.bucket),
            )
        } else {
            let host = format!("{}.s3.{}.amazonaws.com", settings.bucket, settings.region);
            (format!("https://{host}"), host, String::new())
        };
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(storage)?;
        Ok(Self {
            client,
            settings,
            origin,
            host,
            bucket_path,
        })
    }

    /// The unencoded path of `key`'s object.
    fn object_path(&self, key: &str) -> String {
        format!("{}/{}{key}", self.bucket_path, self.settings.prefix)
    }

    /// One signed request; the body is signed whole.
    async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Result<reqwest::Response> {
        let payload = sigv4::sha256_hex(&body);
        let now = Utc::now();
        let date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let signed = sigv4::sign(
            &self.settings.credentials,
            &self.settings.region,
            method.as_str(),
            path,
            query,
            &[
                ("host", self.host.as_str()),
                ("x-amz-content-sha256", payload.as_str()),
                ("x-amz-date", date.as_str()),
            ],
            &payload,
            now,
        );
        let mut url = format!("{}{}", self.origin, sigv4::canonical_uri(path));
        if !query.is_empty() {
            url.push('?');
            url.push_str(&sigv4::canonical_query(query));
        }
        self.client
            .request(method, &url)
            .header("authorization", signed.authorization)
            .header("x-amz-content-sha256", payload)
            .header("x-amz-date", date)
            .body(body)
            .send()
            .await
            .map_err(|error| storage(format!("s3 request failed: {error}")))
    }

    /// A refusal as an error, with the service's error code when it sent
    /// one (never the whole body).
    async fn refused(what: &str, response: reqwest::Response) -> Error {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let code = body
            .split_once("<Code>")
            .and_then(|(_, rest)| rest.split_once("</Code>"))
            .map_or("", |(code, _)| code.trim());
        storage(
            format!("s3 {what} answered {status} {code}")
                .trim_end()
                .to_owned(),
        )
    }

    pub async fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        let path = self.object_path(key);
        let response = self.send(Method::PUT, &path, &[], bytes.to_vec()).await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Self::refused("PUT", response).await)
        }
    }

    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let path = self.object_path(key);
        let response = self.send(Method::GET, &path, &[], Vec::new()).await?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(None),
            status if status.is_success() => Ok(Some(
                response
                    .bytes()
                    .await
                    .map_err(|error| storage(format!("s3 GET body: {error}")))?
                    .to_vec(),
            )),
            _ => Err(Self::refused("GET", response).await),
        }
    }

    pub async fn exists(&self, key: &str) -> Result<bool> {
        let path = self.object_path(key);
        let response = self.send(Method::HEAD, &path, &[], Vec::new()).await?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(false),
            status if status.is_success() => Ok(true),
            _ => Err(Self::refused("HEAD", response).await),
        }
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        let path = self.object_path(key);
        let response = self.send(Method::DELETE, &path, &[], Vec::new()).await?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(Self::refused("DELETE", response).await)
        }
    }

    /// Every object under the prefix, as `(key without the prefix, last
    /// modified)`, page after page.
    pub async fn list(&self) -> Result<Vec<(String, DateTime<Utc>)>> {
        let path = if self.bucket_path.is_empty() {
            "/".to_owned()
        } else {
            self.bucket_path.clone()
        };
        let mut objects = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query = vec![("list-type", "2")];
            if !self.settings.prefix.is_empty() {
                query.push(("prefix", self.settings.prefix.as_str()));
            }
            if let Some(token) = &token {
                query.push(("continuation-token", token.as_str()));
            }
            let response = self.send(Method::GET, &path, &query, Vec::new()).await?;
            if !response.status().is_success() {
                return Err(Self::refused("LIST", response).await);
            }
            let xml = response
                .text()
                .await
                .map_err(|error| storage(format!("s3 LIST body: {error}")))?;
            let page = parse_list(&xml)?;
            for (key, at) in page.objects {
                if let Some(key) = key.strip_prefix(&self.settings.prefix) {
                    objects.push((key.to_owned(), at));
                }
            }
            match page.next {
                Some(next) => token = Some(next),
                None => return Ok(objects),
            }
        }
    }
}

/// One page of `ListObjectsV2`.
struct Page {
    objects: Vec<(String, DateTime<Utc>)>,
    next: Option<String>,
}

/// The text of the first `<name>…</name>` in `xml`.
fn element<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let (_, rest) = xml.split_once(open.as_str())?;
    let (text, _) = rest.split_once(close.as_str())?;
    Some(text)
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn parse_list(xml: &str) -> Result<Page> {
    let mut objects = Vec::new();
    for block in xml.split("<Contents>").skip(1) {
        let block = block.split("</Contents>").next().unwrap_or(block);
        let key =
            element(block, "Key").ok_or_else(|| storage("s3 LIST: a Contents without a Key"))?;
        let modified = element(block, "LastModified")
            .and_then(|text| DateTime::parse_from_rfc3339(text.trim()).ok())
            .map_or_else(Utc::now, |at| at.with_timezone(&Utc));
        objects.push((unescape(key.trim()), modified));
    }
    let truncated = element(xml, "IsTruncated").is_some_and(|text| text.trim() == "true");
    let next = truncated
        .then(|| element(xml, "NextContinuationToken").map(|text| unescape(text.trim())))
        .flatten();
    if truncated && next.is_none() {
        return Err(storage("s3 LIST: truncated without a continuation token"));
    }
    Ok(Page { objects, next })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(endpoint: Option<&str>) -> S3Settings {
        S3Settings {
            bucket: "mail".into(),
            region: "eu-west-1".into(),
            endpoint: endpoint.map(str::to_owned),
            prefix: "messages/".into(),
            credentials: Credentials {
                access_key_id: "id".into(),
                secret_access_key: "secret".into(),
            },
        }
    }

    #[test]
    fn hosts_and_paths() {
        let aws = S3::new(settings(None)).unwrap();
        assert_eq!(aws.host, "mail.s3.eu-west-1.amazonaws.com");
        assert_eq!(aws.origin, "https://mail.s3.eu-west-1.amazonaws.com");
        assert_eq!(aws.object_path("k"), "/messages/k");
        let own = S3::new(settings(Some("https://s3.example.invalid:443/"))).unwrap();
        assert_eq!(own.host, "s3.example.invalid");
        assert_eq!(own.origin, "https://s3.example.invalid:443");
        assert_eq!(own.object_path("k"), "/mail/messages/k");
        let port = S3::new(settings(Some("http://127.0.0.1:9000"))).unwrap();
        assert_eq!(port.host, "127.0.0.1:9000");
        assert!(S3::new(settings(Some("ftp://x"))).is_err());
        assert!(S3::new(settings(Some("https://"))).is_err());
    }

    #[test]
    fn list_pages_are_parsed() {
        let page = parse_list(
            "<ListBucketResult><IsTruncated>true</IsTruncated><NextContinuationToken>t&amp;1</NextContinuationToken><Contents><Key>messages/a</Key><LastModified>2026-10-01T00:00:00.000Z</LastModified></Contents><Contents><Key>messages/b&lt;</Key></Contents></ListBucketResult>",
        )
        .unwrap();
        assert_eq!(page.next.as_deref(), Some("t&1"));
        assert_eq!(page.objects.len(), 2);
        assert_eq!(page.objects[0].0, "messages/a");
        assert_eq!(page.objects[0].1.to_rfc3339(), "2026-10-01T00:00:00+00:00");
        assert_eq!(page.objects[1].0, "messages/b<");
        let last =
            parse_list("<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>")
                .unwrap();
        assert!(last.objects.is_empty() && last.next.is_none());
        assert!(
            parse_list("<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>")
                .is_err()
        );
        assert!(parse_list("<Contents><LastModified>x</LastModified></Contents>").is_err());
    }
}
