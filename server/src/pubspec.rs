use marked_yaml::types::{MarkedScalarNode, Node};
use tower_lsp_server::ls_types::{Position, Range};

use crate::document::lsp_position;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    Dependencies,
    DevDependencies,
    DependencyOverrides,
}

impl SectionKind {
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "dependencies" => Some(Self::Dependencies),
            "dev_dependencies" => Some(Self::DevDependencies),
            "dependency_overrides" => Some(Self::DependencyOverrides),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum DepSource {
    /// A pub.dev-hosted dependency: `http: ^1.0.0`, `http:` or
    /// `http: { hosted: ..., version: ^1.0.0 }`.
    Hosted {
        constraint: Option<String>,
        constraint_range: Option<Range>,
    },
    /// `flutter: { sdk: flutter }`
    Sdk(String),
    Git,
    Path,
    /// Anything unexpected (e.g. a sequence value) — excluded from all features.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct DepEntry {
    pub name: String,
    /// Range of the package-name key, in LSP (UTF-16) coordinates.
    pub name_range: Range,
    pub source: DepSource,
}

#[derive(Debug, Clone)]
pub struct DepSection {
    pub kind: SectionKind,
    pub entries: Vec<DepEntry>,
}

#[derive(Debug, Clone)]
pub struct PubspecModel {
    pub sections: Vec<DepSection>,
}

impl PubspecModel {
    /// Parse a pubspec document. Returns None when the YAML is invalid or the
    /// root is not a mapping (typical mid-edit states).
    pub fn parse(text: &str) -> Option<Self> {
        let root = marked_yaml::parse_yaml(0, text).ok()?;
        let map = root.as_mapping()?;

        let mut sections = Vec::new();
        for (key, value) in map.iter() {
            let Some(kind) = SectionKind::from_key(key.as_str()) else {
                continue;
            };
            let mut entries = Vec::new();
            if let Some(deps) = value.as_mapping() {
                for (dep_key, dep_value) in deps.iter() {
                    let Some(name_range) = scalar_range(text, dep_key) else {
                        continue;
                    };
                    entries.push(DepEntry {
                        name: dep_key.as_str().to_string(),
                        name_range,
                        source: classify(text, dep_value),
                    });
                }
            }
            sections.push(DepSection { kind, entries });
        }
        Some(Self { sections })
    }

