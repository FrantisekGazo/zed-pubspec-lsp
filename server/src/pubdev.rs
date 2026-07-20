use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use serde::Deserialize;

pub const PUB_DEV_URL: &str = "https://pub.dev";

const USER_AGENT: &str = concat!(
    "pubspec-lsp/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/FrantisekGazo/pubspec-lsp)"
);

#[derive(Debug, Clone)]
pub struct VersionEntry {
    pub version: String,
    pub retracted: bool,
}

#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub description: Option<String>,
    pub latest: String,
    /// All published versions, in API order (oldest first).
    pub versions: Vec<VersionEntry>,
    pub is_discontinued: bool,
    pub replaced_by: Option<String>,
}

impl PackageInfo {
    pub fn pub_dev_url(&self) -> String {
        format!("{PUB_DEV_URL}/packages/{}", self.name)
    }
}

/// Cached lookup result. 404s are cached too, so repeated diagnostics runs
/// don't hammer the API for a typo'd package name.
#[derive(Clone)]
enum Lookup {
    Found(Arc<PackageInfo>),
    NotFound,
}

struct NameCache {
    names: Arc<Vec<String>>,
    fetched_at: std::time::Instant,
}

pub struct PubDevClient {
    http: reqwest::Client,
    base_url: String,
    packages: Cache<String, Lookup>,
    names: tokio::sync::Mutex<Option<NameCache>>,
}

impl PubDevClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            base_url: base_url.into(),
            packages: Cache::builder()
                .max_capacity(2048)
                .time_to_live(Duration::from_secs(15 * 60))
                .build(),
            names: tokio::sync::Mutex::new(None),
        }
    }

    /// All pub.dev package names, popularity-ranked (most popular first).
    /// pub.dev asks clients to cache this endpoint for 8 hours; the list is
    /// also persisted to disk so completions work instantly on startup and
    /// offline. Serves stale data when a refresh fails.
    pub async fn package_names(&self) -> Option<Arc<Vec<String>>> {
        let mut guard = self.names.lock().await;

        if let Some(cache) = guard.as_ref() {
            if cache.fetched_at.elapsed() < NAME_LIST_TTL {
                return Some(Arc::clone(&cache.names));
            }
        }

        if guard.is_none() {
            if let Some((names, age)) = load_names_from_disk() {
                let fresh = age < NAME_LIST_TTL;
                let cache = NameCache {
                    names: Arc::new(names),
                    // Backdate so a stale disk cache still triggers a refresh.
                    fetched_at: std::time::Instant::now() - age.min(NAME_LIST_TTL * 2),
                };
                let names = Arc::clone(&cache.names);
                *guard = Some(cache);
                if fresh {
                    return Some(names);
                }
            }
        }

        match self.fetch_names().await {
            Ok(names) => {
                let names = Arc::new(names);
                *guard = Some(NameCache {
                    names: Arc::clone(&names),
                    fetched_at: std::time::Instant::now(),
                });
                Some(names)
            }
            Err(err) => {
                tracing::debug!("package name list fetch failed: {err}");
                // Offline: serve whatever we have, however stale.
                guard.as_ref().map(|cache| Arc::clone(&cache.names))
            }
        }
    }

    async fn fetch_names(&self) -> Result<Vec<String>, String> {
        let url = format!("{}/api/package-name-completion-data", self.base_url);
        let body = self
            .http
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())?;
        let data: NameCompletionData = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        store_names_to_disk(&body);
        Ok(data.packages)
    }

    /// Package metadata from pub.dev. None on 404, network failure, or decode
    /// error — callers degrade silently (no hover / no diagnostic).
    pub async fn package_info(&self, name: &str) -> Option<Arc<PackageInfo>> {
        let lookup = self
            .packages
            .try_get_with(name.to_string(), self.fetch_package(name))
            .await
            .map_err(|err| tracing::debug!("pub.dev fetch failed for {name}: {err}"))
            .ok()?;
        match lookup {
            Lookup::Found(info) => Some(info),
            Lookup::NotFound => None,
        }
    }

    async fn fetch_package(&self, name: &str) -> Result<Lookup, String> {
        let url = format!("{}/api/packages/{}", self.base_url, name);
        let response = self.http.get(url).send().await.map_err(|e| e.to_string())?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Lookup::NotFound);
        }
        let api: ApiPackage = response
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        Ok(Lookup::Found(Arc::new(api.into())))
    }
}

const NAME_LIST_TTL: Duration = Duration::from_secs(8 * 60 * 60);

#[derive(Deserialize)]
struct NameCompletionData {
    packages: Vec<String>,
}

fn names_cache_path() -> Option<std::path::PathBuf> {
    Some(dirs::cache_dir()?.join("pubspec-lsp/name-completion-data.json"))
}

fn load_names_from_disk() -> Option<(Vec<String>, Duration)> {
    let path = names_cache_path()?;
    let age = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|mtime| mtime.elapsed().ok())?;
    let data: NameCompletionData =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    Some((data.packages, age))
}

fn store_names_to_disk(raw_json: &str) {
    let Some(path) = names_cache_path() else {
        return;
    };
    let _ = path.parent().map(std::fs::create_dir_all);
    if let Err(err) = std::fs::write(&path, raw_json) {
        tracing::debug!("failed to persist name list to {path:?}: {err}");
    }
}

#[derive(Deserialize)]
struct ApiPackage {
    name: String,
    #[serde(default, rename = "isDiscontinued")]
    is_discontinued: bool,
    #[serde(default, rename = "replacedBy")]
    replaced_by: Option<String>,
    latest: ApiVersion,
    #[serde(default)]
    versions: Vec<ApiVersion>,
}

#[derive(Deserialize)]
struct ApiVersion {
    version: String,
    #[serde(default)]
    retracted: bool,
    #[serde(default)]
    pubspec: serde_json::Value,
}

impl From<ApiPackage> for PackageInfo {
    fn from(api: ApiPackage) -> Self {
        Self {
            description: api.pubspec_description(),
            name: api.name,
            latest: api.latest.version,
            versions: api
                .versions
                .into_iter()
                .map(|v| VersionEntry {
                    version: v.version,
                    retracted: v.retracted,
                })
                .collect(),
            is_discontinued: api.is_discontinued,
            replaced_by: api.replaced_by,
        }
    }
}

impl ApiPackage {
    fn pubspec_description(&self) -> Option<String> {
        self.latest
            .pubspec
            .get("description")
            .and_then(|d| d.as_str())
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
    }
}
