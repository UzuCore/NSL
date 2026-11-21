use anyhow::{Context, Result};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
/// Base game + update + DLC torrent catalog, in the TorrentShopNX catalog JSON
/// format. See https://github.com/Langegen/switch-game-collection for the
/// source data; we read the shape, not its code.
pub const DEFAULT_CATALOG_URL: &str =
    "https://raw.githubusercontent.com/Langegen/switch-game-collection/main/EN_catalog.json";

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CatalogItem {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub magnet: String,
    #[serde(default)]
    pub topic_id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub year: String,
    #[serde(default)]
    pub genre: String,
    #[serde(default)]
    pub developer: String,
    #[serde(default)]
    pub publisher: String,
    #[serde(default)]
    pub image_format: String,
    #[serde(default)]
    pub interface_lang: String,
    #[serde(default)]
    pub cover: String,
    #[serde(default)]
    pub title_id: Option<String>,
}

impl CatalogItem {
    /// The 16-hex-digit base title id this entry belongs to, if any. Update
    /// and DLC rows are folded onto their application id, mirroring the
    /// library's own grouping, so an update entry matches the base game.
    pub fn base_title_id(&self) -> Option<String> {
        let raw = self.title_id.as_deref()?.trim();
        if raw.is_empty() {
            return None;
        }
        let value = u64::from_str_radix(raw, 16).ok()?;
        Some(format!("{value:016X}", value = value & !0x1FFF))
    }

    /// The BTv1 info hash from the magnet link, upper-cased. Useful for
    /// deduplicating downloads; kept public for the unit tests.
    #[allow(dead_code)]
    pub fn info_hash(&self) -> Option<String> {
        let marker = "urn:btih:";
        let start = self.magnet.find(marker)? + marker.len();
        let rest = &self.magnet[start..];
        let end = rest.find('&').unwrap_or(rest.len());
        let hash = &rest[..end];
        (!hash.is_empty()).then(|| hash.to_ascii_uppercase())
    }
}

/// A local snapshot of the catalog plus bookkeeping for incremental refreshes.
#[derive(Clone, Debug, Default)]
pub struct CatalogState {
    pub items: Vec<CatalogItem>,
    pub source_url: String,
    pub updated_at: Option<std::time::SystemTime>,
    /// ETag from the response that produced the persisted snapshot.
    pub etag: Option<String>,
    /// True only when a conditional HTTP request returned 304; never stored.
    pub not_modified: bool,
}

fn catalog_path(base_dir: &Path) -> PathBuf {
    base_dir.join("titledb").join("catalog.json")
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(timeout).build()
}