    /// Find the dependency entry whose name range contains `pos`.
    pub fn entry_at(&self, pos: Position) -> Option<(&DepSection, &DepEntry)> {
        self.sections.iter().find_map(|section| {
            section
                .entries
                .iter()
                .find(|entry| range_contains(entry.name_range, pos))
                .map(|entry| (section, entry))
        })
    }
}

pub fn range_contains(range: Range, pos: Position) -> bool {
    (range.start.line < pos.line
        || (range.start.line == pos.line && range.start.character <= pos.character))
        && (pos.line < range.end.line
            || (pos.line == range.end.line && pos.character <= range.end.character))
}

fn classify(text: &str, node: &Node) -> DepSource {
    match node {
        Node::Scalar(scalar) => hosted_from_scalar(text, scalar),
        Node::Mapping(map) => {
            if let Some(sdk) = map.get_scalar("sdk") {
                DepSource::Sdk(sdk.as_str().to_string())
            } else if map.get_node("git").is_some() {
                DepSource::Git
            } else if map.get_node("path").is_some() {
                DepSource::Path
            } else if let Some(version) = map.get_scalar("version") {
                hosted_from_scalar(text, version)
            } else {
                DepSource::Hosted {
                    constraint: None,
                    constraint_range: None,
                }
            }
        }
        _ => DepSource::Unknown,
    }
}

fn hosted_from_scalar(text: &str, scalar: &MarkedScalarNode) -> DepSource {
    let raw = scalar.as_str().trim();
    if raw.is_empty() {
        DepSource::Hosted {
            constraint: None,
            constraint_range: None,
        }
    } else {
        DepSource::Hosted {
            constraint: Some(raw.to_string()),
            constraint_range: scalar_range(text, scalar),
        }
    }
}

/// LSP range of a scalar node. Falls back to start + scalar length when the
/// parser did not record an end marker.
fn scalar_range(text: &str, node: &MarkedScalarNode) -> Option<Range> {
    let start = node.span().start()?;
    let start_pos = lsp_position(text, start.line(), start.column());
    let end_pos = match node.span().end() {
        Some(end) => lsp_position(text, end.line(), end.column()),
        None => Position::new(
            start_pos.line,
            start_pos.character + node.as_str().encode_utf16().count() as u32,
        ),
    };
    Some(Range::new(start_pos, end_pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"name: my_app
description: A sample app.
environment:
  sdk: ">=3.0.0 <4.0.0"

dependencies:
  flutter:
    sdk: flutter
  http: ^1.2.0
  collection: "1.18.0"
  pinned_any:
  my_git_dep:
    git:
      url: https://github.com/a/b
      ref: main
  my_path_dep:
    path: ../local
  custom_hosted:
    hosted: https://my-mirror.dev
    version: ^2.0.0

dev_dependencies:
  test: any
"#;

    fn model() -> PubspecModel {
        PubspecModel::parse(FIXTURE).expect("fixture parses")
    }

    fn section(kind: SectionKind) -> DepSection {
        model()
            .sections
            .into_iter()
            .find(|s| s.kind == kind)
            .expect("section present")
    }

    fn entry(name: &str) -> DepEntry {
        model()
            .sections
            .into_iter()
            .flat_map(|s| s.entries)
            .find(|e| e.name == name)
            .expect("entry present")
    }

    #[test]
    fn finds_sections_and_entries() {
        let m = model();
        assert_eq!(m.sections.len(), 2);
        assert_eq!(section(SectionKind::Dependencies).entries.len(), 7);
        assert_eq!(section(SectionKind::DevDependencies).entries.len(), 1);
    }

    #[test]
    fn classifies_sources() {
        assert!(matches!(entry("flutter").source, DepSource::Sdk(ref s) if s == "flutter"));
        assert!(matches!(entry("my_git_dep").source, DepSource::Git));
        assert!(matches!(entry("my_path_dep").source, DepSource::Path));
        assert!(matches!(
            entry("http").source,
            DepSource::Hosted { constraint: Some(ref c), .. } if c == "^1.2.0"
        ));
        assert!(matches!(
            entry("collection").source,
            DepSource::Hosted { constraint: Some(ref c), .. } if c == "1.18.0"
        ));
        assert!(matches!(
            entry("pinned_any").source,
            DepSource::Hosted {
                constraint: None,
                ..
            }
        ));
        assert!(matches!(
            entry("custom_hosted").source,
            DepSource::Hosted { constraint: Some(ref c), .. } if c == "^2.0.0"
        ));
    }

    #[test]
    fn name_range_covers_key() {
        let http = entry("http");
        // "  http: ^1.2.0" is line 9 (index 8), key at columns 2..6.
        assert_eq!(http.name_range.start, Position::new(8, 2));
        assert_eq!(http.name_range.end, Position::new(8, 6));
    }

    #[test]
    fn entry_at_hits_name_only() {
        let m = model();
        let (_, e) = m.entry_at(Position::new(8, 4)).expect("hit http");
        assert_eq!(e.name, "http");
        assert!(m.entry_at(Position::new(8, 10)).is_none()); // inside version
        assert!(m.entry_at(Position::new(0, 2)).is_none()); // top-level name:
    }

    #[test]
    fn invalid_yaml_returns_none() {
        assert!(PubspecModel::parse("dependencies: [http, \n").is_none());
    }

    #[test]
    fn non_mapping_root_returns_none() {
        assert!(PubspecModel::parse("- a\n- b\n").is_none());
    }

    #[test]
    fn empty_sections_are_kept() {
        let m = PubspecModel::parse("dependencies:\n").expect("parses");
        assert_eq!(m.sections.len(), 1);
        assert!(m.sections[0].entries.is_empty());
    }
}
