//! RFC 6238 time-based one-time passwords.
//!
//! RFC 4226 HOTP with HMAC-SHA1, six digits and a 30-second step — what every
//! authenticator app expects — plus the RFC 4648 base32 alphabet those apps
//! read secrets in.
use hmac::{Hmac, Mac};
use sha1::Sha1;

/// Seconds per time step.
pub const STEP_SECONDS: i64 = 30;
/// Digits in a code.
pub const DIGITS: usize = 6;
/// Secret length in bytes (160 bits, the RFC 4226 recommendation).
pub const SECRET_BYTES: usize = 20;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Base32 without padding, as authenticator apps read it.
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Base32 to bytes; case, spaces, hyphens and padding are forgiven. `None`
/// for any other character.
#[must_use]
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for character in text.chars() {
        if matches!(character, ' ' | '-' | '=') {
            continue;
        }
        let value = ALPHABET
            .iter()
            .position(|letter| *letter as char == character.to_ascii_uppercase())?;
        buffer = (buffer << 5) | u32::try_from(value).ok()?;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// The six-digit code for `secret` at time step `counter`.
///
/// # Panics
/// Never: HMAC accepts a key of any length.
#[must_use]
pub fn code(secret: &[u8], counter: i64) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[19] & 0x0f);
    let binary = (u32::from(digest[offset] & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    format!("{:0width$}", binary % 1_000_000, width = DIGITS)
}

/// The time step containing the Unix time `seconds`.
#[must_use]
pub const fn step_of(seconds: i64) -> i64 {
    seconds.div_euclid(STEP_SECONDS)
}

/// Whether `presented` is the code for one of the steps `now - 1 ..= now + 1`
/// that is later than `last_accepted`; the step it matched, so the caller can
/// record it and refuse a replay.
#[must_use]
pub fn verify(secret: &[u8], now_seconds: i64, presented: &str, last_accepted: i64) -> Option<i64> {
    let presented: String = presented.chars().filter(|c| !c.is_whitespace()).collect();
    if presented.len() != DIGITS || !presented.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let current = step_of(now_seconds);
    [current - 1, current, current + 1]
        .into_iter()
        .filter(|step| *step > last_accepted)
        .find(|step| {
            use subtle::ConstantTimeEq as _;
            bool::from(code(secret, *step).as_bytes().ct_eq(presented.as_bytes()))
        })
}

/// The `otpauth://` URI an authenticator app enrols from.
#[must_use]
pub fn provisioning_uri(issuer: &str, account: &str, secret: &[u8]) -> String {
    use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
    let issuer = utf8_percent_encode(issuer, NON_ALPHANUMERIC).to_string();
    let account = utf8_percent_encode(account, NON_ALPHANUMERIC).to_string();
    format!(
        "otpauth://totp/{issuer}:{account}?secret={}&issuer={issuer}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECONDS}",
        encode(secret)
    )
}

/// The provisioning URI as an inline SVG QR code, or `None` if it does not fit.
#[must_use]
pub fn qr_svg(uri: &str) -> Option<String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::new(uri.as_bytes()).ok()?;
    let rendered = code
        .render::<svg::Color<'_>>()
        .min_dimensions(200, 200)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    // Inline in HTML: the XML declaration has no place there.
    let start = rendered.find("<svg")?;
    Some(rendered[start..].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_6238_sha1_vectors() {
        // RFC 6238 appendix B, the 20-byte ASCII secret "12345678901234567890".
        let secret = b"12345678901234567890";
        for (seconds, expected) in [
            (59, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
        ] {
            assert_eq!(code(secret, step_of(seconds)), expected, "{seconds}");
        }
    }

    #[test]
    fn base32_round_trips_and_forgives_formatting() {
        let secret = b"12345678901234567890";
        let text = encode(secret);
        assert_eq!(text, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(decode(&text).unwrap(), secret);
        assert_eq!(
            decode("gezd gnbv-gy3tqojqgezdgnbvgy3tqojq==").unwrap(),
            secret
        );
        assert!(decode("not base32!").is_none());
    }

    #[test]
    fn drift_and_replay() {
        let secret = b"12345678901234567890";
        let now = 1_111_111_111;
        let current = step_of(now);
        assert_eq!(
            verify(secret, now, &code(secret, current), 0),
            Some(current)
        );
        assert_eq!(
            verify(secret, now, &code(secret, current - 1), 0),
            Some(current - 1)
        );
        assert_eq!(
            verify(secret, now, &code(secret, current + 1), 0),
            Some(current + 1)
        );
        assert_eq!(verify(secret, now, &code(secret, current + 2), 0), None);
        assert_eq!(
            verify(secret, now, &code(secret, current), current),
            None,
            "replay"
        );
        assert_eq!(verify(secret, now, "12345", 0), None);
        assert_eq!(verify(secret, now, "abcdef", 0), None);
    }

    #[test]
    fn provisioning_uri_escapes_and_renders() {
        let uri = provisioning_uri("Example Lists", "root@example.com", b"12345678901234567890");
        assert!(uri.starts_with("otpauth://totp/Example%20Lists:root%40example%2Ecom?secret=GEZD"));
        assert!(uri.ends_with("&issuer=Example%20Lists&algorithm=SHA1&digits=6&period=30"));
        let svg = qr_svg(&uri).unwrap();
        assert!(svg.starts_with("<svg"), "{svg}");
        assert!(!svg.contains("<script"));
    }
}
