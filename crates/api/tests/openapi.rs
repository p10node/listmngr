use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::Response,
};
use listmngr_core::Config;
use listmngr_db::{Database, NewUser};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use tower::ServiceExt;

const API_SOURCE: &str = include_str!("../src/lib.rs");
const HTTP_METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];
const ERROR_STATUSES: [&str; 7] = ["400", "401", "403", "404", "409", "429", "500"];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LiveRoute {
    path: String,
    method: String,
    handler: String,
}

async fn setup() -> (axum::Router, String) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "OpenAPI probe".into(),
            email: "openapi@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let token = db
        .tokens()
        .create(user.id, "openapi-probe", &["admin"], None)
        .await
        .unwrap()
        .token;
    let mut config = Config::default();
    config.security.rate_limit.api = "10000/min".into();
    (listmngr_api::router(db, config), token)
}

async fn request(
    app: &axum::Router,
    token: Option<&str>,
    method: &str,
    uri: &str,
    content_type: Option<&str>,
    body: String,
) -> Response {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    let mut request = builder.body(Body::from(body)).unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    app.clone().oneshot(request).await.unwrap()
}

async fn document(app: &axum::Router) -> Value {
    let response = request(app, None, "GET", "/openapi.json", None, String::new()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn matching_delimiter(source: &str, open: usize, left: u8, right: u8) -> usize {
    let bytes = source.as_bytes();
    let mut depth = 0_u32;
    let mut quoted = false;
    let mut escaped = false;
    for (offset, byte) in bytes[open..].iter().copied().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        if byte == b'"' {
            quoted = true;
        } else if byte == left {
            depth += 1;
        } else if byte == right {
            depth -= 1;
            if depth == 0 {
                return open + offset;
            }
        }
    }
    panic!("unclosed delimiter in route source")
}

fn quoted_value(source: &str) -> &str {
    let first = source.find('"').expect("route path starts with a quote");
    let rest = &source[first + 1..];
    let last = rest.find('"').expect("route path ends with a quote");
    &rest[..last]
}

fn route_handlers(route_call: &str, path: &str) -> Vec<LiveRoute> {
    let mut routes = Vec::new();
    for method in HTTP_METHODS {
        let needle = format!("{method}(");
        let mut cursor = 0;
        while let Some(relative) = route_call[cursor..].find(&needle) {
            let start = cursor + relative;
            let preceding = route_call[..start].chars().next_back();
            cursor = start + needle.len();
            if preceding.is_some_and(|character| character.is_alphanumeric() || character == '_') {
                continue;
            }
            let handler = route_call[cursor..]
                .split(|character: char| !character.is_alphanumeric() && character != '_')
                .next()
                .unwrap();
            routes.push(LiveRoute {
                path: format!("/api/v1{path}"),
                method: method.to_owned(),
                handler: handler.to_owned(),
            });
        }
    }
    routes
}

fn live_v1_routes() -> Vec<LiveRoute> {
    let start = API_SOURCE.find("fn phase_one_routes() ").unwrap();
    let end = API_SOURCE[start..].find("\n}\n\nasync fn health").unwrap() + start;
    let router_source = &API_SOURCE[start..end];
    let mut routes = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = router_source[cursor..].find(".route(") {
        let open = cursor + relative + ".route".len();
        let close = matching_delimiter(router_source, open, b'(', b')');
        let route_call = &router_source[open + 1..close];
        routes.extend(route_handlers(route_call, quoted_value(route_call)));
        cursor = close + 1;
    }
    routes.sort();
    routes
}

fn operations(document: &Value) -> BTreeMap<(String, String), Value> {
    let mut operations = BTreeMap::new();
    for (path, path_item) in document["paths"].as_object().unwrap() {
        for method in HTTP_METHODS {
            if let Some(operation) = path_item.get(method) {
                operations.insert((path.clone(), method.to_owned()), operation.clone());
            }
        }
    }
    operations
}

fn resolve_ref<'a>(document: &'a Value, reference: &str) -> Result<&'a Value, String> {
    let pointer = reference
        .strip_prefix('#')
        .ok_or_else(|| format!("external reference is not allowed: {reference}"))?;
    document
        .pointer(pointer)
        .ok_or_else(|| format!("unresolved reference: {reference}"))
}

