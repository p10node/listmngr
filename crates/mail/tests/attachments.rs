use listmngr_mail::attachments::{content, names};

#[test]
fn text_attachment_download_preserves_charset_octets_and_line_endings() {
    for (encoding, body, expected) in [
        (
            "quoted-printable",
            b"caf=E9=0D=0Acol;=80".as_slice(),
            b"caf\xe9\r\ncol;\x80".as_slice(),
        ),
        ("base64", b"6QD/".as_slice(), b"\xe9\x00\xff".as_slice()),
        (
            "8bit",
            b"caf\xe9\r\nraw\xff\n".as_slice(),
            b"caf\xe9\r\nraw\xff\n".as_slice(),
        ),
    ] {
        for mime in ["text/plain", "text/html", "text/csv"] {
            let mut raw = format!("MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nbody\r\n--b\r\nContent-Type: {mime}; charset=windows-1252\r\nContent-Disposition: attachment; filename=data.csv\r\nContent-Transfer-Encoding: {encoding}\r\n\r\n").into_bytes();
            raw.extend_from_slice(body);
            raw.extend_from_slice(b"\r\n--b--\r\n");
            assert_eq!(names(&raw).unwrap(), ["data.csv"]);
            assert_eq!(
                content(&raw, 0).unwrap().unwrap(),
                expected,
                "{mime} {encoding}"
            );
        }
    }
}

#[test]
fn corrupt_attachment_transfer_encoding_is_not_returned_as_valid_bytes() {
    let raw = b"Content-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=broken.bin\r\nContent-Transfer-Encoding: base64\r\n\r\n@@@invalid@@@";
    assert!(content(raw, 0).is_err());
    assert!(names(raw).is_err());
}

#[test]
fn attachment_projection_bounds_and_distinct_decodings() {
    let raw = b"MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=first.bin\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n=00=FF=41\r\n--b\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment\r\nContent-Transfer-Encoding: base64\r\n\r\nAQID\r\n--b--\r\n";
    assert_eq!(names(raw).unwrap(), ["first.bin", "Attachment 1"]);
    assert_eq!(content(raw, 0).unwrap().unwrap(), b"\x00\xffA");
    assert_eq!(content(raw, 1).unwrap().unwrap(), b"\x01\x02\x03");
    assert!(content(raw, 2).unwrap().is_none());
    assert!(names(&[]).is_err());
    let oversized = vec![b'x'; 10 * 1024 * 1024 + 1];
    assert!(names(&oversized).is_err());
    assert!(content(&oversized, 0).is_err());
    let prefix = "Content-Type: multipart/mixed; boundary=b\r\n\r\n";
    let part = "--b\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment\r\n\r\nx\r\n";
    let allowed = format!("{prefix}{}--b--\r\n", part.repeat(64));
    assert_eq!(names(allowed.as_bytes()).unwrap().len(), 64);
    assert_eq!(content(allowed.as_bytes(), 63).unwrap().unwrap(), b"x");
    let denied = format!("{prefix}{}--b--\r\n", part.repeat(65));
    assert!(names(denied.as_bytes()).is_err());
    assert!(content(denied.as_bytes(), 0).is_err());
}
