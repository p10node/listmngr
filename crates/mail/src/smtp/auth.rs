//! Private credential policy; reachable on the wire only from verified TLS.
#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
use super::{AsyncBufRead, AsyncWrite, IoError, IoResult, SmtpClientConfig, command};
use base64::{Engine as _, engine::general_purpose::STANDARD};

#[derive(Clone)]
pub(super) struct AuthPlain(String);
impl std::fmt::Debug for AuthPlain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthPlain([REDACTED])")
    }
}
impl AuthPlain {
    pub(super) fn from_mta(
        mta: &listmngr_core::MtaConfig,
    ) -> Result<Option<Self>, listmngr_core::Error> {
        Ok(mta.smtp_auth_credentials()?.map(|(user, password)| {
            Self(STANDARD.encode(format!("\0{}\0{}", user.expose(), password.expose())))
        }))
    }
    pub(super) async fn authenticate<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
        &self,
        writer: &mut W,
        reader: &mut R,
        config: &SmtpClientConfig,
    ) -> IoResult<bool> {
        tokio::time::timeout(config.command_timeout, async {
            let ehlo = command(
                writer,
                reader,
                config.command_timeout,
                &format!("EHLO {}", config.local_hostname),
            )
            .await?;
            if ehlo.code != 250
                || !ehlo.extensions.iter().any(|line| {
                    let mut tokens = line.split_ascii_whitespace();
                    tokens
                        .next()
                        .is_some_and(|token| token.eq_ignore_ascii_case("AUTH"))
                        && tokens.any(|token| token.eq_ignore_ascii_case("PLAIN"))
                })
            {
                return Err(IoError::other("SMTP AUTH PLAIN unavailable"));
            }
            // RFC 4954: the AUTH command still has SMTP's 512-octet limit.
            // A longer SASL response must follow the server's empty challenge.
            let inline = format!("AUTH PLAIN {}", self.0);
            let mut response_sent = inline.len() + 2 <= 512;
            let mut reply = command(
                writer,
                reader,
                config.command_timeout,
                if response_sent { &inline } else { "AUTH PLAIN" },
            )
            .await?;
            if reply.code == 334 && reply.text.is_empty() && reply.extensions.is_empty() {
                reply = command(writer, reader, config.command_timeout, &self.0).await?;
                response_sent = true;
            }
            if !response_sent || reply.code != 235 {
                return Err(IoError::other("SMTP AUTH rejected"));
            }
            Ok(ehlo
                .extensions
                .iter()
                .any(|line| line.eq_ignore_ascii_case("DSN")))
        })
        .await
        .map_err(|_| IoError::new(std::io::ErrorKind::TimedOut, "SMTP AUTH deadline exceeded"))?
        .map_err(|error: IoError| {
            IoError::new(
                error.kind(),
                "SMTP AUTH negotiation failed (reply redacted)",
            )
        })
    }
}
