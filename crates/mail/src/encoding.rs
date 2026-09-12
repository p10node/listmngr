//! Body encodings for text parts this runtime generates.
//!
//! `7bit` when the text is ASCII with short lines, otherwise RFC 2045
//! quoted-printable, which keeps decorated and converted text readable on
//! the wire.

/// Maximum encoded line length, excluding the line end.
const QP_LINE: usize = 76;
const LINE_LIMIT: usize = 998;

/// Quoted-printable encode `text` (LF line ends) using `newline` on the
/// wire: `=`, bytes outside printable ASCII and trailing whitespace become
/// `=XX`; soft breaks keep lines at 76 columns.
fn quoted_printable(text: &str, newline: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + text.len() / 8);
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.extend_from_slice(newline);
        }
        let bytes = line.as_bytes();
        let mut column = 0;
        for (position, &byte) in bytes.iter().enumerate() {
            let last = position + 1 == bytes.len();
            let literal =
                matches!(byte, 33..=60 | 62..=126) || ((byte == b' ' || byte == b'\t') && !last);
            let width = if literal { 1 } else { 3 };
            if column + width > QP_LINE - 1 {
                out.extend_from_slice(b"=");
                out.extend_from_slice(newline);
                column = 0;
            }
            if literal {
                out.push(byte);
            } else {
                out.extend_from_slice(format!("={byte:02X}").as_bytes());
            }
            column += width;
        }
    }
    out
}

/// The transfer encoding and wire bytes for a generated text body.
#[must_use]
pub fn text_body(text: &str, newline: &[u8]) -> (&'static str, Vec<u8>) {
    let normalized = text.replace("\r\n", "\n");
    let seven_bit =
        normalized.is_ascii() && normalized.split('\n').all(|line| line.len() <= LINE_LIMIT);
    if seven_bit {
        let mut out = Vec::with_capacity(normalized.len());
        for (index, line) in normalized.split('\n').enumerate() {
            if index > 0 {
                out.extend_from_slice(newline);
            }
            out.extend_from_slice(line.as_bytes());
        }
        return ("7bit", out);
    }
    ("quoted-printable", quoted_printable(&normalized, newline))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_travels_7bit_and_the_rest_quoted_printable() {
        assert_eq!(text_body("a\nb", b"\r\n"), ("7bit", b"a\r\nb".to_vec()));
        let (encoding, bytes) = text_body("Café =\n", b"\r\n");
        assert_eq!(encoding, "quoted-printable");
        assert_eq!(bytes, b"Caf=C3=A9 =3D\r\n".to_vec());
        let (_, bytes) = text_body("trailing é \n", b"\r\n");
        assert_eq!(bytes, b"trailing =C3=A9=20\r\n".to_vec());
    }

    #[test]
    fn long_lines_get_soft_breaks_under_76_columns() {
        let (_, bytes) = text_body(&"é".repeat(40), b"\r\n");
        for line in bytes.split(|b| *b == b'\n') {
            assert!(
                line.len() <= QP_LINE + 1,
                "{}",
                String::from_utf8_lossy(line)
            );
        }
        let decoded =
            mail_parser::decoders::quoted_printable::quoted_printable_decode(&bytes).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "é".repeat(40));
    }
}
