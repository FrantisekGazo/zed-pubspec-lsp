use zed_extension_api::settings::LspSettings;
use zed_extension_api::{self as zed, Result};

const SERVER_BINARY: &str = "pubspec-language-server";
const GITHUB_REPO: &str = "FrantisekGazo/pubspec-lsp";

struct PubspecExtension {
    cached_binary_path: Option<String>,
}

impl PubspecExtension {
    fn language_server_binary_path(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<String> {
        // Dev override: a binary on PATH beats downloading.
        if let Some(path) = worktree.which(SERVER_BINARY) {
            return Ok(path);
        }

        if let Some(path) = &self.cached_binary_path {
            if std::fs::metadata(path).is_ok_and(|stat| stat.is_file()) {
                return Ok(path.clone());
            }
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );
        let release = zed::latest_github_release(
            GITHUB_REPO,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let (platform, arch) = zed::current_platform();
        let target = match (platform, arch) {
            (zed::Os::Mac, zed::Architecture::Aarch64) => "aarch64-apple-darwin",
            (zed::Os::Mac, _) => "x86_64-apple-darwin",
            (zed::Os::Linux, zed::Architecture::Aarch64) => "aarch64-unknown-linux-musl",
            (zed::Os::Linux, _) => "x86_64-unknown-linux-musl",
            (zed::Os::Windows, _) => "x86_64-pc-windows-msvc",
        };
        // Must match the archive names produced by .github/workflows/release.yml.
        let asset_name = format!(
            "{SERVER_BINARY}-{target}.{ext}",
            ext = match platform {
                zed::Os::Windows => "zip",
                _ => "tar.gz",
            }
        );
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("release {} has no asset {asset_name}", release.version))?;

        let version_dir = format!("{SERVER_BINARY}-{}", release.version);
        let binary_path = format!(
            "{version_dir}/{SERVER_BINARY}{exe}",
            exe = match platform {
                zed::Os::Windows => ".exe",
                _ => "",
            }
        );

        if !std::fs::metadata(&binary_path).is_ok_and(|stat| stat.is_file()) {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );
            zed::download_file(
                &asset.download_url,
                &version_dir,
                match platform {
                    zed::Os::Windows => zed::DownloadedFileType::Zip,
                    _ => zed::DownloadedFileType::GzipTar,
                },
            )
            .map_err(|err| format!("failed to download {asset_name}: {err}"))?;
            zed::make_file_executable(&binary_path)?;

            // Drop stale version directories.
            if let Ok(entries) = std::fs::read_dir(".") {
                for entry in entries.flatten() {
                    if entry.file_name().to_str() != Some(&version_dir) {
                        std::fs::remove_dir_all(entry.path()).ok();
                    }
                }
            }
        }

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }
}

impl zed::Extension for PubspecExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binary_settings = LspSettings::for_worktree("pubspec-lsp", worktree)
            .ok()
            .and_then(|settings| settings.binary);
        let args = binary_settings
            .as_ref()
            .and_then(|binary| binary.arguments.clone())
            .unwrap_or_default();

        // Explicit user override wins over everything.
        if let Some(path) = binary_settings.and_then(|binary| binary.path) {
            return Ok(zed::Command {
                command: path,
                args,
                env: Default::default(),
            });
        }

        Ok(zed::Command {
            command: self.language_server_binary_path(language_server_id, worktree)?,
            args,
            env: Default::default(),
        })
    }
}

zed::register_extension!(PubspecExtension);
