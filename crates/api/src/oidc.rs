//! The `OpenID` Connect relying-party client.
//!
//! Discovery, Authorization Code with PKCE, the token exchange, and ID-token
//! verification against the provider's JWKS — RS256 and ES256, the
//! algorithms the large providers sign with. Everything here is stateless;
//! the ceremony a browser is in the middle of lives on its session
//! (`listmngr_db::web_oidc`).
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use listmngr_core::{Error, OidcProviderConfig, Result};
use rand::RngCore as _;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long discovery and key sets are reused before they are fetched again.
const CACHE_TTL: Duration = Duration::from_secs(3600);
/// Largest response body accepted from a provider.
const MAX_BODY: usize = 256 * 1024;

/// What an ID token says about the person, once verified.
#[derive(Debug, Clone)]
pub struct Identity {
    /// `sub`: stable per provider.
    pub subject: String,
    /// `email`, if the provider sent one.
    pub email: Option<String>,
    /// `email_verified` as the provider asserts it; `false` when absent.
    pub email_verified: bool,
    /// `name`, if sent.
    pub name: Option<String>,
}

/// The secrets a browser's session keeps between the redirect out and the
/// callback in.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Ceremony {
    pub provider: String,
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    /// `login`, or `link` for a signed-in reader adding the provider.
    pub purpose: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
