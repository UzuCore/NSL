use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::{collections::HashMap, fs, path::Path};

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub title_id: String,
    pub name: String,
    pub version: i64,
    pub release_date: String,
    pub num_players: String,
    pub rating: String,
    pub size: i64,
    pub region: String,
    pub icon_url: String,
    pub screenshots: String,
    pub description: String,
    pub languages: String,
}
fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.split_whitespace().collect::<Vec<_>>().join(" "),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}
fn first(o: &Map<String, Value>, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| text(o.get(*k)))
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}
fn int(o: &Map<String, Value>, keys: &[&str]) -> i64 {
    keys.iter()
        .find_map(|k| {
            o.get(*k).and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
        })
        .unwrap_or(0)
}
fn screenshots(o: &Map<String, Value>) -> String {
    let urls: Vec<String> = o
        .get("screenshots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|s| s.starts_with("http"))
        .map(str::to_owned)
        .collect();
    serde_json::to_string(&urls).unwrap_or_else(|_| "[]".into())
}
/// Every game in a release, i.e. the base title behind a patch/DLC id.
///
/// Nintendo encodes the content kind in the low 13 bits of a title id:
/// `0x000` is the application, `0x800` the patch, and everything else is DLC.
/// The catalog ships patch entries as their own rows with no name and no
/// artwork - only `version` - so they have to be folded back into the base row
/// or the newest-update data is lost.
fn base_title_id(title_id: &str) -> Option<String> {
    let value = u64::from_str_radix(title_id, 16).ok()?;
    Some(format!("{:016X}", value & !0x1FFF))
}

fn hex_kind(title_id: &str) -> ContentKind {
    let Some(value) = u64::from_str_radix(title_id, 16).ok() else {
        // Non-hex ids (homebrew placeholders) behave like applications.
        return ContentKind::Application;
    };
    match value & 0x1FFF {
        0 => ContentKind::Application,
        0x800 => ContentKind::Patch,
        _ => ContentKind::AddOn,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentKind {
    Application,
    Patch,
    AddOn,
}

/// Latest version a Nintendo version integer can reasonably represent.
/// Real patch ordinals top out around 323, so anything beyond this is junk
/// data rather than a version.
const MAX_PLAUSIBLE_VERSION: i64 = 0x4000_0000;

fn plausible_version(value: i64) -> Option<i64> {
    (1..=MAX_PLAUSIBLE_VERSION).contains(&value).then_some(value)
}

fn parse_titles(v: Value) -> HashMap<String, Entry> {
    let data = v.get("titledb").cloned().unwrap_or(v);
    let mut out = HashMap::new();
    // Patch rows carry only a version; remember them so they can upgrade the
    // matching application row after every entry has been read.
    let mut patch_versions: HashMap<String, i64> = HashMap::new();
    match data {
        Value::Object(map) => {
            for (tid, info) in map {
                let Some(o) = info.as_object() else { continue };
                let name = first(o, &["name", "title", "Name"]);
                let id = first(o, &["id", "titleId", "title_id"]);
                let id = if id.is_empty() {
                    tid
                } else {
                    id
                }
                .to_ascii_uppercase();
                if id.is_empty() {
                    continue;
                }
                let version = int(o, &["version", "Version"]);
                if hex_kind(&id) == ContentKind::Patch {
                    if let (Some(base), Some(version)) =
                        (base_title_id(&id), plausible_version(version))
                    {
                        patch_versions
                            .entry(base)
                            .and_modify(|current| *current = (*current).max(version))
                            .or_insert(version);
                    }
                }
                if name.is_empty() {
                    // Nameless rows are patch/DLC stubs: they contribute their
                    // version above but must not become library entries.
                    continue;
                }
                let e = Entry {
                    title_id: id,
                    name,
                    version,
                    release_date: first(o, &["releaseDate", "release_date"]),
                    num_players: first(o, &["numberOfPlayers", "numPlayers"]),
                    rating: first(o, &["rating", "contentRating"]),
                    size: int(o, &["size"]),
                    region: first(o, &["region"]),
                    icon_url: first(o, &["iconUrl", "icon_url"]),
                    screenshots: screenshots(o),
                    description: text(o.get("description")),
                    languages: first(o, &["languages"]),
                };
                out.insert(e.title_id.clone(), e);
            }
        }
        Value::Array(items) => {
            for info in items {
                let Some(o) = info.as_object() else { continue };
                let id = first(o, &["id", "titleId", "title_id"]).to_ascii_uppercase();
                if id.is_empty() {
                    continue;
                }
                let version = int(o, &["version"]);
                if hex_kind(&id) == ContentKind::Patch {
                    if let (Some(base), Some(version)) =
                        (base_title_id(&id), plausible_version(version))
                    {
                        patch_versions
                            .entry(base)
                            .and_modify(|current| *current = (*current).max(version))
                            .or_insert(version);
                    }
                }
                let name = first(o, &["name", "title"]);
                if name.is_empty() {
                    continue;
                }
                let e = Entry {
                    title_id: id.clone(),
                    name,
                    version,
                    release_date: first(o, &["releaseDate"]),
                    num_players: first(o, &["numberOfPlayers"]),
                    rating: first(o, &["rating"]),
                    size: int(o, &["size"]),
                    region: first(o, &["region"]),
                    icon_url: String::new(),
                    screenshots: "[]".into(),
                    description: text(o.get("description")),
                    languages: first(o, &["languages"]),
                };
                out.insert(id, e);
            }
        }
        _ => {}
    }
    for (base, version) in patch_versions {
        out.entry(base.clone())
            .and_modify(|entry| {
                if version > entry.version {
                    entry.version = version;
                }
            })
            .or_insert_with(|| Entry {
                title_id: base,
                version,
                ..Entry::default()
            });
    }
    out
}
pub fn load_local(dir: &Path) -> Result<Vec<Entry>> {
    let read = |name: &str| -> Result<Value> {
        let p = dir.join(name);
        serde_json::from_slice(
            &fs::read(&p).with_context(|| format!("{} 파일을 읽지 못했습니다", p.display()))?,
        )
        .with_context(|| format!("{} JSON 형식이 잘못되었습니다", p.display()))
    };
    let mut titles = parse_titles(read("titles.json")?);
    if let Ok(versions) = read("versions.json") {
        if let Some(map) = versions.as_object() {
            for (tid, values) in map {
                if let Some(obj) = values.as_object() {
                    let latest = obj
                        .keys()
                        .filter_map(|s| s.parse::<i64>().ok())
                        .max()
                        .unwrap_or(0);
                    if let Some(e) = titles.get_mut(&tid.to_ascii_uppercase()) {
                        e.version = latest
                    } else {
                        titles.insert(
                            tid.to_ascii_uppercase(),
                            Entry {
                                title_id: tid.to_ascii_uppercase(),
                                version: latest,
                                ..Entry::default()
                            },
                        );
                    }
                }
            }
        }
    }
    if let Ok(kr) = read("KR.ko.json") {
        if let Some(map) = kr.as_object() {
            for (_, value) in map {
                let Some(o) = value.as_object() else { continue };
                let id = first(o, &["id", "titleId"]).to_ascii_uppercase();
                if id.is_empty() {
                    continue;
                }
                let name = first(o, &["name", "title"]);
                let desc = text(o.get("description"));
                let e = titles.entry(id.clone()).or_insert_with(|| Entry {
                    title_id: id,
                    ..Entry::default()
                });
                if !name.is_empty() {
                    e.name = name
                }
                if !desc.is_empty() {
                    e.description = desc
                }
            }
        }
    }
    Ok(titles.into_values().collect())
}

pub fn download_full(dir: &Path, source: &str) -> Result<Vec<Entry>> {
    fs::create_dir_all(dir)?;
    let titles_url = if source == "custom" {
        "https://github.com/UzuCore/NSL/releases/download/titledb/titles.json"
    } else {
        "https://tinfoil.media/repo/db/titles.json"
    };
    let endpoints = [
        ("titles.json", titles_url),
        (
            "versions.json",
            "https://raw.githubusercontent.com/blawar/titledb/master/versions.json",
        ),
        (
            "KR.ko.json",
            "https://raw.githubusercontent.com/blawar/titledb/master/KR.ko.json",
        ),
    ];
    for (name, url) in endpoints {
        let response=ureq::get(url).set("User-Agent","Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/124.0.0.0 Safari/537.36").set("Accept","application/json, text/plain, */*").call();
        match response {
            Ok(resp) => {
                let body = resp.into_string()?;
                let _: Value = serde_json::from_str(&body)
                    .with_context(|| format!("{name} 응답이 올바른 JSON이 아닙니다"))?;
                let tmp = dir.join(format!("{name}.tmp"));
                fs::write(&tmp, body)?;
                let target = dir.join(name);
                if target.exists() {
                    fs::remove_file(&target)?
                }
                fs::rename(tmp, target)?;
            }
            Err(e) if name == "KR.ko.json" => {
                eprintln!("한국어 타이틀 목록 다운로드 생략: {e}");
            }
            Err(e) => anyhow::bail!("{name} 다운로드 실패: {e}"),
        }
    }
    load_local(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nameless_patch_rows_upgrade_the_base_entry() {
        // The real catalog stores patches as their own nameless rows: only the
        // version is useful, and dropping them loses every "update available"
        // signal. Here the base row has no version and the patch has 393216.
        let mut titles = parse_titles(serde_json::json!({
            "titledb": {
                "0100D2C0115B6000": { "id": "0100D2C0115B6000", "name": "Game" },
                "0100D2C0115B6800": { "id": "0100D2C0115B6800", "version": 393216 }
            }
        }));
        let base = titles.remove("0100D2C0115B6000").expect("base entry");
        assert_eq!(base.name, "Game");
        assert_eq!(base.version, 393216);
        assert!(
            !titles.contains_key("0100D2C0115B6800"),
            "patch rows must not become library entries"
        );
    }

    #[test]
    fn newest_patch_wins_and_older_data_never_downgrades() {
        // A base and its single patch id, both carrying a version: the newer one
        // wins regardless of which row was read first.
        let upgraded = parse_titles(serde_json::json!({
            "titledb": {
                "0100D2C0115B6000": { "id": "0100D2C0115B6000", "name": "Game", "version": 131072 },
                "0100D2C0115B6800": { "id": "0100D2C0115B6800", "version": 262144 }
            }
        }));
        assert_eq!(upgraded["0100D2C0115B6000"].version, 262144);

        // The reverse case must not downgrade a base row that is already newer.
        let kept = parse_titles(serde_json::json!({
            "titledb": {
                "0100D2C0115B6000": { "id": "0100D2C0115B6000", "name": "Game", "version": 262144 },
                "0100D2C0115B6800": { "id": "0100D2C0115B6800", "version": 65536 }
            }
        }));
        assert_eq!(kept["0100D2C0115B6000"].version, 262144);
    }

    #[test]
    fn patch_only_bases_still_get_an_entry() {
        // 201 real patch bases have no application row at all; their version is
        // still worth keeping in case a file shows up later.
        let titles = parse_titles(serde_json::json!({
            "titledb": {
                "0100D2C0115B6800": { "id": "0100D2C0115B6800", "version": 65536 }
            }
        }));
        let base = titles.get("0100D2C0115B6000").expect("derived base entry");
        assert_eq!(base.version, 65536);
        assert!(base.name.is_empty());
    }

    #[test]
    fn dlc_rows_and_junk_versions_do_not_leak_into_the_base() {
        let titles = parse_titles(serde_json::json!({
            "titledb": {
                "0100D2C0115B6000": { "id": "0100D2C0115B6000", "name": "Game", "version": 65536 },
                "0100D2C0115B7001": { "id": "0100D2C0115B7001", "version": 999999 },
                "0100D2C0115B6800": { "id": "0100D2C0115B6800", "version": 0 },
                "0100D2C0115B6801": { "id": "0100D2C0115B6801", "version": 9000000000000000i64 }
            }
        }));
        let base = titles.get("0100D2C0115B6000").expect("base entry");
        assert_eq!(base.version, 65536, "DLC and implausible versions ignored");
    }

    #[test]
    fn merges_titles_versions_and_korean_names_from_local_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("titles.json"),
            r#"{"titledb":{"0100D2C0115B6000":{"name":"Database Name","description":["First","second"],"screenshots":["https://img.example/a.jpg","invalid"]}}}"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("versions.json"),
            r#"{"0100D2C0115B6000":{"0":{},"65536":{}},"0100D2C0115B6800":{"131072":{}}}"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("KR.ko.json"),
            r#"{"sample":{"id":"0100D2C0115B6000","name":"한국어 이름","description":"번역된 설명"}}"#,
        )
        .unwrap();

        let entries = load_local(dir.path()).unwrap();
        let base = entries
            .iter()
            .find(|entry| entry.title_id == "0100D2C0115B6000")
            .unwrap();
        let update = entries
            .iter()
            .find(|entry| entry.title_id == "0100D2C0115B6800")
            .unwrap();
        assert_eq!(base.name, "한국어 이름");
        assert_eq!(base.version, 65536);
        assert_eq!(base.description, "번역된 설명");
        assert_eq!(base.screenshots, r#"["https://img.example/a.jpg"]"#);
        assert_eq!(update.version, 131072);
        assert!(update.name.is_empty());
    }
}
