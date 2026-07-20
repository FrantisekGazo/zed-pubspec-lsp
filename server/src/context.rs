use tower_lsp_server::ls_types::{Position, Range};

/// What kind of completion the cursor position calls for.
///
/// This is deliberately line-based rather than model-based: while the user is
/// typing (`  htt` under `dependencies:`) the buffer is usually not valid
/// YAML, so the parsed model can't be relied on here.
#[derive(Debug, Clone, PartialEq)]
pub enum CompletionContext {
    PackageName {
        prefix: String,
        replace_range: Range,
        /// True when the line has no `:` yet, so the completion should append
        /// one (`http` → `http: `).
        needs_colon: bool,
    },
    Version {
        package: String,
        prefix: String,
        replace_range: Range,
        /// True when there is no space between the `:` and the cursor, so the
        /// completion should insert one (`http:` → `http: ^1.6.0`).
        needs_space: bool,
    },
}

const SECTION_KEYS: [&str; 3] = ["dependencies", "dev_dependencies", "dependency_overrides"];

pub fn completion_context(text: &str, pos: Position) -> Option<CompletionContext> {
    let lines: Vec<&str> = text.lines().collect();
    let line = *lines.get(pos.line as usize)?;

    if !in_dependency_section(&lines, pos.line as usize) {
        return None;
    }

    let chars: Vec<char> = line.chars().collect();
    let cursor = utf16_to_char_idx(&chars, pos.character as usize);

    let indent = chars.iter().take_while(|c| **c == ' ').count();
    // Direct children of the section sit at one indent level (conventionally
    // 2 spaces). Deeper lines are inside a git/path/hosted block.
    if !(1..=3).contains(&indent) || cursor < indent {
        return None;
    }

    // Key token: identifier chars starting at the indent.
    let key_end = (indent..chars.len())
        .find(|&i| !is_package_char(chars[i]))
        .unwrap_or(chars.len());

    if cursor <= key_end {
        // Cursor within (or right after) the key token, before any `:`.
        let prefix: String = chars[indent..cursor].iter().collect();
        let needs_colon = !chars[key_end..].contains(&':');
        return Some(CompletionContext::PackageName {
            prefix,
            replace_range: char_span_to_range(&chars, pos.line, indent, key_end),
            needs_colon,
        });
    }

    // Version position: `  <name>: <cursor>` — cursor after the colon.
    if chars.get(key_end).copied() != Some(':') || cursor <= key_end {
        return None;
    }
    let value_start = (key_end + 1..chars.len())
        .find(|&i| chars[i] != ' ')
        .unwrap_or(chars.len());
    if cursor < value_start.min(chars.len()) && cursor <= key_end + 1 {
        return None; // cursor directly on the colon
    }
    let value_end = (value_start..chars.len())
        .find(|&i| chars[i] == '#')
        .map(|i| {
            // Trim trailing spaces before an inline comment.
            (value_start..i)
                .rev()
                .find(|&j| chars[j] != ' ')
                .map_or(value_start, |j| j + 1)
        })
        .unwrap_or(chars.len());
    if cursor > value_end.max(value_start) {
        return None;
    }
    let package: String = chars[indent..key_end].iter().collect();
    if package.is_empty() {
        return None;
    }
    let prefix: String = chars[value_start.min(cursor)..cursor].iter().collect();
    Some(CompletionContext::Version {
        package,
        prefix,
        replace_range: char_span_to_range(&chars, pos.line, value_start.min(cursor), value_end),
        needs_space: value_start == key_end + 1,
    })
}

/// Walk upward from the cursor line to find the governing top-level key;
/// true when it is one of the dependency sections.
fn in_dependency_section(lines: &[&str], cursor_line: usize) -> bool {
    for line in lines[..cursor_line].iter().rev() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() || trimmed.trim_start().starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') {
            // Top-level key reached — the nearest one governs the cursor.
            let key = trimmed.split(':').next().unwrap_or("");
            return SECTION_KEYS.contains(&key);
        }
    }
    false
}

fn is_package_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn utf16_to_char_idx(chars: &[char], utf16_col: usize) -> usize {
    let mut units = 0;
    for (i, c) in chars.iter().enumerate() {
        if units >= utf16_col {
            return i;
        }
        units += c.len_utf16();
    }
    chars.len()
}

fn char_span_to_range(chars: &[char], line: u32, start: usize, end: usize) -> Range {
    let utf16_at = |idx: usize| -> u32 { chars[..idx].iter().map(|c| c.len_utf16() as u32).sum() };
    Range::new(
        Position::new(line, utf16_at(start)),
        Position::new(line, utf16_at(end.max(start))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "\
name: my_app
dependencies:
  http: ^1.2.0
  coll
  provider:
  my_git_dep:
    git:
      url: https://example.com/repo
dev_dependencies:
  test: any
";

    fn ctx(line: u32, character: u32) -> Option<CompletionContext> {
        completion_context(DOC, Position::new(line, character))
    }

    #[test]
    fn package_name_partial_token() {
        // "  coll" with cursor at end
        match ctx(3, 6) {
            Some(CompletionContext::PackageName {
                prefix,
                needs_colon,
                replace_range,
            }) => {
                assert_eq!(prefix, "coll");
                assert!(needs_colon);
                assert_eq!(
                    replace_range,
                    Range::new(Position::new(3, 2), Position::new(3, 6))
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn package_name_mid_token_keeps_full_replace_range() {
        // cursor after "co" in "  coll"
        match ctx(3, 4) {
            Some(CompletionContext::PackageName {
                prefix,
                replace_range,
                ..
            }) => {
                assert_eq!(prefix, "co");
                assert_eq!(replace_range.end.character, 6);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn package_name_with_existing_colon() {
        // cursor inside "http" on "  http: ^1.2.0"
        match ctx(2, 4) {
            Some(CompletionContext::PackageName {
                prefix,
                needs_colon,
                ..
            }) => {
                assert_eq!(prefix, "ht");
                assert!(!needs_colon);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn version_after_colon() {
        // "  http: ^1.2.0", cursor after "^1."
        match ctx(2, 11) {
            Some(CompletionContext::Version {
                package,
                prefix,
                replace_range,
                needs_space,
            }) => {
                assert_eq!(package, "http");
                assert_eq!(prefix, "^1.");
                assert_eq!(
                    replace_range,
                    Range::new(Position::new(2, 8), Position::new(2, 14))
                );
                assert!(!needs_space);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn version_on_empty_value() {
        // "  provider:" with cursor right after the colon
        match ctx(4, 11) {
            Some(CompletionContext::Version {
                package,
                prefix,
                needs_space,
                ..
            }) => {
                assert_eq!(package, "provider");
                assert_eq!(prefix, "");
                assert!(needs_space);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn dev_dependencies_section_works() {
        match ctx(9, 4) {
            Some(CompletionContext::PackageName { prefix, .. }) => assert_eq!(prefix, "te"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn no_completion_outside_sections() {
        assert_eq!(ctx(0, 3), None); // top-level "name: my_app"
    }

    #[test]
    fn no_completion_in_nested_blocks() {
        assert_eq!(ctx(6, 6), None); // "    git:"
        assert_eq!(ctx(7, 10), None); // "      url: ..."
    }

    #[test]
    fn no_completion_on_section_header_line() {
        assert_eq!(ctx(1, 5), None); // "dependencies:" itself is top-level
    }
}
