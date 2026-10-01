use std::{path::PathBuf, time::Duration};

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub paths: PathsConfig,
    pub indexer: IndexerConfig,
    pub download: DownloadConfig,
    pub ranking: RankingConfig,
    pub log: LogConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
    pub static_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathsConfig {
    pub data_dir: PathBuf,
    pub library_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexerConfig {
    pub enabled: bool,
    pub run_on_start: bool,
    #[serde(with = "humantime_serde")]
    pub refresh_interval: Duration,
    /// Maximum persistent FTP source-range cache size in MiB; zero disables the cache.
    pub ftp_cache_mb: u64,
    pub writer_memory_mb: usize,
    pub extensions: Vec<String>,
    /// Overrides for `elem_descr` keys; resolved from the dump when unset.
    pub language_key: Option<i64>,
    pub isbn_key: Option<i64>,
    pub sources: Vec<SourceConfig>,
    pub api: ApiSyncConfig,
}

/// Incremental updates through the libgen.li `json.php` API after a source was bootstrapped from a
/// snapshot; when enabled, FTP bootstrap runs only for sources that have never been indexed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiSyncConfig {
    pub enabled: bool,
    /// `json.php` endpoints, tried in order on failure.
    pub urls: Vec<String>,
    /// Records per listing request (the API allows at most 10000).
    pub page_size: usize,
    /// Ids per lookup request.
    pub batch_size: usize,
    /// Lookup requests in flight at once.
    pub concurrency: usize,
    /// How many past days of file changes (removals, edits) to replay after a dump import. New files
    /// are always fetched completely by id, however old the dump.
    pub modified_catchup_days: u64,
    /// Pause before every request, to stay polite.
    #[serde(with = "humantime_serde")]
    pub request_delay: Duration,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    pub name: String,
    /// FTP directory listings used to discover the newest snapshot.
    #[serde(default)]
    pub listing_urls: Vec<String>,
    /// Regex capturing the dump date and, for multi-volume archives, the volume number.
    #[serde(default)]
    pub pattern: String,
    /// Use a local dump (.sql, .sql.gz or first .rar volume) instead of downloading.
    #[serde(default)]
    pub local_path: Option<PathBuf>,
    /// LibGen topics this source covers (`f` fiction, `l` non-fiction). A combined libgen.li dump is
    /// filtered to them; API updates follow them. Empty = everything in the dump, no API updates.
    #[serde(default)]
    pub topics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadConfig {
    pub resolvers: Vec<Resolver>,
    pub user_agent: String,
    #[serde(with = "humantime_serde")]
    pub connect_timeout: Duration,
    pub max_concurrent: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolver {
    /// URL with `{md5}` placeholder. Without `link_pattern` it must serve the file directly.
    pub url: String,
    /// Regex whose first capture group is the file link inside the HTML page served by `url`.
    #[serde(default)]
    pub link_pattern: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingConfig {
    /// Optional AI re-ranking: `none` (BM25 + format rules only), `api` (remote OpenJev server) or
    /// `local` (in-process model; needs a build with the `local` feature).
    pub provider: String,
    /// How many top BM25 hits are re-ranked.
    pub rerank_top: usize,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    pub cache_size: usize,
    pub api: ApiRankingConfig,
    pub local: LocalRankingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiRankingConfig {
    /// OpenJev / Jev compatible server exposing `/v1/systemone`.
    pub url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub concurrency: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalRankingConfig {
    /// Local GGUF model; when unset `model_url` is downloaded into `<data_dir>/models`.
    pub model_path: Option<PathBuf>,
    pub model_url: String,
    /// CPU threads for inference; 0 picks the number of physical cores (at most 6).
    pub threads: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    pub level: String,
    pub json: bool,
}

impl Default for Config {
    fn default() -> Self {
        let ext = |s: &[&str]| s.iter().map(|s| s.to_string()).collect();
        Self {
            server: ServerConfig {
                bind: "0.0.0.0:8080".into(),
                static_dir: "web/dist".into(),
            },
            paths: PathsConfig {
                data_dir: "data".into(),
                library_dir: "library".into(),
            },
            indexer: IndexerConfig {
                enabled: true,
                run_on_start: true,
                refresh_interval: Duration::from_secs(24 * 3600),
                ftp_cache_mb: 512,
                writer_memory_mb: 64,
                extensions: ext(&[
                    "epub", "mobi", "azw3", "azw", "fb2", "pdf", "djvu", "txt", "rtf", "doc",
                    "docx", "lit",
                ]),
                language_key: None,
                isbn_key: None,
                sources: vec![SourceConfig {
                    name: "libgen".into(),
                    listing_urls: ext(&["ftp://ftp.libgen.bz/upload/dbbackup/"]),
                    pattern: r"libgen_new-(\d{4}-\d{2}-\d{2})\.part(\d+)\.rar".into(),
                    local_path: None,
                    topics: ext(&["f", "l"]),
                }],
                api: ApiSyncConfig {
                    enabled: true,
                    urls: ext(&[
                        "https://libgen.li/json.php",
                        "https://libgen.bz/json.php",
                        "https://libgen.vg/json.php",
                    ]),
                    page_size: 2_000,
                    batch_size: 100,
                    concurrency: 4,
                    modified_catchup_days: 7,
                    request_delay: Duration::from_secs(1),
                    timeout: Duration::from_secs(45),
                },
            },
            download: DownloadConfig {
                resolvers: [
                    "https://libgen.li",
                    "https://libgen.bz",
                    "https://libgen.vg",
                ]
                .iter()
                .map(|m| Resolver {
                    url: format!("{m}/ads.php?md5={{md5}}"),
                    link_pattern: Some(r#"href="([^"]*get\.php\?md5=[^"]+)""#.into()),
                })
                .collect(),
                user_agent:
                    "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0".into(),
                connect_timeout: Duration::from_secs(20),
                max_concurrent: 2,
            },
            ranking: RankingConfig {
                provider: "none".into(),
                rerank_top: 30,
                timeout: Duration::from_secs(10),
                cache_size: 20_000,
                api: ApiRankingConfig {
                    url: "http://localhost:8081".into(),
                    api_key: None,
                    model: "openjev-latest".into(),
                    concurrency: 8,
                },
                local: LocalRankingConfig {
                    model_path: None,
                    model_url: "https://huggingface.co/unsloth/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q4_K_M.gguf".into(),
                    threads: 0,
                },
            },
            log: LogConfig {
                level: "info".into(),
                json: false,
            },
        }
    }
}

impl Config {
    /// Defaults < `libgendex.toml` (or `$LIBGENDEX_CONFIG`) < `LIBGENDEX_*` env vars (`__` separates nested keys).
    pub fn load() -> anyhow::Result<Self> {
        let path = std::env::var("LIBGENDEX_CONFIG").unwrap_or_else(|_| "libgendex.toml".into());
        let cfg = Figment::from(Serialized::defaults(Config::default()))
            .merge(Toml::file(path))
            .merge(Env::prefixed("LIBGENDEX_").split("__").ignore(&["config"]))
            .extract()?;
        Ok(cfg)
    }
}
