//! The vendored browser assets are exactly the bytes this repository reviewed.
use base64::Engine as _;
use sha2::Digest as _;

#[test]
fn the_vendored_htmx_matches_its_pinned_hash() {
    let digest = sha2::Sha384::digest(listmngr_web::HTMX.as_bytes());
    assert_eq!(
        base64::engine::general_purpose::STANDARD.encode(digest),
        listmngr_web::HTMX_SHA384,
        "crates/web/assets/README.md records the provenance of this file"
    );
}

#[test]
fn no_asset_reaches_out_to_another_origin() {
    for (name, asset) in [
        ("style.css", listmngr_web::STYLESHEET),
        ("htmx.min.js", listmngr_web::HTMX),
        ("passkeys.js", listmngr_web::PASSKEYS_SCRIPT),
        ("moderation.js", listmngr_web::MODERATION_SCRIPT),
    ] {
        for marker in ["http://", "https://", "//cdn", "unpkg", "jsdelivr"] {
            assert!(
                !asset.contains(marker),
                "{name} refers to {marker}; assets are served from this origin only"
            );
        }
    }
}
