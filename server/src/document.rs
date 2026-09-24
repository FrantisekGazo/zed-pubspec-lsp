use std::collections::HashMap;

use tokio::sync::RwLock;
use tower_lsp_server::ls_types::Position;

use crate::pubspec::PubspecModel;

#[derive(Debug, Clone)]
pub struct Document {
    pub text: String,
    pub version: i32,
    pub model: Option<PubspecModel>,
}

#[derive(Default)]
pub struct DocumentStore {
    docs: RwLock<HashMap<String, Document>>,
}

impl DocumentStore {
    pub async fn upsert(&self, uri: &str, text: String, version: i32) {
        let model = PubspecModel::parse(&text);
        self.docs.write().await.insert(
            uri.to_string(),
            Document {
                text,
                version,
                model,
            },
        );
    }

    pub async fn remove(&self, uri: &str) {
        self.docs.write().await.remove(uri);
    }

    pub async fn get(&self, uri: &str) -> Option<Document> {
        self.docs.read().await.get(uri).cloned()
    }
}

/// Whether a document URI names a pubspec. The server is attached to every
/// YAML file, so everything else is ignored.
pub fn is_pubspec_uri(uri: &str) -> bool {
    let file_name = uri.rsplit('/').next().unwrap_or(uri);
    matches!(file_name, "pubspec.yaml" | "pubspec_overrides.yaml")
}

/// Convert a marked-yaml marker (1-based line/column, counted in characters)
/// to an LSP position (0-based line, UTF-16 code-unit column).
pub fn lsp_position(text: &str, line1: usize, col1: usize) -> Position {
    let line0 = line1.saturating_sub(1);
    let line = text.lines().nth(line0).unwrap_or("");
    let character: usize = line
        .chars()
        .take(col1.saturating_sub(1))
        .map(char::len_utf16)
        .sum();
    Position::new(line0 as u32, character as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_position_ascii() {
        assert_eq!(lsp_position("abc\ndef", 2, 2), Position::new(1, 1));
    }

    #[test]
    fn lsp_position_non_bmp() {
        // '😀' is one char but two UTF-16 code units.
        assert_eq!(lsp_position("😀abc", 1, 3), Position::new(0, 3));
    }

    #[test]
    fn lsp_position_out_of_bounds_line() {
        assert_eq!(lsp_position("abc", 9, 1), Position::new(8, 0));
    }

    #[test]
    fn pubspec_uris_are_accepted() {
        assert!(is_pubspec_uri("file:///app/pubspec.yaml"));
        assert!(is_pubspec_uri(
            "file:///app/packages/core/pubspec_overrides.yaml"
        ));
        assert!(is_pubspec_uri("file:///C:/app/pubspec.yaml"));
    }

    #[test]
    fn other_yaml_uris_are_rejected() {
        assert!(!is_pubspec_uri("file:///app/analysis_options.yaml"));
        assert!(!is_pubspec_uri("file:///app/.github/workflows/ci.yml"));
        assert!(!is_pubspec_uri("file:///app/my_pubspec.yaml"));
        assert!(!is_pubspec_uri("file:///app/pubspec.yaml.bak"));
    }
}
