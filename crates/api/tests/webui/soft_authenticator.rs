//! A software passkey: ES256 keys and CTAP2-shaped responses, enough to run
//! registration and assertion ceremonies against the server without a
//! browser. Attestation is `none`, user presence and verification are always
//! claimed, and the signature counter increments per assertion.
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::{SigningKey, signature::Signer};
use sha2::{Digest, Sha256};

pub struct SoftPasskey {
    key: SigningKey,
    credential_id: Vec<u8>,
    user_handle: String,
    counter: u32,
    rp_id: String,
    origin: String,
}

fn cbor(value: &ciborium::Value) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(value, &mut out).unwrap();
    out
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

impl SoftPasskey {
    /// Answer a registration ceremony from its creation options JSON.
    pub fn register(options: &serde_json::Value, origin: &str) -> (Self, String) {
        let public_key = options.get("publicKey").unwrap_or(options);
        let rp_id = public_key["rp"]["id"].as_str().expect("rp id").to_owned();
        let challenge = public_key["challenge"].as_str().expect("challenge");
        let user_handle = public_key["user"]["id"]
            .as_str()
            .expect("user id")
            .to_owned();
        let key = SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
        let point = key.verifying_key().to_encoded_point(false);
        let mut credential_id = vec![0_u8; 32];
        rand_fill(&mut credential_id);
        let cose = ciborium::Value::Map(vec![
            (1.into(), 2.into()),
            (3.into(), (-7).into()),
            ((-1).into(), 1.into()),
            (
                (-2).into(),
                ciborium::Value::Bytes(point.x().unwrap().to_vec()),
            ),
            (
                (-3).into(),
                ciborium::Value::Bytes(point.y().unwrap().to_vec()),
            ),
        ]);
        let mut auth_data = Sha256::digest(rp_id.as_bytes()).to_vec();
        auth_data.push(0x01 | 0x04 | 0x40);
        auth_data.extend_from_slice(&0_u32.to_be_bytes());
        auth_data.extend_from_slice(&[0_u8; 16]);
        auth_data.extend_from_slice(&u16::try_from(credential_id.len()).unwrap().to_be_bytes());
        auth_data.extend_from_slice(&credential_id);
        auth_data.extend_from_slice(&cbor(&cose));
        let attestation = cbor(&ciborium::Value::Map(vec![
            ("fmt".into(), "none".into()),
            ("attStmt".into(), ciborium::Value::Map(vec![])),
            ("authData".into(), ciborium::Value::Bytes(auth_data)),
        ]));
        let client_data = serde_json::json!({
            "type": "webauthn.create", "challenge": challenge, "origin": origin, "crossOrigin": false
        })
        .to_string();
        let response = serde_json::json!({
            "id": b64(&credential_id), "rawId": b64(&credential_id), "type": "public-key",
            "response": {
                "clientDataJSON": b64(client_data.as_bytes()),
                "attestationObject": b64(&attestation),
                "transports": ["internal"]
            },
            "clientExtensionResults": {},
            "authenticatorAttachment": "platform"
        })
        .to_string();
        (
            Self {
                key,
                credential_id,
                user_handle,
                counter: 0,
                rp_id,
                origin: origin.to_owned(),
            },
            response,
        )
    }

    /// Answer an authentication ceremony from its request options JSON.
    pub fn assert(&mut self, options: &serde_json::Value) -> String {
        let public_key = options.get("publicKey").unwrap_or(options);
        let challenge = public_key["challenge"].as_str().expect("challenge");
        self.counter += 1;
        let mut auth_data = Sha256::digest(self.rp_id.as_bytes()).to_vec();
        auth_data.push(0x01 | 0x04);
        auth_data.extend_from_slice(&self.counter.to_be_bytes());
        let client_data = serde_json::json!({
            "type": "webauthn.get", "challenge": challenge, "origin": self.origin, "crossOrigin": false
        })
        .to_string();
        let mut signed = auth_data.clone();
        signed.extend_from_slice(&Sha256::digest(client_data.as_bytes()));
        let signature: p256::ecdsa::DerSignature = self.key.sign(&signed);
        serde_json::json!({
            "id": b64(&self.credential_id), "rawId": b64(&self.credential_id), "type": "public-key",
            "response": {
                "clientDataJSON": b64(client_data.as_bytes()),
                "authenticatorData": b64(&auth_data),
                "signature": b64(signature.as_bytes()),
                "userHandle": self.user_handle
            },
            "clientExtensionResults": {},
            "authenticatorAttachment": "platform"
        })
        .to_string()
    }

    pub fn credential_id(&self) -> String {
        b64(&self.credential_id)
    }
}

fn rand_fill(bytes: &mut [u8]) {
    use p256::elliptic_curve::rand_core::RngCore as _;
    p256::elliptic_curve::rand_core::OsRng.fill_bytes(bytes);
}
