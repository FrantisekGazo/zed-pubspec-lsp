use std::collections::HashMap;

use tower_lsp_server::ls_types::{
    CodeAction, CodeActionKind, Diagnostic, Position, Range, TextEdit, Uri, WorkspaceEdit,
};

use crate::diagnostics::SOURCE;

/// "Update to X" quickfix for one outdated-dependency diagnostic (built from
/// the `latest`/`caret` we stashed in `Diagnostic.data` — no refetch needed).
/// The caret is re-added only when the original constraint had one.
pub fn update_action(uri: &Uri, diagnostic: &Diagnostic) -> Option<CodeAction> {
    let new_text = diagnostic_replacement(diagnostic)?;
    let edit = TextEdit {
        range: diagnostic.range,
        new_text: new_text.clone(),
    };
    Some(CodeAction {
        title: format!("Update to {new_text}"),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        edit: Some(workspace_edit(uri, vec![edit])),
        is_preferred: Some(true),
        ..Default::default()
    })
}

/// One action updating every outdated dependency in the document at once.
/// `diagnostics` are the last published diagnostics for this document.
pub fn update_all_action(uri: &Uri, diagnostics: &[Diagnostic]) -> Option<CodeAction> {
    let edits: Vec<TextEdit> = diagnostics
        .iter()
        .filter(|d| d.source.as_deref() == Some(SOURCE))
        .filter_map(|d| {
            Some(TextEdit {
                range: d.range,
                new_text: diagnostic_replacement(d)?,
            })
        })
        .collect();
    if edits.len() < 2 {
        return None;
    }
    Some(CodeAction {
        title: format!("Update all dependencies to latest ({})", edits.len()),
        kind: Some(CodeActionKind::SOURCE),
        edit: Some(workspace_edit(uri, edits)),
        ..Default::default()
    })
}

/// The constraint text to write for an outdated diagnostic: `^<latest>` when
/// the original used a caret, otherwise the bare `<latest>`.
fn diagnostic_replacement(diagnostic: &Diagnostic) -> Option<String> {
    let data = diagnostic.data.as_ref()?;
    let latest = data.get("latest")?.as_str()?;
    let caret = data.get("caret").and_then(|c| c.as_bool()).unwrap_or(true);
    Some(if caret {
        format!("^{latest}")
    } else {
        latest.to_string()
    })
}

fn workspace_edit(uri: &Uri, edits: Vec<TextEdit>) -> WorkspaceEdit {
    WorkspaceEdit {
        changes: Some(HashMap::from([(uri.clone(), edits)])),
        ..Default::default()
    }
}

const SECTION_KEYS: [&str; 3] = ["dependencies", "dev_dependencies", "dependency_overrides"];

/// Sort the dependency section containing `cursor_line` alphabetically.
/// Works on raw lines (never re-emits YAML): an entry block is its key line
/// plus all following deeper-indented/blank lines, and contiguous comment
/// lines directly above a key travel with it. Returns None when the cursor is
/// not in a section or the section is already sorted.
pub fn sort_section_edit(text: &str, cursor_line: u32) -> Option<TextEdit> {
    let lines: Vec<&str> = text.lines().collect();
    let cursor_line = cursor_line as usize;

    // Locate the section header at or above the cursor.
    let header = (0..=cursor_line.min(lines.len().saturating_sub(1)))
        .rev()
        .find(|&i| !lines[i].starts_with(' ') && !lines[i].trim().is_empty())?;
    let header_key = lines[header].trim_end().split(':').next()?;
    if !SECTION_KEYS.contains(&header_key) {
        return None;
    }

    // Section body: everything up to the next top-level non-blank line.
    let body_start = header + 1;
    let body_end = (body_start..lines.len())
        .find(|&i| !lines[i].starts_with(' ') && !lines[i].trim().is_empty())
        .unwrap_or(lines.len());
    if cursor_line > body_end {
        return None;
    }
    let body = &lines[body_start..body_end];

    // Split into blocks, one per dependency entry. Comments directly above a
    // key travel with it; blank lines (and anything above them) trail the
    // preceding entry instead.
    let mut prefix: Vec<&str> = Vec::new(); // lines before the first entry
    let mut blocks: Vec<(String, Vec<&str>)> = Vec::new();
    let mut pending: Vec<&str> = Vec::new(); // comments/blanks not yet attached
    for &line in body {
        let trimmed = line.trim();
        if is_entry_key_line(line) {
            // The maximal trailing run of comment lines belongs to this key.
            let split = pending
                .iter()
                .rposition(|held| held.trim().is_empty())
                .map_or(0, |blank| blank + 1);
            let attached = pending.split_off(split);
            match blocks.last_mut() {
                Some((_, prev)) => prev.append(&mut pending),
                None => prefix.append(&mut pending),
            }
            let name = trimmed.split(':').next().unwrap_or("").to_string();
            let mut block = attached;
            block.push(line);
            blocks.push((name, block));
        } else if trimmed.starts_with('#') || trimmed.is_empty() {
            // Might belong to the next entry (comment header) — hold it.
            pending.push(line);
        } else if let Some((_, block)) = blocks.last_mut() {
            // Nested content (git/path/hosted details). Any held blank or
            // comment lines sit inside this entry, keep them with it.
            block.append(&mut pending);
            block.push(line);
        } else {
            // Nested content before any entry — malformed; bail out.
            return None;
        }
    }

    if blocks.len() < 2 {
        return None;
    }

    let mut sorted = blocks.clone();
    sorted.sort_by_key(|(name, _)| name.to_lowercase());
    if sorted.iter().map(|b| &b.0).eq(blocks.iter().map(|b| &b.0)) {
        return None;
    }

    // Trailing comments/blanks after the last entry stay at the end.
    let mut new_lines: Vec<&str> = prefix;
    new_lines.extend(sorted.into_iter().flat_map(|(_, block)| block));
    new_lines.extend(pending);

    let sorted_body_end = body_start + new_lines.len();
    debug_assert_eq!(sorted_body_end, body_end);
    Some(TextEdit {
        range: Range::new(
            Position::new(body_start as u32, 0),
            Position::new(
                (body_end - 1) as u32,
                lines[body_end - 1].encode_utf16().count() as u32,
            ),
        ),
        new_text: new_lines.join("\n"),
    })
}

