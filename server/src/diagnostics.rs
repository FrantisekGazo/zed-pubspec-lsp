use futures::stream::{self, StreamExt};
use serde_json::json;
use tower_lsp_server::ls_types::{Diagnostic, DiagnosticSeverity};

use crate::document::Document;
use crate::pubdev::PubDevClient;
use crate::pubspec::{DepEntry, DepSource, SectionKind};
use crate::versions::is_outdated;

pub const SOURCE: &str = "pubspec-lsp";
const MAX_CONCURRENT_FETCHES: usize = 8;

/// Compute diagnostics for every hosted dependency. Network failures and
/// unknown packages contribute no diagnostics — this must never get loud
/// when offline.
pub async fn compute(doc: &Document, pubdev: &PubDevClient) -> Vec<Diagnostic> {
    let Some(model) = doc.model.as_ref() else {
        return Vec::new();
    };

    let mut tasks = Vec::new();
    for section in &model.sections {
        // Overrides are deliberate pins; nagging about them being outdated
        // would be noise. Discontinued warnings still apply.
        let check_outdated = section.kind != SectionKind::DependencyOverrides;
        for entry in &section.entries {
            if let DepSource::Hosted { .. } = entry.source {
                tasks.push(check_entry(entry, pubdev, check_outdated));
            }
        }
    }

    let mut diagnostics: Vec<Diagnostic> = stream::iter(tasks)
        .buffer_unordered(MAX_CONCURRENT_FETCHES)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect();
    // Deterministic order for tests and stable client rendering.
    diagnostics.sort_by_key(|d| (d.range.start.line, d.range.start.character));
    diagnostics
}

async fn check_entry(
    entry: &DepEntry,
    pubdev: &PubDevClient,
    check_outdated: bool,
) -> Vec<Diagnostic> {
    let Some(info) = pubdev.package_info(&entry.name).await else {
        return Vec::new();
    };
    let mut result = Vec::new();

    if info.is_discontinued {
        let mut message = format!("Package '{}' is discontinued", entry.name);
        if let Some(replacement) = &info.replaced_by {
            message.push_str(&format!(", replaced by '{replacement}'"));
        }
        result.push(Diagnostic {
            range: entry.name_range,
            severity: Some(DiagnosticSeverity::WARNING),
            source: Some(SOURCE.into()),
            message,
            ..Default::default()
        });
    }

    if check_outdated {
        if let DepSource::Hosted {
            constraint,
            constraint_range,
        } = &entry.source
        {
            if is_outdated(constraint.as_deref(), &info.latest) == Some(true) {
                result.push(Diagnostic {
                    range: constraint_range.unwrap_or(entry.name_range),
                    severity: Some(DiagnosticSeverity::HINT),
                    source: Some(SOURCE.into()),
                    message: format!("Newer version available: {}", info.latest),
                    // Consumed by the "update to latest" code action, so it
                    // doesn't need to refetch. `caret` mirrors the original
                    // constraint so the update preserves (or omits) the caret.
                    data: Some(json!({
                        "latest": info.latest,
                        "caret": uses_caret(constraint.as_deref()),
                    })),
                    ..Default::default()
                });
            }
        }
    }

    result
}

/// Whether a constraint pins with a caret (`^1.2.0`), ignoring surrounding
/// whitespace and quotes. Determines whether the "update" action re-adds one.
fn uses_caret(constraint: Option<&str>) -> bool {
    constraint
        .map(|c| {
            c.trim()
                .trim_matches(|ch| ch == '"' || ch == '\'')
                .trim_start()
                .starts_with('^')
        })
        .unwrap_or(false)
}
