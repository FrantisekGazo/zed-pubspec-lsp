use pubspec_language_server::diagnostics;
use pubspec_language_server::document::Document;
use pubspec_language_server::pubdev::PubDevClient;
use pubspec_language_server::pubspec::PubspecModel;
use serde_json::json;
use tower_lsp_server::ls_types::DiagnosticSeverity;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn package_json(name: &str, latest: &str, discontinued: bool) -> serde_json::Value {
    let mut root = json!({
        "name": name,
        "latest": {
            "version": latest,
            "pubspec": { "name": name, "description": format!("The {name} package.") }
        },
        "versions": [
            { "version": "0.9.0", "pubspec": {} },
            { "version": latest, "pubspec": {} }
        ]
    });
    if discontinued {
        root["isDiscontinued"] = json!(true);
        root["replacedBy"] = json!("shiny_new");
    }
    root
}

async fn mock_pubdev() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/packages/http"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(package_json("http", "1.6.0", false)),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/packages/pedantic"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(package_json("pedantic", "1.11.1", true)),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/packages/no_such_pkg"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    server
}

fn document(text: &str) -> Document {
    Document {
        text: text.to_string(),
        version: 1,
        model: PubspecModel::parse(text),
    }
}

#[tokio::test]
async fn outdated_and_discontinued_diagnostics() {
    let mock = mock_pubdev().await;
    let pubdev = PubDevClient::new(mock.uri());
    let doc = document(
        "name: demo\n\
         dependencies:\n\
         \x20 http: ^0.13.0\n\
         \x20 pedantic: ^0.9.0\n",
    );

    let diags = diagnostics::compute(&doc, &pubdev).await;

    assert_eq!(diags.len(), 3, "diags: {diags:#?}");
    // Line 2: http outdated (hint on the constraint).
    assert_eq!(diags[0].severity, Some(DiagnosticSeverity::HINT));
    assert_eq!(diags[0].message, "Newer version available: 1.6.0");
    assert_eq!(diags[0].range.start.line, 2);
    assert_eq!(
        diags[0].data,
        Some(json!({ "latest": "1.6.0", "caret": true }))
    );
    // Line 3: pedantic discontinued (warning on the name) + outdated hint.
    assert_eq!(diags[1].severity, Some(DiagnosticSeverity::WARNING));
    assert!(diags[1].message.contains("discontinued"));
    assert!(diags[1].message.contains("shiny_new"));
    assert_eq!(diags[2].severity, Some(DiagnosticSeverity::HINT));
}

#[tokio::test]
async fn bare_constraint_reports_no_caret() {
    let mock = mock_pubdev().await;
    let pubdev = PubDevClient::new(mock.uri());
    let doc = document("dependencies:\n\x20 http: 1.5.0\n");

    let diags = diagnostics::compute(&doc, &pubdev).await;
    assert_eq!(diags.len(), 1);
    assert_eq!(
        diags[0].data,
        Some(json!({ "latest": "1.6.0", "caret": false }))
    );
}

#[tokio::test]
async fn up_to_date_unknown_and_nonhosted_produce_nothing() {
    let mock = mock_pubdev().await;
    let pubdev = PubDevClient::new(mock.uri());
    let doc = document(
        "dependencies:\n\
         \x20 http: ^1.6.0\n\
         \x20 no_such_pkg: ^1.0.0\n\
         \x20 flutter:\n\
         \x20   sdk: flutter\n",
    );

    let diags = diagnostics::compute(&doc, &pubdev).await;
    assert!(diags.is_empty(), "diags: {diags:#?}");
}

#[tokio::test]
async fn offline_is_silent() {
    // Port from a server that's already shut down — connection refused.
    let mock = MockServer::start().await;
    let base = mock.uri();
    drop(mock);

    let pubdev = PubDevClient::new(base);
    let doc = document("dependencies:\n\x20 http: ^0.13.0\n");
    let diags = diagnostics::compute(&doc, &pubdev).await;
    assert!(diags.is_empty());
}

#[tokio::test]
async fn overrides_skip_outdated_but_keep_discontinued() {
    let mock = mock_pubdev().await;
    let pubdev = PubDevClient::new(mock.uri());
    let doc = document(
        "dependency_overrides:\n\
         \x20 http: ^0.13.0\n\
         \x20 pedantic: ^1.0.0\n",
    );

    let diags = diagnostics::compute(&doc, &pubdev).await;
    assert_eq!(diags.len(), 1, "diags: {diags:#?}");
    assert_eq!(diags[0].severity, Some(DiagnosticSeverity::WARNING));
    assert!(diags[0].message.contains("discontinued"));
}

#[tokio::test]
async fn broken_yaml_produces_nothing() {
    let mock = mock_pubdev().await;
    let pubdev = PubDevClient::new(mock.uri());
    let doc = document("dependencies: [http, \n");
    assert!(doc.model.is_none());
    assert!(diagnostics::compute(&doc, &pubdev).await.is_empty());
}