fn validate_refs(document: &Value, value: &Value) -> Result<(), String> {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                resolve_ref(document, reference)?;
            }
            for child in object.values() {
                validate_refs(document, child)?;
            }
        }
        Value::Array(array) => {
            for child in array {
                validate_refs(document, child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn schema_is_typed(schema: &Value) -> bool {
    schema.get("$ref").is_some()
        || schema.get("type").is_some()
        || schema.get("oneOf").is_some()
        || schema.get("anyOf").is_some()
        || schema.get("allOf").is_some()
}

fn response_has_typed_content(response: &Value) -> bool {
    response["content"].as_object().is_some_and(|content| {
        !content.is_empty()
            && content
                .values()
                .all(|media| schema_is_typed(&media["schema"]))
    })
}

fn path_placeholders(path: &str) -> BTreeSet<&str> {
    path.split('{')
        .skip(1)
        .filter_map(|part| part.split('}').next())
        .collect()
}

fn expected_request_media(handler: &str) -> BTreeSet<&'static str> {
    let marker = format!("async fn {handler}(");
    let start = API_SOURCE.find(&marker).unwrap() + marker.len() - 1;
    let end = matching_delimiter(API_SOURCE, start, b'(', b')');
    let parameters = &API_SOURCE[start + 1..end];
    if parameters.contains("JsonOrForm<") {
        BTreeSet::from(["application/json", "application/x-www-form-urlencoded"])
    } else if parameters.contains("Json<") {
        BTreeSet::from(["application/json"])
    } else {
        BTreeSet::new()
    }
}

fn validate_operation(
    path: &str,
    method: &str,
    handler: &str,
    operation: &Value,
) -> Result<(), String> {
    let label = format!("{method} {path}");
    let placeholders = path_placeholders(path);
    let parameters = operation["parameters"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let declared: BTreeSet<&str> = parameters
        .iter()
        .filter(|parameter| parameter["in"] == "path")
        .filter_map(|parameter| parameter["name"].as_str())
        .collect();
    if placeholders != declared {
        return Err(format!(
            "{label}: path parameters {declared:?} != {placeholders:?}"
        ));
    }
    if parameters.iter().any(|parameter| {
        parameter["in"] == "path"
            && (parameter["required"] != true || !schema_is_typed(&parameter["schema"]))
    }) {
        return Err(format!(
            "{label}: path parameters must be required and typed"
        ));
    }

    if matches!(method, "post" | "put" | "patch") {
        let expected = expected_request_media(handler);
        let content = operation["requestBody"]["content"]
            .as_object()
            .ok_or_else(|| format!("{label}: missing request body"))?;
        let actual: BTreeSet<&str> = content.keys().map(String::as_str).collect();
        if actual != expected {
            return Err(format!("{label}: request media {actual:?} != {expected:?}"));
        }
        if content
            .values()
            .any(|media| !schema_is_typed(&media["schema"]))
        {
            return Err(format!("{label}: request body schema is untyped"));
        }
    }

    let responses = operation["responses"]
        .as_object()
        .ok_or_else(|| format!("{label}: missing responses"))?;
    let successes: Vec<_> = responses
        .iter()
        .filter(|(status, _)| status.starts_with('2'))
        .collect();
    if successes.is_empty()
        || successes.iter().any(|(status, response)| {
            status.as_str() != "204" && !response_has_typed_content(response)
        })
    {
        return Err(format!("{label}: missing typed success response"));
    }
    for status in ERROR_STATUSES {
        let response = responses
            .get(status)
            .ok_or_else(|| format!("{label}: missing shared {status} error"))?;
        if response["content"]["application/json"]["schema"]["$ref"]
            != "#/components/schemas/ErrorResponse"
        {
            return Err(format!("{label}: {status} must use ErrorResponse"));
        }
    }
    if operation["security"] != json!([{"bearerAuth": []}]) {
        return Err(format!("{label}: bearer security is missing or incorrect"));
    }
    Ok(())
}

fn validate_openapi(document: &Value, routes: &[LiveRoute]) -> Result<(), String> {
    oas3::from_json(serde_json::to_string(document).unwrap())
        .map_err(|error| format!("OpenAPI parse failed: {error}"))?;
    validate_refs(document, document)?;
    if document["components"]["securitySchemes"]["bearerAuth"]["type"] != "http"
        || document["components"]["securitySchemes"]["bearerAuth"]["scheme"] != "bearer"
    {
        return Err("bearerAuth must be an HTTP bearer scheme".into());
    }
    let documented = operations(document);
    let live: BTreeSet<_> = routes
        .iter()
        .map(|route| (route.path.clone(), route.method.clone()))
        .collect();
    let operation_keys: BTreeSet<_> = documented.keys().cloned().collect();
    if live != operation_keys {
        return Err(format!(
            "live route set differs from OpenAPI: live={live:?}, docs={operation_keys:?}"
        ));
    }
    for route in routes {
        let operation = &documented[&(route.path.clone(), route.method.clone())];
        validate_operation(&route.path, &route.method, &route.handler, operation)?;
    }
    Ok(())
}

#[tokio::test]
async fn openapi_is_parseable_resolved_and_matches_every_live_v1_operation() {
    let (app, _) = setup().await;
    let document = document(&app).await;
    validate_openapi(&document, &live_v1_routes()).unwrap();
}

type Sabotage = (&'static str, Box<dyn Fn(&mut Value)>);

#[tokio::test]
async fn structural_validator_rejects_contract_regressions() {
    let (app, _) = setup().await;
    let original = document(&app).await;
    let routes = live_v1_routes();
    let cases: [Sabotage; 6] = [
        (
            "request body",
            Box::new(|doc| {
                doc["paths"]["/api/v1/domains"]["post"]
                    .as_object_mut()
                    .unwrap()
                    .remove("requestBody");
            }),
        ),
        (
            "required path",
            Box::new(|doc| {
                doc["paths"]["/api/v1/domains/{host}"]["get"]["parameters"][0]["required"] =
                    json!(false);
            }),
        ),
        (
            "success schema",
            Box::new(|doc| {
                doc["paths"]["/api/v1/domains"]["get"]["responses"]["200"]["content"]["application/json"].as_object_mut().unwrap().remove("schema");
            }),
        ),
        (
            "shared error",
            Box::new(|doc| {
                doc["paths"]["/api/v1/domains"]["get"]["responses"]["400"]["content"]["application/json"]
                    ["schema"]["$ref"] = json!("#/components/schemas/Domain");
            }),
        ),
        (
            "security",
            Box::new(|doc| {
                doc["paths"]["/api/v1/domains"]["get"]
                    .as_object_mut()
                    .unwrap()
                    .remove("security");
            }),
        ),
        (
            "reference",
            Box::new(|doc| {
                doc["paths"]["/api/v1/domains"]["get"]["responses"]["200"]["content"]["application/json"]
                    ["schema"]["$ref"] = json!("#/components/schemas/Missing");
            }),
        ),
    ];
    for (name, sabotage) in cases {
        let mut changed = original.clone();
        sabotage(&mut changed);
        assert!(
            validate_openapi(&changed, &routes).is_err(),
            "validator accepted missing {name}"
        );
    }
    let mut incomplete_live_routes = routes;
    incomplete_live_routes.pop();
    assert!(
        validate_openapi(&original, &incomplete_live_routes).is_err(),
        "validator accepted route/document divergence"
    );
}

fn dereference_schema<'a>(document: &'a Value, schema: &'a Value) -> &'a Value {
    schema
        .get("$ref")
        .and_then(Value::as_str)
        .map_or(schema, |reference| {
            resolve_ref(document, reference).unwrap()
        })
}

fn schema_example(document: &Value, original: &Value) -> Value {
    let schema = dereference_schema(document, original);
    if let Some(value) = schema.get("example").or_else(|| schema.get("default")) {
        return value.clone();
    }
    if let Some(value) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return value.clone();
    }
    for composition in ["oneOf", "anyOf"] {
        if let Some(first) = schema
            .get(composition)
            .and_then(Value::as_array)
            .and_then(|items| items.first())
        {
            return schema_example(document, first);
        }
    }
    let schema_type = schema["type"].as_str().or_else(|| {
        schema["type"]
            .as_array()?
            .iter()
            .find_map(Value::as_str)
            .filter(|kind| *kind != "null")
    });
    match schema_type {
        Some("object") | None if schema.get("properties").is_some() => {
            let required: BTreeSet<&str> = schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let mut object = Map::new();
            if let Some(properties) = schema["properties"].as_object() {
                for (name, property) in properties {
                    if required.contains(name.as_str()) {
                        object.insert(name.clone(), schema_example(document, property));
                    }
                }
            }
            Value::Object(object)
        }
        Some("array") => Value::Array(Vec::new()),
        Some("boolean") => Value::Bool(false),
        Some("integer" | "number") => json!(1),
        _ => match schema["format"].as_str() {
            Some("uuid") => json!("00000000-0000-0000-0000-000000000001"),
            Some("date-time") => json!("2026-01-01T00:00:00Z"),
            _ => json!("test"),
        },
    }
}

fn operation_uri(document: &Value, path: &str, operation: &Value) -> String {
    let mut uri = path.to_owned();
    for parameter in operation["parameters"].as_array().into_iter().flatten() {
        if parameter["in"] == "path" {
            let name = parameter["name"].as_str().unwrap();
            let value = schema_example(document, &parameter["schema"]);
            let value = value.as_str().unwrap_or("1").replace('@', "%40");
            uri = uri.replace(&format!("{{{name}}}"), &value);
        }
    }
    uri
}

fn operation_body(document: &Value, operation: &Value) -> (Option<String>, String) {
    let Some(content) = operation["requestBody"]["content"].as_object() else {
        return (None, String::new());
    };
    let (content_type, media) = content
        .get_key_value("application/json")
        .or_else(|| content.iter().next())
        .unwrap();
    let example = schema_example(document, &media["schema"]);
    (
        Some(content_type.clone()),
        serde_json::to_string(&example).unwrap(),
    )
}

async fn assert_operation_reaches_handler(
    app: &axum::Router,
    token: &str,
    document: &Value,
    path: &str,
    method: &str,
    operation: &Value,
) {
    let uri = operation_uri(document, path, operation);
    let (content_type, body) = operation_body(document, operation);
    let response = request(
        app,
        Some(token),
        &method.to_uppercase(),
        &uri,
        content_type.as_deref(),
        body,
    )
    .await;
    assert_ne!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "documented operation did not route: {method} {path}"
    );
    if response.status() == StatusCode::NOT_FOUND {
        let is_json = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json"));
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        assert!(
            is_json && body["code"] == "not_found",
            "undocumented router 404 for {method} {path} at {uri}: {body}"
        );
    }
}

#[tokio::test]
async fn every_documented_v1_operation_reaches_its_live_handler_with_generated_input() {
    let (app, token) = setup().await;
    let document = document(&app).await;
    let documented = operations(&document);
    let expected: BTreeSet<_> = documented.keys().cloned().collect();
    let mut visited = BTreeSet::new();
    for ((path, method), operation) in &documented {
        assert_operation_reaches_handler(&app, &token, &document, path, method, operation).await;
        visited.insert((path.clone(), method.clone()));
    }
    assert_eq!(visited, expected);
}

#[tokio::test]
async fn paginated_operations_and_rate_limit_headers_are_typed() {
    let (app, _) = setup().await;
    let document = document(&app).await;
    let paginated = [
        ("/api/v1/system/pipelines", "get"),
        ("/api/v1/system/chains", "get"),
        ("/api/v1/domains", "get"),
        ("/api/v1/domains/{host}/lists", "get"),
        ("/api/v1/domains/{host}/owners", "get"),
        ("/api/v1/lists", "get"),
        ("/api/v1/lists/styles", "get"),
        ("/api/v1/lists/{id}/archivers", "get"),
        ("/api/v1/lists/{id}/templates", "get"),
        ("/api/v1/lists/{id}/roster/{role}", "get"),
        ("/api/v1/members/find", "post"),
        ("/api/v1/users", "get"),
        ("/api/v1/users/{id}/addresses", "get"),
        ("/api/v1/addresses/{email}/memberships", "get"),
        ("/api/v1/owners", "get"),
    ];
    for (path, method) in paginated {
        let operation = &document["paths"][path][method];
        let query_names: BTreeSet<_> = operation["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|parameter| parameter["in"] == "query")
            .filter_map(|parameter| parameter["name"].as_str())
            .collect();
        assert!(
            query_names.is_superset(&BTreeSet::from(["cursor", "page", "count"])),
            "{method} {path}: {query_names:?}"
        );
    }
    assert_eq!(
        document["paths"]["/api/v1/system/pipelines"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/CatalogPageResponse"
    );

    for ((path, method), operation) in operations(&document) {
        let retry_after = &operation["responses"]["429"]["headers"]["Retry-After"];
        assert_eq!(retry_after["schema"]["type"], "integer", "{method} {path}");
        assert_eq!(retry_after["schema"]["minimum"], 1, "{method} {path}");
    }
}