fn safe_http_etag(value: &str) -> bool {
    value.len() <= 200
        && (value.starts_with('"') || value.starts_with("W/\""))
        && value.ends_with('"')
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn cached_not_modified(
    cached: Option<CatalogState>,
    progress: Option<&crate::progress::TaskProgress>,
) -> Result<CatalogState> {
    let mut previous = cached.context("304 응답을 복구할 카탈로그 캐시가 없습니다")?;
    previous.not_modified = true;
    previous.updated_at = Some(std::time::SystemTime::now());
    if let Some(progress) = progress {
        progress.begin("게임 카탈로그 이미 최신", None, false);
    }
    Ok(previous)
}

fn parse_items(text: &str) -> Result<Vec<CatalogItem>> {
    let items: Vec<CatalogItem> =
        serde_json::from_str(text).context("카탈로그 JSON 형식이 잘못되었습니다")?;
    Ok(items
        .into_iter()
        .filter(|item| !item.magnet.is_empty())
        .collect())
}

/// Download the full catalog and persist it next to the title database files.
pub fn download_with_progress(
    base_dir: &Path,
    source_url: &str,
    progress: Option<&crate::progress::TaskProgress>,
) -> Result<CatalogState> {
    if let Some(progress) = progress {
        progress.begin("게임 카탈로그 연결 중", None, true);
    }
    // An unchanged 23 MB catalog need not be re-downloaded on every refresh.
    // The validator lives atomically in the same JSON file as the matching
    // catalog snapshot, so it can never describe different saved data.
    let cached = load(base_dir).filter(|state| state.source_url == source_url);
    let validator = cached
        .as_ref()
        .and_then(|state| state.etag.as_deref())
        .filter(|value| safe_http_etag(value))
        .map(str::to_owned);
    let mut request = agent(Duration::from_secs(120))
        .get(source_url)
        .set("User-Agent", "NSL");
    if let Some(etag) = &validator {
        request = request.set("If-None-Match", etag);
    }
    let response = match request.call() {
        // ureq 2.x treats a normal 304 as an Ok 3xx response.
        Ok(response) if response.status() == 304 => {
            anyhow::ensure!(validator.is_some(), "검증되지 않은 카탈로그 304 응답입니다");
            return cached_not_modified(cached, progress);
        }
        Ok(response) => response,
        Err(ureq::Error::Status(304, _)) if validator.is_some() => {
            return cached_not_modified(cached, progress);
        }
        Err(error) => return Err(error).context("카탈로그 다운로드 실패"),
    };
    let etag = response
        .header("ETag")
        .filter(|value| safe_http_etag(value))
        .map(str::to_owned);
    // Cap the body explicitly: slicing silently at the limit produced a
    // confusing "invalid JSON" error instead of a size error.
    const MAX_CATALOG_BYTES: u64 = 256 * 1024 * 1024;
    let mut bytes = Vec::new();
    if let Some(progress) = progress {
        let size = response
            .header("Content-Length")
            .and_then(|v| v.parse::<u64>().ok());
        progress.begin("게임 카탈로그 다운로드", size, true);
    }
    let mut reader = response.into_reader().take(MAX_CATALOG_BYTES + 1);
    if let Some(progress) = progress {
        crate::progress::MeteredReader::new(&mut reader, progress).read_to_end(&mut bytes)
    } else {
        reader.read_to_end(&mut bytes)
    }
    .context("카탈로그 응답을 읽지 못했습니다")?;
    if bytes.len() as u64 > MAX_CATALOG_BYTES {
        anyhow::bail!("카탈로그가 허용 크기(256 MB)를 초과했습니다");
    }
    if let Some(progress) = progress {
        progress.begin("게임 카탈로그 분석 및 저장", None, false);
    }
    let body = String::from_utf8(bytes).context("카탈로그 응답이 UTF-8이 아닙니다")?;
    let items = parse_items(&body)?;
    drop(body);
    let state = CatalogState {
        items,
        source_url: source_url.to_owned(),
        updated_at: Some(std::time::SystemTime::now()),
        etag,
        not_modified: false,
    };
    save(base_dir, &state)?;
    Ok(state)
}

pub fn save(base_dir: &Path, state: &CatalogState) -> Result<()> {
    let path = catalog_path(base_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let payload = CatalogBorrowed {
        source_url: &state.source_url,
        items: &state.items,
        etag: state.etag.as_deref(),
    };
    crate::database::atomic_write(&path, &serde_json::to_vec(&payload)?)?;
    Ok(())
}

#[derive(serde::Serialize)]
struct CatalogBorrowed<'a> {
    source_url: &'a str,
    items: &'a [CatalogItem],
    #[serde(skip_serializing_if = "Option::is_none")]
    etag: Option<&'a str>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CatalogFile {
    #[serde(default)]
    source_url: String,
    #[serde(default)]
    items: Vec<CatalogItem>,
    #[serde(default)]
    etag: Option<String>,
}

pub fn load(base_dir: &Path) -> Option<CatalogState> {
    let bytes = fs::read(catalog_path(base_dir)).ok()?;
    let payload: CatalogFile = serde_json::from_slice(&bytes).ok()?;
    Some(CatalogState {
        items: payload
            .items
            .into_iter()
            .filter(|item| !item.magnet.is_empty())
            .collect(),
        source_url: payload.source_url,
        updated_at: fs::metadata(catalog_path(base_dir))
            .and_then(|m| m.modified())
            .ok(),
        etag: payload.etag.filter(|value| safe_http_etag(value)),
        not_modified: false,
    })
}

/// Every catalog row that belongs to one base title id. Used to look up which
/// torrent carries the newest update for an owned game; kept public so the
/// integration tests can pin the folding behavior.
#[allow(dead_code)]
pub fn items_for_base<'a>(items: &'a [CatalogItem], base_title_id: &str) -> Vec<&'a CatalogItem> {
    items
        .iter()
        .filter(|item| {
            item.base_title_id()
                .is_some_and(|base| base.eq_ignore_ascii_case(base_title_id))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Explicit live-network QA: download the catalog with the production
    /// parser and persistence path, but only into a disposable QA workspace.
    /// Read metadata only; never initiate a copyrighted game transfer.
    #[test]
    #[ignore = "requires live external catalog HTTPS access; isolated metadata-only QA"]
    fn stage17_live_catalog_download_parse_reload_readonly() -> anyhow::Result<()> {
        let qa = Path::new("E:\\NSL_QA_20261120");
        anyhow::ensure!(qa.is_dir(), "Stage 17 isolated QA root missing");
        let workspace = tempfile::Builder::new()
            .prefix("stage17-catalog-live-")
            .tempdir_in(qa)?;
        let started = std::time::Instant::now();
        let catalog = download_with_progress(workspace.path(), DEFAULT_CATALOG_URL, None)?;
        anyhow::ensure!(
            catalog.items.len() > 1_000,
            "Unexpectedly small real catalog: {} entries",
            catalog.items.len()
        );
        let saved = catalog_path(workspace.path());
        let bytes = fs::metadata(&saved)?.len();
        let loaded = load(workspace.path()).context("Downloaded catalog did not reload")?;
        anyhow::ensure!(loaded.items.len() == catalog.items.len());
        anyhow::ensure!(loaded.source_url == DEFAULT_CATALOG_URL);
        let id_count = loaded
            .items
            .iter()
            .filter(|item| item.base_title_id().is_some())
            .count();
        let unique_ids = loaded
            .items
            .iter()
            .filter_map(|item| item.base_title_id())
            .collect::<std::collections::HashSet<_>>()
            .len();
        let valid_btih = loaded
            .items
            .iter()
            .filter(|item| {
                item.info_hash().is_some_and(|hash| {
                    hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            })
            .count();
        anyhow::ensure!(id_count > 1_000 && unique_ids > 1_000);
        let original_bytes = fs::read(&saved)?;
        let original_mtime = fs::metadata(&saved)?.modified()?;
        let validated = download_with_progress(workspace.path(), DEFAULT_CATALOG_URL, None)?;
        anyhow::ensure!(
            validated.not_modified,
            "Live catalog did not return a conditional HTTP 304 for its ETag"
        );
        anyhow::ensure!(validated.items.len() == loaded.items.len());
        anyhow::ensure!(fs::read(&saved)? == original_bytes);
        anyhow::ensure!(fs::metadata(&saved)?.modified()? == original_mtime);
        println!(
            "STAGE17_LIVE_CATALOG_PARSE_RELOAD_PASS entries={} saved_bytes={} with_base_title_id={} distinct_base_title_ids={} valid_btih40={} conditional_http_304=true duplicate_download_avoided=true elapsed_s={:.2} private_qa=true",
            loaded.items.len(), bytes, id_count, unique_ids, valid_btih,
            started.elapsed().as_secs_f64(),
        );
        Ok(())
    }

    #[test]
    fn base_title_id_folds_updates_and_dlcs() {
        let item = |id: &str| CatalogItem {
            title_id: Some(id.into()),
            magnet: "magnet:?xt=urn:btih:ABC".into(),
            ..Default::default()
        };
        assert_eq!(
            item("0100D2C0115B6000").base_title_id().as_deref(),
            Some("0100D2C0115B6000")
        );
        assert_eq!(
            item("0100D2C0115B6800").base_title_id().as_deref(),
            Some("0100D2C0115B6000"),
            "patch ids fold onto the application id"
        );
        assert_eq!(
            item("0100D2C0115B7001").base_title_id().as_deref(),
            Some("0100D2C0115B6000"),
            "dlc ids fold onto the application id"
        );
        assert_eq!(item("not-hex").base_title_id(), None);
        assert_eq!(item("").base_title_id(), None);
    }

    #[test]
    fn info_hash_extracts_the_btih_field() {
        let item = CatalogItem {
            magnet: "magnet:?xt=urn:btih:DF7468F241B0050047A76348C2B3302E193A10F8&tr=http%3A%2F%2Fbt.t-ru.org%2Fann%3Fmagnet".into(),
            ..Default::default()
        };
        assert_eq!(
            item.info_hash().as_deref(),
            Some("DF7468F241B0050047A76348C2B3302E193A10F8")
        );
    }

    #[test]
    fn items_for_base_matches_case_insensitively() {
        let items = vec![
            CatalogItem {
                title_id: Some("0100D2C0115B6800".into()),
                magnet: "magnet:?xt=urn:btih:A".into(),
                ..Default::default()
            },
            CatalogItem {
                title_id: Some("0100ABCDEF0123456".into()),
                magnet: "magnet:?xt=urn:btih:B".into(),
                ..Default::default()
            },
        ];
        let found = items_for_base(&items, "0100d2c0115b6000");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].info_hash().as_deref(), Some("A"));
    }

    #[test]
    fn catalog_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let state = CatalogState {
            items: vec![CatalogItem {
                title: "Game".into(),
                title_id: Some("0100D2C0115B6000".into()),
                magnet: "magnet:?xt=urn:btih:A".into(),
                size: "1.0 GB".into(),
                ..Default::default()
            }],
            source_url: DEFAULT_CATALOG_URL.into(),
            updated_at: None,
            etag: Some("\"sample-etag\"".into()),
            not_modified: false,
        };
        save(dir.path(), &state).unwrap();
        let loaded = load(dir.path()).expect("catalog reloads");
        assert_eq!(loaded.items.len(), 1);
        assert_eq!(loaded.items[0].title, "Game");
        assert_eq!(loaded.source_url, DEFAULT_CATALOG_URL);
        assert_eq!(loaded.etag.as_deref(), Some("\"sample-etag\""));
        assert!(!loaded.not_modified);
    }

    #[test]
    fn unchanged_http_catalog_uses_etag_without_redownloading_or_rewriting_cache() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/catalog.json", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(12);
            let mut requests = Vec::new();
            while requests.len() < 2 {
                if std::time::Instant::now() > deadline {
                    return requests;
                }
                let (mut stream, _) = match listener.accept() {
                    Ok(connected) => connected,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => panic!("Test HTTP listener failed: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = [0u8; 4096];
                let count = stream.read(&mut request).unwrap();
                let header = String::from_utf8_lossy(&request[..count]).to_string();
                eprintln!(
                    "STAGE17_ETAG_TEST_HTTP request_bytes={} request_line={:?}",
                    count,
                    header.lines().next().unwrap_or("")
                );
                requests.push(header.clone());
                if header.contains("If-None-Match: \"sample-etag\"")
                    || header.contains("if-none-match: \"sample-etag\"")
                {
                    stream
                        .write_all(
                            b"HTTP/1.1 304 Not Modified\r\nETag: \"sample-etag\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                } else {
                    let body = br#"[{"title":"Local game","magnet":"magnet:?xt=urn:btih:ABCD","title_id":"0100D2C0115B6000"}]"#;
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: \"sample-etag\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream.write_all(header.as_bytes()).unwrap();
                    stream.write_all(body).unwrap();
                }
            }
            requests
        });
        let private = tempfile::tempdir().unwrap();
        let first = download_with_progress(private.path(), &url, None).unwrap();
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.etag.as_deref(), Some("\"sample-etag\""));
        assert!(!first.not_modified);
        let path = catalog_path(private.path());
        let original = fs::read(&path).unwrap();
        let original_modified = fs::metadata(&path).unwrap().modified().unwrap();
        let second = download_with_progress(private.path(), &url, None).unwrap();
        assert_eq!(second.items.len(), 1);
        assert!(
            second.not_modified,
            "HTTP 304 must preserve the local catalog"
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(
            fs::metadata(&path).unwrap().modified().unwrap(),
            original_modified
        );
        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("if-none-match: \"sample-etag\""),
            "Second request must carry the saved ETag"
        );
        assert!(!requests[0].to_ascii_lowercase().contains("if-none-match:"));
    }

    #[test]
    fn unsafe_http_etags_are_not_persisted_or_sent() {
        assert!(safe_http_etag("\"abc\""));
        assert!(safe_http_etag("W/\"abc\""));
        assert!(!safe_http_etag("abc"));
        assert!(!safe_http_etag("\"bad\r\nInjection: 1\""));
        assert!(!safe_http_etag(&format!("\"{}\"", "z".repeat(256))));
    }

    #[test]
    fn parse_drops_rows_without_magnets() {
        let items =
            parse_items(r#"[{"title":"A","magnet":"magnet:?xt=urn:btih:A"},{"title":"B"}]"#)
                .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "A");
    }

    #[test]
    fn parse_real_catalog_shape() {
        // A trimmed excerpt of the real EN_catalog.json entry layout.
        let sample = r#"[
            {
                "title": "The Legend of Heroes: Trails in the Sky 2nd Chapter Remake [NSZ][DEMO][ENG]",
                "size": "3.63 GB",
                "magnet": "magnet:?xt=urn:btih:BDE9FC9AC4884B1759956051405EDC3C02DF05DF&tr=http%3A%2F%2Fbt.t-ru.org%2Fann%3Fmagnet",
                "topic_id": "6897473",
                "url": "https://rutracker.org/forum/viewtopic.php?t=6897473",
                "year": "2026, September",
                "genre": "Role-Playing, Action",
                "developer": "Nihon Falcom",
                "publisher": "GungHo America",
                "image_format": ".NSZ (compressed ~22%, installed size 4.65 GB) [DEMO]",
                "interface_lang": "English [ENG]",
                "cover": "https://raw.githubusercontent.com/Langegen/switch-game-collection/main/covers/6897473.jpg",
                "title_id": "0100AD6029730000"
            }
        ]"#;
        let items = parse_items(sample).unwrap();
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.base_title_id().as_deref(), Some("0100AD6029730000"));
        assert_eq!(
            item.info_hash().as_deref(),
            Some("BDE9FC9AC4884B1759956051405EDC3C02DF05DF")
        );
        assert_eq!(item.topic_id, "6897473");
    }

    #[test]
    fn parse_tolerates_missing_optional_fields() {
        // The catalog has rows with only a title, magnet, and no title id
        // (homebrew entries); they must not break the parser.
        let items =
            parse_items(r#"[{"title":"Homebrew Tool","magnet":"magnet:?xt=urn:btih:ABCD"}]"#)
                .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].base_title_id(), None);
        assert_eq!(items[0].size, "");
    }
}
