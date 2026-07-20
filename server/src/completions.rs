use tower_lsp_server::ls_types::{
    CompletionItem, CompletionItemKind, CompletionTextEdit, Position, TextEdit,
};

use crate::context::{completion_context, CompletionContext};
use crate::document::Document;
use crate::pubdev::{PackageInfo, PubDevClient};

const MAX_NAME_ITEMS: usize = 50;
const MAX_VERSION_ITEMS: usize = 20;

pub async fn completions(
    doc: &Document,
    pos: Position,
    pubdev: &PubDevClient,
) -> Option<Vec<CompletionItem>> {
    match completion_context(&doc.text, pos)? {
        CompletionContext::PackageName {
            prefix,
            replace_range,
            needs_colon,
        } => {
            let names = pubdev.package_names().await?;
            Some(
                filter_names(&names, &prefix)
                    .into_iter()
                    .enumerate()
                    .map(|(i, name)| {
                        let insert = if needs_colon {
                            format!("{name}: ")
                        } else {
                            name.to_string()
                        };
                        CompletionItem {
                            label: name.to_string(),
                            kind: Some(CompletionItemKind::MODULE),
                            // Preserve popularity ranking against client-side
                            // alphabetical re-sorting.
                            sort_text: Some(format!("{i:05}")),
                            filter_text: Some(name.to_string()),
                            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                                range: replace_range,
                                new_text: insert,
                            })),
                            ..Default::default()
                        }
                    })
                    .collect(),
            )
        }
        CompletionContext::Version {
            package,
            prefix,
            replace_range,
            needs_space,
        } => {
            let info = pubdev.package_info(&package).await?;
            Some(version_items(&info, &prefix, replace_range, needs_space))
        }
    }
}

/// Prefix matches first (in popularity order), then substring matches.
fn filter_names<'a>(names: &'a [String], prefix: &str) -> Vec<&'a str> {
    let needle = prefix.to_ascii_lowercase();
    let mut starts: Vec<&str> = Vec::new();
    let mut contains: Vec<&str> = Vec::new();
    for name in names {
        if starts.len() >= MAX_NAME_ITEMS {
            break;
        }
        if name.starts_with(&needle) {
            starts.push(name);
        } else if !needle.is_empty() && name.contains(&needle) {
            contains.push(name);
        }
    }
    starts
        .into_iter()
        .chain(contains)
        .take(MAX_NAME_ITEMS)
        .collect()
}

fn version_items(
    info: &PackageInfo,
    prefix: &str,
    replace_range: tower_lsp_server::ls_types::Range,
    needs_space: bool,
) -> Vec<CompletionItem> {
    let space = if needs_space { " " } else { "" };
    // The user may have typed a caret already; don't double it.
    let caret = if prefix.trim_start().starts_with('^') {
        ""
    } else {
        "^"
    };

    // Newest first; the API lists versions oldest first.
    let recent = info
        .versions
        .iter()
        .rev()
        .filter(|v| !v.retracted && v.version != info.latest)
        .take(MAX_VERSION_ITEMS - 1);

    std::iter::once((info.latest.as_str(), true))
        .chain(recent.map(|v| (v.version.as_str(), false)))
        .enumerate()
        .map(|(i, (version, is_latest))| {
            let constraint = format!("^{version}");
            CompletionItem {
                label: constraint.clone(),
                label_details: is_latest.then(|| {
                    tower_lsp_server::ls_types::CompletionItemLabelDetails {
                        detail: None,
                        description: Some("latest".into()),
                    }
                }),
                kind: Some(CompletionItemKind::VALUE),
                sort_text: Some(format!("{i:05}")),
                filter_text: Some(constraint.clone()),
                preselect: is_latest.then_some(true),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range: replace_range,
                    new_text: format!("{space}{caret}{version}"),
                })),
                ..Default::default()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pubdev::VersionEntry;
    use tower_lsp_server::ls_types::Range;

    fn info() -> PackageInfo {
        PackageInfo {
            name: "http".into(),
            description: None,
            latest: "1.6.0".into(),
            versions: ["0.13.6", "1.5.0", "1.6.0-beta", "1.6.0"]
                .into_iter()
                .map(|v| VersionEntry {
                    version: v.into(),
                    retracted: v == "1.6.0-beta",
                })
                .collect(),
            is_discontinued: false,
            replaced_by: None,
        }
    }

    #[test]
    fn filter_names_prefix_before_substring() {
        let names: Vec<String> = ["http", "dio", "http_parser", "chopper_http"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            filter_names(&names, "http"),
            vec!["http", "http_parser", "chopper_http"]
        );
        // Empty prefix: everything, popularity order.
        assert_eq!(filter_names(&names, "").len(), 4);
    }

    #[test]
    fn version_items_latest_first_skips_retracted() {
        let items = version_items(&info(), "", Range::default(), false);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["^1.6.0", "^1.5.0", "^0.13.6"]);
        assert_eq!(items[0].preselect, Some(true));
        assert_eq!(items[0].sort_text.as_deref(), Some("00000"));
    }

    #[test]
    fn version_items_respect_typed_caret_and_space() {
        let items = version_items(&info(), "^1", Range::default(), false);
        let edit = match items[0].text_edit.as_ref().unwrap() {
            CompletionTextEdit::Edit(edit) => edit,
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(edit.new_text, "1.6.0"); // caret already typed

        let items = version_items(&info(), "", Range::default(), true);
        let edit = match items[0].text_edit.as_ref().unwrap() {
            CompletionTextEdit::Edit(edit) => edit,
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(edit.new_text, " ^1.6.0"); // no space after colon yet
    }
}
