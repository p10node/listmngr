//! A minimal `OpenID` Connect provider for tests: discovery, an authorization
//! endpoint that redirects straight back with a code, a token endpoint that
//! checks PKCE and mints an ES256 ID token, and a JWKS endpoint. Which
//! identity it asserts is set by the test.
use axum::{
    Router,
    extract::{Query, State},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::{SigningKey, signature::Signer};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The identity the provider will assert on the next login.
#[derive(Debug, Clone)]
pub struct MockIdentity {
    pub subject: String,
    pub email: String,
    pub email_verified: bool,
    pub name: String,
}

/// What an authorization code was issued for: nonce, PKCE challenge, redirect.
type PendingCode = (String, String, String);

#[derive(Clone)]
struct Shared {
    issuer: String,
    client_id: String,
    client_secret: String,
    key: SigningKey,
    identity: Arc<Mutex<MockIdentity>>,
    codes: Arc<Mutex<HashMap<String, PendingCode>>>,
    /// Every ID token issued, for assertions about what was signed.
    issued: Arc<Mutex<Vec<String>>>,
}

pub struct MockProvider {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    identity: Arc<Mutex<MockIdentity>>,
    issued: Arc<Mutex<Vec<String>>>,
}

impl MockProvider {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let identity = Arc::new(Mutex::new(MockIdentity {
            subject: "subject-1".into(),
            email: "person@example.net".into(),
            email_verified: true,
            name: "Người Mới".into(),
        }));
        let tokens = Arc::new(Mutex::new(Vec::new()));
        let shared = Shared {
            issuer: issuer.clone(),
            client_id: "listmngr-test".into(),
            client_secret: "s3cret-for-tests".into(),
            key: SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng),
            identity: identity.clone(),
            codes: Arc::new(Mutex::new(HashMap::new())),
            issued: tokens.clone(),
        };
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/authorize", get(authorize))
            .route("/token", post(token))
            .route("/jwks", get(jwks))
            .with_state(shared);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            client_id: "listmngr-test".into(),
            client_secret: "s3cret-for-tests".into(),
            issuer,
            identity,
            issued: tokens,
        }
    }

    pub fn assert_identity(&self, identity: MockIdentity) {
        *self.identity.lock().unwrap() = identity;
    }

    pub fn issued_tokens(&self) -> usize {
        self.issued.lock().unwrap().len()
    }
}

async fn discovery(State(shared): State<Shared>) -> Response {
    axum::Json(serde_json::json!({
        "issuer": shared.issuer,
        "authorization_endpoint": format!("{}/authorize", shared.issuer),
        "token_endpoint": format!("{}/token", shared.issuer),
        "jwks_uri": format!("{}/jwks", shared.issuer),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["ES256"],
        "code_challenge_methods_supported": ["S256"],
    }))
    .into_response()
}

async fn authorize(
    State(shared): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let required = [
        "client_id",
        "redirect_uri",
        "state",
        "nonce",
        "code_challenge",
        "code_challenge_method",
        "scope",
        "response_type",
    ];
    if required.iter().any(|key| !query.contains_key(*key))
        || query["client_id"] != shared.client_id
        || query["response_type"] != "code"
        || query["code_challenge_method"] != "S256"
        || !query["scope"].split(' ').any(|scope| scope == "openid")
    {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "bad authorization request",
        )
            .into_response();
    }
    let code = URL_SAFE_NO_PAD.encode(rand_bytes());
    shared.codes.lock().unwrap().insert(
        code.clone(),
        (
            query["nonce"].clone(),
            query["code_challenge"].clone(),
            query["redirect_uri"].clone(),
        ),
    );
    let location = format!(
        "{}?{}",
        query["redirect_uri"],
        serde_urlencoded::to_string([("code", code.as_str()), ("state", query["state"].as_str())])
            .unwrap()
    );
    Redirect::to(&location).into_response()
}

async fn token(State(shared): State<Shared>, body: String) -> Response {
    let form: HashMap<String, String> = serde_urlencoded::from_str(&body).unwrap_or_default();
    let Some(code) = form.get("code") else {
        return (axum::http::StatusCode::BAD_REQUEST, "no code").into_response();
    };
    let Some((nonce, challenge, redirect_uri)) = shared.codes.lock().unwrap().remove(code) else {
        return (axum::http::StatusCode::BAD_REQUEST, "unknown or used code").into_response();
    };
    let verifier = form.get("code_verifier").cloned().unwrap_or_default();
    let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    if form.get("grant_type").map(String::as_str) != Some("authorization_code")
        || form.get("redirect_uri") != Some(&redirect_uri)
        || form.get("client_id") != Some(&shared.client_id)
        || form.get("client_secret") != Some(&shared.client_secret)
        || expected != challenge
    {
        return (axum::http::StatusCode::BAD_REQUEST, "bad token request").into_response();
    }
    let identity = shared.identity.lock().unwrap().clone();
    let now = chrono::Utc::now().timestamp();
    let header = URL_SAFE_NO_PAD
        .encode(serde_json::json!({"alg":"ES256","kid":"mock-1","typ":"JWT"}).to_string());
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "iss": shared.issuer, "sub": identity.subject, "aud": shared.client_id,
            "exp": now + 300, "iat": now, "nonce": nonce,
            "email": identity.email, "email_verified": identity.email_verified, "name": identity.name
        })
        .to_string(),
    );
    let signing_input = format!("{header}.{payload}");
    let signature: p256::ecdsa::Signature = shared.key.sign(signing_input.as_bytes());
    let id_token = format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    );
    shared.issued.lock().unwrap().push(id_token.clone());
    axum::Json(serde_json::json!({
        "access_token": "opaque-access-token", "token_type": "Bearer", "expires_in": 300, "id_token": id_token
    }))
    .into_response()
}

async fn jwks(State(shared): State<Shared>) -> Response {
    let point = shared.key.verifying_key().to_encoded_point(false);
    axum::Json(serde_json::json!({"keys": [{
        "kty": "EC", "crv": "P-256", "kid": "mock-1", "alg": "ES256", "use": "sig",
        "x": URL_SAFE_NO_PAD.encode(point.x().unwrap()),
        "y": URL_SAFE_NO_PAD.encode(point.y().unwrap()),
    }]}))
    .into_response()
}

fn rand_bytes() -> [u8; 16] {
    use p256::elliptic_curve::rand_core::RngCore as _;
    let mut bytes = [0_u8; 16];
    p256::elliptic_curve::rand_core::OsRng.fill_bytes(&mut bytes);
    bytes
}