struct Discovery {
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

/// One configured provider with its cached discovery document and keys.
#[derive(Debug)]
pub struct Provider {
    pub config: OidcProviderConfig,
    discovery: Mutex<Option<(Discovery, Instant)>>,
    keys: Mutex<Option<(Value, Instant)>>,
}

/// Every configured provider, by name.
#[derive(Debug, Default)]
pub struct Providers {
    by_name: HashMap<String, Provider>,
    client: Option<reqwest::Client>,
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unavailable(what: &str) -> Error {
    Error::Validation(format!("identity provider {what} unavailable"))
}

impl Providers {
    /// # Panics
    /// Never: the HTTP client is built with static, valid settings.
    #[must_use]
    pub fn from_config(configured: &[OidcProviderConfig]) -> Self {
        if configured.is_empty() {
            return Self::default();
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .user_agent("listmngr")
            .build()
            .expect("static client settings");
        Self {
            by_name: configured
                .iter()
                .map(|config| {
                    (
                        config.name.clone(),
                        Provider {
                            config: config.clone(),
                            discovery: Mutex::new(None),
                            keys: Mutex::new(None),
                        },
                    )
                })
                .collect(),
            client: Some(client),
        }
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Provider> {
        self.by_name.get(name)
    }

    /// Providers in configuration order, for the login page.
    #[must_use]
    pub fn all(&self) -> Vec<&Provider> {
        let mut providers: Vec<&Provider> = self.by_name.values().collect();
        providers.sort_by(|a, b| a.config.name.cmp(&b.config.name));
        providers
    }

    fn client(&self) -> Result<&reqwest::Client> {
        self.client.as_ref().ok_or_else(|| unavailable("client"))
    }

    async fn fetch_json(&self, url: &str) -> Result<Value> {
        let response = self
            .client()?
            .get(url)
            .send()
            .await
            .map_err(|_| unavailable("request"))?;
        if !response.status().is_success() {
            return Err(unavailable("response"));
        }
        let bytes = response.bytes().await.map_err(|_| unavailable("body"))?;
        if bytes.len() > MAX_BODY {
            return Err(unavailable("body"));
        }
        serde_json::from_slice(&bytes).map_err(|_| unavailable("document"))
    }

    async fn discovery(&self, provider: &Provider) -> Result<Discovery> {
        if let Some((cached, at)) = provider.discovery.lock().expect("cache lock").as_ref()
            && at.elapsed() < CACHE_TTL
        {
            return Ok(cached.clone());
        }
        let url = format!(
            "{}/.well-known/openid-configuration",
            provider.config.issuer
        );
        let document = self.fetch_json(&url).await?;
        if document["issuer"].as_str().map(|s| s.trim_end_matches('/'))
            != Some(provider.config.issuer.as_str())
        {
            return Err(Error::Validation(
                "identity provider issuer mismatch".into(),
            ));
        }
        let field = |name: &str| -> Result<String> {
            document[name]
                .as_str()
                .filter(|url| {
                    url.starts_with("https://") || url.starts_with(&provider.config.issuer)
                })
                .map(ToOwned::to_owned)
                .ok_or_else(|| unavailable(name))
        };
        let discovery = Discovery {
            authorization_endpoint: field("authorization_endpoint")?,
            token_endpoint: field("token_endpoint")?,
            jwks_uri: field("jwks_uri")?,
        };
        *provider.discovery.lock().expect("cache lock") = Some((discovery.clone(), Instant::now()));
        Ok(discovery)
    }

    async fn keys(&self, provider: &Provider, refresh: bool) -> Result<Value> {
        if !refresh
            && let Some((cached, at)) = provider.keys.lock().expect("cache lock").as_ref()
            && at.elapsed() < CACHE_TTL
        {
            return Ok(cached.clone());
        }
        let discovery = self.discovery(provider).await?;
        let keys = self.fetch_json(&discovery.jwks_uri).await?;
        *provider.keys.lock().expect("cache lock") = Some((keys.clone(), Instant::now()));
        Ok(keys)
    }

    /// Start a ceremony: the secrets to keep on the session and the URL to
    /// send the browser to.
    /// # Errors
    /// The provider's discovery document could not be fetched or is not
    /// for the configured issuer.
    pub async fn begin(
        &self,
        provider: &Provider,
        redirect_uri: &str,
        purpose: &str,
        now_ms: i64,
    ) -> Result<(Ceremony, String)> {
        let discovery = self.discovery(provider).await?;
        let ceremony = Ceremony {
            provider: provider.config.name.clone(),
            state: random_token(),
            nonce: random_token(),
            verifier: format!("{}{}", random_token(), random_token()),
            purpose: purpose.to_owned(),
            expires_at: now_ms + 10 * 60 * 1000,
        };
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(ceremony.verifier.as_bytes()));
        let query = serde_urlencoded::to_string([
            ("response_type", "code"),
            ("client_id", provider.config.client_id.as_str()),
            ("redirect_uri", redirect_uri),
            ("scope", provider.config.scopes.join(" ").as_str()),
            ("state", ceremony.state.as_str()),
            ("nonce", ceremony.nonce.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ])
        .map_err(|_| unavailable("request"))?;
        let separator = if discovery.authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        Ok((
            ceremony,
            format!("{}{separator}{query}", discovery.authorization_endpoint),
        ))
    }

    /// Exchange the code for tokens and verify the ID token against the
    /// ceremony's nonce.
    /// # Errors
    /// A failed exchange, a token that does not verify, or one for another
    /// issuer, audience, time or nonce.
    pub async fn complete(
        &self,
        provider: &Provider,
        ceremony: &Ceremony,
        code: &str,
        redirect_uri: &str,
        now_seconds: i64,
    ) -> Result<Identity> {
        let discovery = self.discovery(provider).await?;
        let secret = provider
            .config
            .client_secret
            .as_ref()
            .map(listmngr_core::SmtpAuthSecret::expose)
            .ok_or_else(|| unavailable("credentials"))?;
        let response = self
            .client()?
            .post(&discovery.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("client_id", provider.config.client_id.as_str()),
                ("client_secret", secret),
                ("code_verifier", ceremony.verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|_| unavailable("token endpoint"))?;
        if !response.status().is_success() {
            return Err(Error::Authentication);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| unavailable("token response"))?;
        if bytes.len() > MAX_BODY {
            return Err(unavailable("token response"));
        }
        let tokens: Value =
            serde_json::from_slice(&bytes).map_err(|_| unavailable("token response"))?;
        let id_token = tokens["id_token"].as_str().ok_or(Error::Authentication)?;
        self.verify_id_token(provider, id_token, &ceremony.nonce, now_seconds)
            .await
    }

    async fn verify_id_token(
        &self,
        provider: &Provider,
        token: &str,
        nonce: &str,
        now_seconds: i64,
    ) -> Result<Identity> {
        let mut parts = token.split('.');
        let (Some(header), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(Error::Authentication);
        };
        let header_json: Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(header)
                .map_err(|_| Error::Authentication)?,
        )
        .map_err(|_| Error::Authentication)?;
        let alg = header_json["alg"].as_str().unwrap_or_default();
        let kid = header_json["kid"].as_str();
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| Error::Authentication)?;
        let signing_input = format!("{header}.{payload}");
        // An unknown key id fetches the set once more: providers rotate keys.
        let mut keys = self.keys(provider, false).await?;
        if find_key(&keys, kid, alg).is_none() {
            keys = self.keys(provider, true).await?;
        }
        let key = find_key(&keys, kid, alg).ok_or(Error::Authentication)?;
        verify_signature(alg, key, signing_input.as_bytes(), &signature)?;
        let claims: Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(payload)
                .map_err(|_| Error::Authentication)?,
        )
        .map_err(|_| Error::Authentication)?;
        let issuer_ok = claims["iss"].as_str().map(|s| s.trim_end_matches('/'))
            == Some(provider.config.issuer.as_str());
        let audience_ok = match &claims["aud"] {
            Value::String(aud) => aud == &provider.config.client_id,
            Value::Array(auds) => auds.iter().any(|aud| aud == &provider.config.client_id),
            _ => false,
        };
        let expires = claims["exp"].as_i64().unwrap_or(0);
        let issued = claims["iat"].as_i64().unwrap_or(i64::MAX);
        let nonce_ok = claims["nonce"].as_str().is_some_and(|presented| {
            use subtle::ConstantTimeEq as _;
            bool::from(presented.as_bytes().ct_eq(nonce.as_bytes()))
        });
        if !issuer_ok
            || !audience_ok
            || !nonce_ok
            || expires <= now_seconds
            || issued > now_seconds + 300
        {
            return Err(Error::Authentication);
        }
        let subject = claims["sub"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 255)
            .ok_or(Error::Authentication)?
            .to_owned();
        Ok(Identity {
            subject,
            email: claims["email"].as_str().map(ToOwned::to_owned),
            email_verified: claims["email_verified"] == Value::Bool(true),
            name: claims["name"].as_str().map(ToOwned::to_owned),
        })
    }
}

fn find_key<'a>(keys: &'a Value, kid: Option<&str>, alg: &str) -> Option<&'a Value> {
    let set = keys["keys"].as_array()?;
    let wanted_kty = match alg {
        "RS256" => "RSA",
        "ES256" => "EC",
        _ => return None,
    };
    set.iter().find(|key| {
        key["kty"].as_str() == Some(wanted_kty)
            && key["use"].as_str().is_none_or(|purpose| purpose == "sig")
            && key["alg"].as_str().is_none_or(|key_alg| key_alg == alg)
            && (kid.is_none() || key["kid"].as_str() == kid)
    })
}

