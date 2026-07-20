# pubspec-lsp — Zed extension for pubspec.yaml

A Zed extension that provides language-server support for Flutter/Dart `pubspec.yaml` files, filling the gap that "Pubspec Assist" covers in VS Code.

## Features

- **Package-name completion** in `dependencies` / `dev_dependencies` / `dependency_overrides`, backed by the pub.dev popularity-ranked name list (cached 8 h, works offline once fetched).
- **Version completion** for a package: `^latest` first, then recent versions (retracted versions skipped).
- **Diagnostics**: outdated dependencies (hint with the latest available version) and discontinued packages (warning, with the suggested replacement). Dependency overrides only get discontinued warnings — pins there are deliberate.
- **Code actions**: *Update to `^<latest>`* (quickfix on an outdated dependency), *Update all dependencies to latest*, and *Sort dependencies alphabetically* (preserves comments and git/path blocks).
- **Hover** on a package name: description, latest version, link to its pub.dev page.

`pubspec_overrides.yaml` is covered too. Git/path/SDK dependencies are recognized and left alone (no version noise), but still participate in sorting and hover.

## How it works

Two parts, one repo:

1. **The Zed extension** (`src/lib.rs`) — Rust/WASM glue using [`zed_extension_api`](https://docs.rs/zed_extension_api). It registers a custom `Pubspec` language scoped to `pubspec.yaml` / `pubspec_overrides.yaml` via `path_suffixes` (Zed picks the language with the longest matching suffix, so regular YAML files are untouched), and locates or downloads the language server.
2. **The language server** (`server/`) — a standalone Rust binary (`pubspec-language-server`) built on `tower-lsp-server`, talking to the pub.dev API with in-memory + on-disk caching and silent offline degradation.

Binary resolution order: `lsp.pubspec-lsp.binary.path` from Zed settings → `pubspec-language-server` on PATH → previously downloaded binary → download from this repo's GitHub releases for the current platform.

## Development

```sh
# Build & test the language server
cargo build -p pubspec-language-server
cargo test -p pubspec-language-server

# Make the dev binary findable (or set lsp.pubspec-lsp.binary.path in Zed settings)
export PATH="$PWD/target/debug:$PATH"
```

Then in Zed: command palette → `zed: install dev extension` → select this directory. Open any `pubspec.yaml`; the status bar should show the `Pubspec` language and the `Pubspec LSP` server.

To point Zed at a specific server binary:

```jsonc
// Zed settings.json
{
  "lsp": {
    "pubspec-lsp": {
      "binary": { "path": "/path/to/target/debug/pubspec-language-server" }
    }
  }
}
```

### Logs

The server logs to stderr (visible via Zed's `debug: open language server logs`). Set `RUST_LOG=pubspec_language_server=debug` for more detail.

## Releasing

**1. Publish.** Bump the same version in `Cargo.toml`, `extension.toml`, and
`server/Cargo.toml` (they must match — the server download URL is built from the
extension version), then tag and push:

```sh
git tag -a vX.Y.Z -m "vX.Y.Z" && git push origin master vX.Y.Z
```

The tag triggers `release.yml`, which attaches the per-platform server archives.
Wait for all 5 assets on the release.

**2. Update the registry.** In a fork of `zed-industries/extensions`, point the
`extensions/pubspec-lsp` submodule at the new tag and bump the `version` under
`[pubspec-lsp]` in `extensions.toml` (keep it alphabetically sorted), then open a
PR. No auto-bump bot applies here — `zed-zippy` only runs for repos in the
`zed-industries`/`zed-extensions` orgs. First PR: zed-industries/extensions#6896.

## Notes

- Requires a Zed version with longest-suffix language matching (zed-industries/zed#29716, 2025).
- pub.dev etiquette: descriptive `User-Agent`, the name-completion list is cached ≥ 8 h as requested by the API's `cache-control`, package metadata 15 min. Offline = features silently degrade, never errors.

## References

- Zed extension docs: <https://zed.dev/docs/extensions/developing-extensions>
- Language extensions / language servers: <https://zed.dev/docs/extensions/languages>
- pub.dev API: <https://pub.dev/help/api>
- Prior art: Pubspec Assist (VS Code), <https://github.com/jeroen-meijer/pubspec_assist>
