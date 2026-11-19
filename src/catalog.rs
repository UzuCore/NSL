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
}

fn catalog_path(base_dir: &Path) -> PathBuf {
    base_dir.join("titledb").join("catalog.json")
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(timeout).build()
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
    let response = agent(Duration::from_secs(120))
        .get(source_url)
        .set("User-Agent", "NSL")
        .call()
        .context("카탈로그 다운로드 실패")?;
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
    };
    crate::database::atomic_write(&path, &serde_json::to_vec(&payload)?)?;
    Ok(())
}

#[derive(serde::Serialize)]
struct CatalogBorrowed<'a> {
    source_url: &'a str,
    items: &'a [CatalogItem],
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CatalogFile {
    #[serde(default)]
    source_url: String,
    #[serde(default)]
    items: Vec<CatalogItem>,
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
        };
        save(dir.path(), &state).unwrap();
        let loaded = load(dir.path()).expect("catalog reloads");
        assert_eq!(loaded.items.len(), 1);
        assert_eq!(loaded.items[0].title, "Game");
        assert_eq!(loaded.source_url, DEFAULT_CATALOG_URL);
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