/// Verify `signature` over `message` with the JWK `key`, for `alg`.
/// # Errors
/// Authentication for an unsupported algorithm, a malformed key or a
/// signature that does not verify.
pub fn verify_signature(alg: &str, key: &Value, message: &[u8], signature: &[u8]) -> Result<()> {
    let field = |name: &str| -> Result<Vec<u8>> {
        key[name]
            .as_str()
            .and_then(|value| URL_SAFE_NO_PAD.decode(value).ok())
            .ok_or(Error::Authentication)
    };
    match alg {
        "RS256" => {
            use rsa::signature::Verifier as _;
            let n = rsa::BigUint::from_bytes_be(&field("n")?);
            let e = rsa::BigUint::from_bytes_be(&field("e")?);
            let key = rsa::RsaPublicKey::new(n, e).map_err(|_| Error::Authentication)?;
            let verifier = rsa::pkcs1v15::VerifyingKey::<Sha256>::new(key);
            let signature =
                rsa::pkcs1v15::Signature::try_from(signature).map_err(|_| Error::Authentication)?;
            verifier
                .verify(message, &signature)
                .map_err(|_| Error::Authentication)
        }
        "ES256" => {
            use p256::ecdsa::signature::Verifier as _;
            let x = field("x")?;
            let y = field("y")?;
            if x.len() != 32 || y.len() != 32 {
                return Err(Error::Authentication);
            }
            let point = p256::EncodedPoint::from_affine_coordinates(
                p256::FieldBytes::from_slice(&x),
                p256::FieldBytes::from_slice(&y),
                false,
            );
            let key = p256::ecdsa::VerifyingKey::from_encoded_point(&point)
                .map_err(|_| Error::Authentication)?;
            let signature =
                p256::ecdsa::Signature::from_slice(signature).map_err(|_| Error::Authentication)?;
            key.verify(message, &signature)
                .map_err(|_| Error::Authentication)
        }
        _ => Err(Error::Authentication),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rs256_and_es256_signatures_verify_and_tampering_fails() {
        use p256::ecdsa::signature::Signer as _;
        use rsa::signature::SignatureEncoding as _;
        use rsa::traits::PublicKeyParts as _;
        let message = b"header.payload";
        // ES256
        let es_key = p256::ecdsa::SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
        let point = es_key.verifying_key().to_encoded_point(false);
        let es_jwk = serde_json::json!({
            "kty": "EC", "crv": "P-256",
            "x": URL_SAFE_NO_PAD.encode(point.x().unwrap()),
            "y": URL_SAFE_NO_PAD.encode(point.y().unwrap()),
        });
        let es_sig: p256::ecdsa::Signature = es_key.sign(message);
        assert!(verify_signature("ES256", &es_jwk, message, &es_sig.to_bytes()).is_ok());
        assert!(verify_signature("ES256", &es_jwk, b"other", &es_sig.to_bytes()).is_err());
        // RS256
        let rs_key =
            rsa::RsaPrivateKey::new(&mut p256::elliptic_curve::rand_core::OsRng, 2048).unwrap();
        let public = rs_key.to_public_key();
        let rs_jwk = serde_json::json!({
            "kty": "RSA",
            "n": URL_SAFE_NO_PAD.encode(public.n().to_bytes_be()),
            "e": URL_SAFE_NO_PAD.encode(public.e().to_bytes_be()),
        });
        let signer = rsa::pkcs1v15::SigningKey::<Sha256>::new(rs_key);
        let rs_sig: rsa::pkcs1v15::Signature = signer.sign(message);
        assert!(verify_signature("RS256", &rs_jwk, message, &rs_sig.to_bytes()).is_ok());
        assert!(verify_signature("RS256", &rs_jwk, b"other", &rs_sig.to_bytes()).is_err());
        assert!(verify_signature("HS256", &rs_jwk, message, &rs_sig.to_bytes()).is_err());
    }
}
