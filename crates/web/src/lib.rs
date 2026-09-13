#![forbid(unsafe_code)]
//! Small, dependency-free server-rendered browser presentation primitives.

/// Escape untrusted text for HTML text and quoted attribute contexts.
#[must_use]
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Render a complete semantic document. `body` must be trusted HTML whose data
/// has already been escaped with [`escape`]. No script or external assets load.
#[must_use]
pub fn document(title: &str, body: &str) -> String {
    let title = escape(title);
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><link rel=\"stylesheet\" href=\"/web/style.css\"><title>{title} · listmngr</title></head><body><a class=\"skip\" href=\"#main\">Skip to content</a><header><a class=\"brand\" href=\"/web\">listmngr</a><nav aria-label=\"Main\"><a href=\"/web\">Lists</a><a href=\"/web/account\">My subscriptions</a><a href=\"/web/moderation\">Moderation</a><a href=\"/web/login\">Log in</a></nav></header><main id=\"main\"><h1>{title}</h1>{body}</main><footer>Mailing lists, managed by your community.</footer></body></html>"
    )
}

/// Responsive, local-only stylesheet with visible keyboard focus.
pub const STYLESHEET: &str = r"
:root{font-family:system-ui,sans-serif;color:#172b3a;background:#f3f6f8;line-height:1.6;color-scheme:light}
*{box-sizing:border-box}body{margin:0}header{background:#153b48;color:white;padding:1.2rem max(1rem,calc((100% - 62rem)/2));display:flex;flex-wrap:wrap;gap:1rem;align-items:center}header a{color:white}nav{display:flex;flex-wrap:wrap;gap:1.3rem}.brand{font-size:1.4rem;font-weight:750;margin-right:auto;text-decoration:none}main{max-width:62rem;margin:2rem auto;padding:0 1rem}h1{font-size:clamp(1.8rem,5vw,2.6rem);line-height:1.2}h2{font-size:1.3rem}a{color:#00667c;text-underline-offset:.18em}article,section,form{background:white;border:1px solid #b5c8cf;border-radius:.55rem;padding:1.25rem;margin:1.25rem 0}article form,section form{border:0;padding:0}label{display:block;font-weight:600}input,select,textarea,button{font:inherit;max-width:100%;border:1px solid #627f8b;border-radius:.3rem;padding:.55rem .7rem}input,select,textarea{display:block;width:min(100%,32rem);margin-top:.25rem}textarea{min-height:6rem}button{background:#075c70;color:white;font-weight:650;cursor:pointer}button:hover{background:#123d4a}:focus-visible{outline:3px solid #c56600;outline-offset:3px}pre{white-space:pre-wrap;overflow-wrap:anywhere;background:#f1f4f5;padding:1rem;max-height:28rem;overflow:auto}dd{margin-left:0;overflow-wrap:anywhere}dt{font-weight:bold}li{padding:.5rem 0}footer{max-width:62rem;margin:3rem auto;padding:1rem;color:#405f6d}.skip{position:absolute;left:-10000px}.skip:focus{position:static;background:white;padding:1rem;display:block}
";
