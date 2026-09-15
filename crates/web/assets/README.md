# Vendored browser assets

No page loads a third-party origin: everything here is served from this
deployment, and the build gates pin the bytes.

| File | Upstream | Version | License | SHA-384 (base64, `integrity` form) |
| --- | --- | --- | --- | --- |
| `htmx.min.js` | <https://unpkg.com/htmx.org@2.0.10/dist/htmx.min.js> | 2.0.10 | 0BSD | `H5SrcfygHmAuTDZphMHqBJLc3FhssKjG7w/CeCpFReSfwBWDTKpkzPP8c+cLsK+V` |

The copy was fetched from two independent mirrors (unpkg and jsDelivr) and both
bytes matched (SHA-256
`71ea67185bfa8c98c39d31717c6fce5d852370fcdfd129db4543774d3145c0de`).
`crates/web/tests/assets.rs` fails if the file ever stops matching the pinned
hash, so a swapped asset cannot reach a browser unnoticed.

To update: fetch the new version, record its hash here and in
`listmngr_web::HTMX_SHA384`, and re-run the gates.

`style.css` is first-party and ships the design tokens both colour schemes
bind. `passkeys.js` is first-party too: the WebAuthn ceremonies, the only
script any page loads, and only on the pages that offer passkeys.
