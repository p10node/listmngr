//! The API reference this server serves for its own document.
//!
//! It is rendered from the same `OpenAPI` value `/openapi.json` returns, so the
//! page cannot drift from the API, and it needs no script and no third-party
//! asset: an operator who opens it is not made to fetch code from a CDN.
use axum::{
    http::header,
    response::{Html, IntoResponse, Response},
};
use listmngr_web::{DocOperation, DocPath};
use serde_json::Value;

const METHODS: &[&str] = &["get", "put", "post", "patch", "delete", "head", "options"];

/// Render the reference for `document`, an `OpenAPI` 3 value.
pub fn render(document: &Value) -> Response {
    let info = &document["info"];
    let page = listmngr_web::ApiDocs {
        title: text(&info["title"]),
        version: text(&info["version"]),
        intro: "Every operation below is served by this deployment. This page is \
                generated from the same document as /openapi.json and is read-only: \
                it neither calls the API nor loads anything from another origin."
            .into(),
        authentication: "Requests carry a scoped API token as an Authorization: \
                         Bearer header. Browser sessions are separate and do not \
                         authorize these operations."
            .into(),
        paths: paths(document),
        footer: "Point your own client at /openapi.json for an interactive \
                 explorer; this server does not embed one."
            .into(),
    };
    let mut response =
        Html(askama::Template::render(&page).expect("template renders")).into_response();
    for (name, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "strict-origin"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        (
            "content-security-policy",
            "default-src 'none'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
        ),
    ] {
        response.headers_mut().insert(
            header::HeaderName::from_static(name),
            header::HeaderValue::from_static(value),
        );
    }
    response
}

fn paths(document: &Value) -> Vec<DocPath> {
    let Some(paths) = document["paths"].as_object() else {
        return Vec::new();
    };
    paths
        .iter()
        .map(|(path, item)| DocPath {
            path: path.clone(),
            operations: operations(item),
        })
        .collect()
}

fn operations(item: &Value) -> Vec<DocOperation> {
    METHODS
        .iter()
        .filter_map(|method| {
            let operation = item.get(*method)?;
            Some(DocOperation {
                method: method.to_uppercase(),
                summary: summary(operation),
                scopes: scopes(operation),
                parameters: parameters(operation),
                responses: responses(operation),
            })
        })
        .collect()
}

/// The first line of the summary or description; the rest is prose for a
/// reader of the document itself.
fn summary(operation: &Value) -> String {
    let source = operation
        .get("summary")
        .and_then(Value::as_str)
        .or_else(|| operation.get("description").and_then(Value::as_str))
        .unwrap_or_default();
    source.lines().next().unwrap_or_default().trim().to_owned()
}

fn scopes(operation: &Value) -> String {
    let Some(requirements) = operation["security"].as_array() else {
        return String::new();
    };
    let mut names: Vec<String> = requirements
        .iter()
        .filter_map(Value::as_object)
        .flat_map(|requirement| requirement.values())
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect();
    names.sort();
    names.dedup();
    names.join(", ")
}

fn parameters(operation: &Value) -> String {
    let Some(parameters) = operation["parameters"].as_array() else {
        return String::new();
    };
    parameters
        .iter()
        .map(|parameter| {
            let name = text(&parameter["name"]);
            let location = text(&parameter["in"]);
            let required = if parameter["required"] == Value::Bool(true) {
                ", required"
            } else {
                ""
            };
            format!("{name} ({location}{required})")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn responses(operation: &Value) -> String {
    let Some(responses) = operation["responses"].as_object() else {
        return String::new();
    };
    responses
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}