/// A direct dependency key line: 1–3 spaces of indent, then `name:`.
fn is_entry_key_line(line: &str) -> bool {
    let indent = line.len() - line.trim_start().len();
    if !(1..=3).contains(&indent) || !line.starts_with(' ') {
        return false;
    }
    let rest = line.trim_start();
    let key_len = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    key_len > 0 && rest[key_len..].starts_with(':')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tower_lsp_server::ls_types::Uri;

    fn diagnostic(data: serde_json::Value) -> Diagnostic {
        Diagnostic {
            range: Range::default(),
            source: Some(SOURCE.into()),
            data: Some(data),
            ..Default::default()
        }
    }

    fn uri() -> Uri {
        "file:///pubspec.yaml".parse().unwrap()
    }

    fn action_edit_text(action: &CodeAction) -> String {
        let changes = action.edit.as_ref().unwrap().changes.as_ref().unwrap();
        changes.values().next().unwrap()[0].new_text.clone()
    }

    #[test]
    fn update_action_keeps_caret_when_original_had_one() {
        let action = update_action(
            &uri(),
            &diagnostic(json!({ "latest": "1.6.0", "caret": true })),
        )
        .unwrap();
        assert_eq!(action.title, "Update to ^1.6.0");
        assert_eq!(action_edit_text(&action), "^1.6.0");
    }

    #[test]
    fn update_action_omits_caret_when_original_had_none() {
        let action = update_action(
            &uri(),
            &diagnostic(json!({ "latest": "1.6.0", "caret": false })),
        )
        .unwrap();
        assert_eq!(action.title, "Update to 1.6.0");
        assert_eq!(action_edit_text(&action), "1.6.0");
    }

    #[test]
    fn update_action_defaults_to_caret_when_flag_absent() {
        let action = update_action(&uri(), &diagnostic(json!({ "latest": "1.6.0" }))).unwrap();
        assert_eq!(action_edit_text(&action), "^1.6.0");
    }

    fn apply(text: &str, edit: &TextEdit) -> String {
        let lines: Vec<&str> = text.lines().collect();
        let mut out: Vec<String> = lines[..edit.range.start.line as usize]
            .iter()
            .map(|l| l.to_string())
            .collect();
        out.push(edit.new_text.clone());
        out.extend(
            lines[(edit.range.end.line as usize + 1)..]
                .iter()
                .map(|l| l.to_string()),
        );
        out.join("\n") + if text.ends_with('\n') { "\n" } else { "" }
    }

    #[test]
    fn sorts_simple_section() {
        let text = "dependencies:\n  zeta: ^1.0.0\n  alpha: ^2.0.0\n";
        let edit = sort_section_edit(text, 1).expect("edit");
        assert_eq!(
            apply(text, &edit),
            "dependencies:\n  alpha: ^2.0.0\n  zeta: ^1.0.0\n"
        );
    }

    #[test]
    fn already_sorted_returns_none() {
        let text = "dependencies:\n  alpha: ^2.0.0\n  zeta: ^1.0.0\n";
        assert!(sort_section_edit(text, 1).is_none());
    }

    #[test]
    fn comments_travel_with_entries_and_blocks_stay_intact() {
        let text = "\
dependencies:
  # networking
  zeta:
    git:
      url: https://example.com/zeta

  alpha: ^2.0.0
name_after: x
";
        let edit = sort_section_edit(text, 2).expect("edit");
        let expected = "\
dependencies:
  alpha: ^2.0.0
  # networking
  zeta:
    git:
      url: https://example.com/zeta

name_after: x
";
        assert_eq!(apply(text, &edit), expected);
    }

    #[test]
    fn only_the_cursor_section_is_sorted() {
        let text = "\
dependencies:
  zeta: ^1.0.0
  alpha: ^2.0.0
dev_dependencies:
  beta: ^1.0.0
  alpha: ^1.0.0
";
        // Cursor in dev_dependencies (line 4).
        let edit = sort_section_edit(text, 4).expect("edit");
        let applied = apply(text, &edit);
        assert!(applied.contains("dependencies:\n  zeta: ^1.0.0\n  alpha: ^2.0.0\n"));
        assert!(applied.contains("dev_dependencies:\n  alpha: ^1.0.0\n  beta: ^1.0.0\n"));
    }

    #[test]
    fn cursor_outside_sections_returns_none() {
        let text = "name: app\ndependencies:\n  zeta: ^1\n  alpha: ^2\n";
        assert!(sort_section_edit(text, 0).is_none());
    }

    #[test]
    fn single_entry_returns_none() {
        let text = "dependencies:\n  alpha: ^2.0.0\n";
        assert!(sort_section_edit(text, 1).is_none());
    }
}
