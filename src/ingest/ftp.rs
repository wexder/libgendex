//! FTP directory discovery for native RAR snapshots.

use crate::config::SourceConfig;
use anyhow::{Context, Result, bail};
use regex::Regex;
use reqwest::Url;
use std::{collections::BTreeMap, time::Duration};
use suppaftp::{tokio::AsyncFtpStream, types::FileType};
use tracing::{info, warn};

const LISTING_TIMEOUT: Duration = Duration::from_secs(45);
type Snapshots = BTreeMap<String, BTreeMap<u32, String>>;

fn listing_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).context("invalid FTP listing URL")?;
    if url.scheme() != "ftp" || raw.contains('*') {
        bail!("snapshot listings require a plain ftp:// directory URL: {raw}");
    }
    if !url.path().ends_with('/') {
        bail!("FTP listing URL must end with '/': {raw}");
    }
    Ok(url)
}

async fn list(url: &Url) -> Result<String> {
    tokio::time::timeout(LISTING_TIMEOUT, async {
        let host = url.host_str().context("FTP listing URL missing host")?;
        let mut ftp = AsyncFtpStream::connect((host, url.port().unwrap_or(21))).await?;
        let (user, pass) = if url.username().is_empty() {
            ("anonymous", "anonymous@")
        } else {
            (url.username(), url.password().unwrap_or_default())
        };
        ftp.login(user, pass).await?;
        ftp.transfer_type(FileType::Binary).await?;
        let names = ftp.nlst(Some(url.path())).await?;
        let _ = tokio::time::timeout(Duration::from_secs(2), ftp.quit()).await;
        Ok(names
            .iter()
            .map(|n| n.rsplit('/').next().unwrap_or(n))
            .collect::<Vec<_>>()
            .join("\n"))
    })
    .await
    .context("FTP directory listing timed out")?
}

fn collect_snapshot(found: &mut Snapshots, base: &Url, pattern: &Regex, body: &str) -> Result<()> {
    for cap in pattern.captures_iter(body) {
        let (Some(file), Some(date)) = (cap.get(0), cap.get(1)) else {
            continue;
        };
        if !file.as_str().to_ascii_lowercase().ends_with(".rar") {
            bail!("FTP snapshots must be RAR archives: {}", file.as_str());
        }
        let part: u32 = cap.get(2).map_or(Ok(1), |p| p.as_str().parse())?;
        if part == 0 {
            bail!("RAR volume numbers must start at 1");
        }
        found
            .entry(date.as_str().to_owned())
            .or_default()
            .entry(part)
            .or_insert(base.join(file.as_str())?.to_string());
    }
    Ok(())
}

fn newest_snapshot(mut found: Snapshots) -> Result<(String, Vec<String>)> {
    let (date, parts) = found
        .pop_last()
        .context("no RAR snapshot found in FTP listings")?;
    for (index, part) in parts.keys().enumerate() {
        if *part as usize != index + 1 {
            bail!("snapshot {date} is missing RAR volume {}", index + 1);
        }
    }
    Ok((date, parts.into_values().collect()))
}

pub(super) async fn discover(source: &SourceConfig) -> Result<(String, Vec<String>)> {
    let pattern = Regex::new(&source.pattern).context("invalid snapshot filename pattern")?;
    if pattern.captures_len() < 2 {
        bail!("snapshot filename pattern must capture the date");
    }
    // Validate every configured endpoint before any network access.
    let listings = source
        .listing_urls
        .iter()
        .map(|s| listing_url(s))
        .collect::<Result<Vec<_>>>()?;
    if listings.is_empty() {
        bail!("source {} has no FTP listing URLs", source.name);
    }
    let mut found = Snapshots::new();
    for listing in listings {
        info!(source = %source.name, %listing, "listing FTP snapshots");
        match list(&listing).await {
            Ok(body) => collect_snapshot(&mut found, &listing, &pattern, &body)?,
            Err(error) => {
                warn!(source = %source.name, %listing, error = format!("{error:#}"), "FTP listing unavailable")
            }
        }
    }
    let (date, urls) = newest_snapshot(found)?;
    info!(source = %source.name, %date, volumes = urls.len(), "discovered FTP snapshot");
    Ok((date, urls))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn snapshots_are_sorted_by_date_and_volume_with_duplicate_mirrors_ignored() {
        let pattern = Regex::new(&Config::default().indexer.sources[0].pattern).unwrap();
        let mut found = Snapshots::new();
        let base = listing_url("ftp://mirror.test/upload/").unwrap();
        let body = "libgen_new-2026-09-06.part002.rar\nlibgen_new-2026-08-01.part001.rar\nlibgen_new-2026-09-06.part001.rar";
        collect_snapshot(&mut found, &base, &pattern, body).unwrap();
        collect_snapshot(
            &mut found,
            &listing_url("ftp://other.test/upload/").unwrap(),
            &pattern,
            body,
        )
        .unwrap();
        let (date, urls) = newest_snapshot(found).unwrap();
        assert_eq!(date, "2026-09-06");
        assert_eq!(
            urls,
            vec![
                "ftp://mirror.test/upload/libgen_new-2026-09-06.part001.rar",
                "ftp://mirror.test/upload/libgen_new-2026-09-06.part002.rar"
            ]
        );
    }

    #[test]
    fn rejects_incomplete_volumes_and_non_rar_snapshots() {
        let base = listing_url("ftp://mirror.test/upload/").unwrap();
        let pattern = Regex::new(&Config::default().indexer.sources[0].pattern).unwrap();
        let mut found = Snapshots::new();
        collect_snapshot(
            &mut found,
            &base,
            &pattern,
            "libgen_new-2026-09-06.part002.rar",
        )
        .unwrap();
        assert!(
            newest_snapshot(found)
                .unwrap_err()
                .to_string()
                .contains("missing RAR volume 1")
        );
        assert!(
            collect_snapshot(
                &mut Snapshots::new(),
                &base,
                &pattern,
                "libgen_new-2026-09-06.part000.rar"
            )
            .is_err()
        );
        let gzip_pattern = Regex::new(r"dump-(\d{4}-\d{2}-\d{2})\.sql\.gz").unwrap();
        assert!(
            collect_snapshot(
                &mut Snapshots::new(),
                &base,
                &gzip_pattern,
                "dump-2026-09-06.sql.gz"
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_http_wildcards_and_non_directory_listings() {
        assert!(listing_url("https://mirror.test/upload/").is_err());
        assert!(listing_url("ftp://mirror.test/upload/*/").is_err());
        assert!(listing_url("ftp://mirror.test/upload").is_err());
    }
}
