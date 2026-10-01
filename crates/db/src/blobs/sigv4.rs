//! AWS Signature Version 4 for S3.
//!
//! The `s3` service, `UNSIGNED-PAYLOAD` never used: every request here
//! carries the `SHA-256` of its whole body, as "Signature Calculations
//! for the Authorization Header" describes it, checked against the
//! worked examples of that document.
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac as _};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use sha2::{Digest as _, Sha256};

/// What `SigV4` leaves unencoded: the unreserved characters of RFC 3986.
const ENCODE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// The `SHA-256` of the empty payload.
pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The credential a request is signed with; the secret never appears in
/// `Debug`.
#[derive(Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .finish_non_exhaustive()
    }
}

/// Hex `SHA-256`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// A path's segments, each URI-encoded once (S3 does not double-encode),
/// the separators kept.
#[must_use]
pub fn canonical_uri(path: &str) -> String {
    let encoded: Vec<String> = path
        .split('/')
        .map(|segment| utf8_percent_encode(segment, ENCODE).to_string())
        .collect();
    let joined = encoded.join("/");
    if joined.starts_with('/') {
        joined
    } else {
        format!("/{joined}")
    }
}

/// `key=value` pairs, each side URI-encoded, sorted by key then value.
#[must_use]
pub fn canonical_query(query: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(key, value)| {
            (
                utf8_percent_encode(key, ENCODE).to_string(),
                utf8_percent_encode(value, ENCODE).to_string(),
            )
        })
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// The signed request: what goes into `Authorization`, and the two
/// intermediate values the AWS examples print (for tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    pub authorization: String,
    pub canonical_request: String,
    pub string_to_sign: String,
}

/// Sign `method path?query` for `region` at `at`.
///
/// Every header in `headers` is signed; `host`, `x-amz-content-sha256`
/// and `x-amz-date` must be among them, names in any case. The payload
/// is given as its hex `SHA-256`, `payload_hash`.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn sign(
    credentials: &Credentials,
    region: &str,
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    headers: &[(&str, &str)],
    payload_hash: &str,
    at: DateTime<Utc>,
) -> Signed {
    let mut canonical_headers: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    canonical_headers.sort();
    let signed_headers = canonical_headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let mut canonical_request = format!(
        "{method}\n{}\n{}\n",
        canonical_uri(path),
        canonical_query(query)
    );
    for (name, value) in &canonical_headers {
        canonical_request.push_str(name);
        canonical_request.push(':');
        canonical_request.push_str(value);
        canonical_request.push('\n');
    }
    canonical_request.push('\n');
    canonical_request.push_str(&signed_headers);
    canonical_request.push('\n');
    canonical_request.push_str(payload_hash);
    let date = at.format("%Y%m%d").to_string();
    let timestamp = at.format("%Y%m%dT%H%M%SZ").to_string();
    let scope = format!("{date}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let k_date = hmac(
        format!("AWS4{}", credentials.secret_access_key).as_bytes(),
        date.as_bytes(),
    );
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, b"s3");
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));
    Signed {
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope},SignedHeaders={signed_headers},Signature={signature}",
            credentials.access_key_id
        ),
        canonical_request,
        string_to_sign,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Credentials {
        Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
        }
    }

    fn at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2013-05-24T00:00:00Z")
            .unwrap()
            .into()
    }

    const HOST: &str = "examplebucket.s3.amazonaws.com";

    #[test]
    fn get_object() {
        let signed = sign(
            &example(),
            "us-east-1",
            "GET",
            "/test.txt",
            &[],
            &[
                ("Host", HOST),
                ("Range", "bytes=0-9"),
                ("x-amz-content-sha256", EMPTY_SHA256),
                ("x-amz-date", "20130524T000000Z"),
            ],
            EMPTY_SHA256,
            at(),
        );
        assert_eq!(
            signed.canonical_request,
            "GET\n/test.txt\n\nhost:examplebucket.s3.amazonaws.com\nrange:bytes=0-9\nx-amz-content-sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\nx-amz-date:20130524T000000Z\n\nhost;range;x-amz-content-sha256;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            signed.string_to_sign,
            "AWS4-HMAC-SHA256\n20130524T000000Z\n20130524/us-east-1/s3/aws4_request\n7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972"
        );
        assert_eq!(
            signed.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn debug_hides_the_secret() {
        assert!(!format!("{:?}", example()).contains("wJalr"));
    }

    #[test]
    fn put_object() {
        let payload = sha256_hex(b"Welcome to Amazon S3.");
        assert_eq!(
            payload,
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
        );
        let signed = sign(
            &example(),
            "us-east-1",
            "PUT",
            "/test$file.text",
            &[],
            &[
                ("Date", "Fri, 24 May 2013 00:00:00 GMT"),
                ("Host", HOST),
                ("x-amz-content-sha256", &payload),
                ("x-amz-date", "20130524T000000Z"),
                ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
            ],
            &payload,
            at(),
        );
        assert!(
            signed
                .canonical_request
                .starts_with("PUT\n/test%24file.text\n\n")
        );
        assert!(
            signed.authorization.ends_with(
                "Signature=98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
            ),
            "{}",
            signed.authorization
        );
    }

    #[test]
    fn get_bucket_lifecycle() {
        let signed = sign(
            &example(),
            "us-east-1",
            "GET",
            "/",
            &[("lifecycle", "")],
            &[
                ("Host", HOST),
                ("x-amz-content-sha256", EMPTY_SHA256),
                ("x-amz-date", "20130524T000000Z"),
            ],
            EMPTY_SHA256,
            at(),
        );
        assert!(signed.canonical_request.starts_with("GET\n/\nlifecycle=\n"));
        assert!(
            signed.authorization.ends_with(
                "Signature=fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"
            ),
            "{}",
            signed.authorization
        );
    }

    #[test]
    fn list_objects() {
        let signed = sign(
            &example(),
            "us-east-1",
            "GET",
            "/",
            &[("prefix", "J"), ("max-keys", "2")],
            &[
                ("Host", HOST),
                ("x-amz-content-sha256", EMPTY_SHA256),
                ("x-amz-date", "20130524T000000Z"),
            ],
            EMPTY_SHA256,
            at(),
        );
        assert!(
            signed
                .canonical_request
                .starts_with("GET\n/\nmax-keys=2&prefix=J\n")
        );
        assert!(
            signed.authorization.ends_with(
                "Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
            ),
            "{}",
            signed.authorization
        );
    }
}
