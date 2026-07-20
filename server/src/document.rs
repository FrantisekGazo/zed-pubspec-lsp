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
}
