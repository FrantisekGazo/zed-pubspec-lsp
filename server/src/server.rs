use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::{
    CodeActionOrCommand, CodeActionParams, CodeActionProviderCapability, CodeActionResponse,
    CompletionOptions, CompletionParams, CompletionResponse, Diagnostic,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams, Hover,
    HoverParams, HoverProviderCapability, InitializeParams, InitializeResult, ServerCapabilities,
    ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};
use tower_lsp_server::{Client, LanguageServer};

use crate::actions;
use crate::completions;
use crate::diagnostics;
use crate::document::DocumentStore;
use crate::hover;
use crate::pubdev::{PubDevClient, PUB_DEV_URL};

const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(500);

pub struct Backend {
    client: Client,
    docs: Arc<DocumentStore>,
    pubdev: Arc<PubDevClient>,
    /// Per-document change counter; a scheduled diagnostics run only
    /// publishes if its document hasn't changed since it was scheduled.
    diag_generations: Arc<Mutex<HashMap<String, u64>>>,
    /// Last published diagnostics per document, feeding the
    /// "update all dependencies" code action.
    published: Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            docs: Arc::new(DocumentStore::default()),
            pubdev: Arc::new(PubDevClient::new(PUB_DEV_URL)),
            diag_generations: Arc::new(Mutex::new(HashMap::new())),
            published: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn schedule_diagnostics(&self, uri: Uri) {
        let generation = {
            let mut generations = self.diag_generations.lock().unwrap();
            let counter = generations.entry(uri.as_str().to_string()).or_insert(0);
            *counter += 1;
            *counter
        };

        let docs = Arc::clone(&self.docs);
        let pubdev = Arc::clone(&self.pubdev);
        let client = self.client.clone();
        let published = Arc::clone(&self.published);
        let key = uri.as_str().to_string();
        let generations = Arc::clone(&self.diag_generations);
        let is_current = {
            let key = key.clone();
            move || generations.lock().unwrap().get(&key).copied() == Some(generation)
        };

        tokio::spawn(async move {
            tokio::time::sleep(DIAGNOSTICS_DEBOUNCE).await;
            if !is_current() {
                return;
            }
            let Some(doc) = docs.get(uri.as_str()).await else {
                return;
            };
            let diags = diagnostics::compute(&doc, &pubdev).await;
            if !is_current() {
                return;
            }
            published.lock().unwrap().insert(key, diags.clone());
            client
                .publish_diagnostics(uri, diags, Some(doc.version))
                .await;
        });
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                // Pubspec files are tiny; full-document sync keeps things simple.
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![":".into(), " ".into(), "^".into()]),
                    ..Default::default()
                }),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "pubspec-language-server".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            offset_encoding: None,
        })
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        self.docs
            .upsert(doc.uri.as_str(), doc.text, doc.version)
            .await;
        self.schedule_diagnostics(doc.uri);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // FULL sync: the last change carries the whole document.
        let Some(change) = params.content_changes.into_iter().next_back() else {
            return;
        };
        self.docs
            .upsert(
                params.text_document.uri.as_str(),
                change.text,
                params.text_document.version,
            )
            .await;
        self.schedule_diagnostics(params.text_document.uri);
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.docs.remove(uri.as_str()).await;
        self.diag_generations.lock().unwrap().remove(uri.as_str());
        self.published.lock().unwrap().remove(uri.as_str());
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let mut response: Vec<CodeActionOrCommand> = Vec::new();

        // Quickfix per outdated-dependency diagnostic under the cursor.
        for diagnostic in &params.context.diagnostics {
            if diagnostic.source.as_deref() == Some(diagnostics::SOURCE) {
                if let Some(action) = actions::update_action(&uri, diagnostic) {
                    response.push(CodeActionOrCommand::CodeAction(action));
                }
            }
        }

        // Update-everything action, from the last published diagnostics.
        let published = self
            .published
            .lock()
            .unwrap()
            .get(uri.as_str())
            .cloned()
            .unwrap_or_default();
        if let Some(action) = actions::update_all_action(&uri, &published) {
            response.push(CodeActionOrCommand::CodeAction(action));
        }

        // Sort the section under the cursor.
        if let Some(doc) = self.docs.get(uri.as_str()).await {
            if let Some(edit) = actions::sort_section_edit(&doc.text, params.range.start.line) {
                response.push(CodeActionOrCommand::CodeAction(
                    tower_lsp_server::ls_types::CodeAction {
                        title: "Sort dependencies alphabetically".into(),
                        kind: Some(tower_lsp_server::ls_types::CodeActionKind::REFACTOR_REWRITE),
                        edit: Some(tower_lsp_server::ls_types::WorkspaceEdit {
                            changes: Some(HashMap::from([(uri.clone(), vec![edit])])),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ));
            }
        }

        Ok((!response.is_empty()).then_some(response))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let position = params.text_document_position.position;
        let uri = params.text_document_position.text_document.uri;
        let Some(doc) = self.docs.get(uri.as_str()).await else {
            return Ok(None);
        };
        Ok(completions::completions(&doc, position, &self.pubdev)
            .await
            .map(CompletionResponse::Array))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let Some(doc) = self.docs.get(uri.as_str()).await else {
            return Ok(None);
        };
        Ok(hover::hover(
            &doc,
            params.text_document_position_params.position,
            &self.pubdev,
        )
        .await)
    }
}
