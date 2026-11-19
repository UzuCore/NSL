use crate::progress::{MeteredReader, TaskProgress};
use anyhow::{Context, Result};
use memmap2::Mmap;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Read,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

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
        Some(Value::String(s)) => {
            let mut cleaned = String::with_capacity(s.len());
            for word in s.split_whitespace() {
                if !cleaned.is_empty() {
                    cleaned.push(' ');
                }
                cleaned.push_str(word);
            }
            cleaned
        }
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Array(a)) => {
            let mut cleaned = String::new();
            let mut first = true;
            for value in a.iter().filter_map(Value::as_str) {
                if !first {
                    cleaned.push_str(", ");
                }
                cleaned.push_str(value);
                first = false;
            }
            cleaned
        }
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
    (1..=MAX_PLAUSIBLE_VERSION)
        .contains(&value)
        .then_some(value)
}

#[cfg(test)]
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
                let id = if id.is_empty() { tid } else { id }.to_ascii_uppercase();
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
/// Stream each JSON record instead of materializing a multi-hundred-megabyte
/// serde_json::Value tree beside the final 46k-entry database.
fn insert_streamed_title(
    fallback_id: &str,
    info: &Value,
    array_item: bool,
    entries: &mut HashMap<String, Entry>,
    patch_versions: &mut HashMap<String, i64>,
) {
    let Some(o) = info.as_object() else {
        return;
    };
    let raw_id = first(o, &["id", "titleId", "title_id"]);
    let id = if raw_id.is_empty() {
        fallback_id
    } else {
        &raw_id
    }
    .to_ascii_uppercase();
    if id.is_empty() {
        return;
    }
    let version = if array_item {
        int(o, &["version"])
    } else {
        int(o, &["version", "Version"])
    };
    if hex_kind(&id) == ContentKind::Patch {
        if let (Some(base), Some(version)) = (base_title_id(&id), plausible_version(version)) {
            patch_versions
                .entry(base)
                .and_modify(|current| *current = (*current).max(version))
                .or_insert(version);
        }
    }
    let name = if array_item {
        first(o, &["name", "title"])
    } else {
        first(o, &["name", "title", "Name"])
    };
    if name.is_empty() {
        return;
    }
    let entry = Entry {
        title_id: id.clone(),
        name,
        version,
        release_date: first(o, &["releaseDate", "release_date"]),
        num_players: first(o, &["numberOfPlayers", "numPlayers"]),
        rating: first(o, &["rating", "contentRating"]),
        size: int(o, &["size"]),
        region: first(o, &["region"]),
        icon_url: if array_item {
            String::new()
        } else {
            first(o, &["iconUrl", "icon_url"])
        },
        screenshots: if array_item {
            "[]".into()
        } else {
            screenshots(o)
        },
        description: text(o.get("description")),
        languages: first(o, &["languages"]),
    };
    entries.insert(id, entry);
}

struct TitlesSeed<'a> {
    entries: &'a mut HashMap<String, Entry>,
    patches: &'a mut HashMap<String, i64>,
}
struct TitlesVisitor<'a> {
    entries: &'a mut HashMap<String, Entry>,
    patches: &'a mut HashMap<String, i64>,
}
impl<'de> DeserializeSeed<'de> for TitlesSeed<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<(), D::Error> {
        deserializer.deserialize_any(TitlesVisitor {
            entries: self.entries,
            patches: self.patches,
        })
    }
}
impl<'de> Visitor<'de> for TitlesVisitor<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("title database object or array")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        let mut wrapped = false;
        while let Some(key) = map.next_key::<String>()? {
            if key == "titledb" {
                self.entries.clear();
                self.patches.clear();
                map.next_value_seed(TitlesSeed {
                    entries: &mut *self.entries,
                    patches: &mut *self.patches,
                })?;
                wrapped = true;
            } else {
                let value = map.next_value::<Value>()?;
                if !wrapped {
                    insert_streamed_title(&key, &value, false, self.entries, self.patches);
                }
            }
        }
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
        while let Some(value) = seq.next_element::<Value>()? {
            insert_streamed_title("", &value, true, self.entries, self.patches);
        }
        Ok(())
    }
}

struct VersionSeed<'a>(&'a mut HashMap<String, Entry>);
impl<'de> DeserializeSeed<'de> for VersionSeed<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<(), D::Error> {
        deserializer.deserialize_map(VersionVisitor(self.0))
    }
}
struct VersionVisitor<'a>(&'a mut HashMap<String, Entry>);
impl<'de> Visitor<'de> for VersionVisitor<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("versions JSON object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        while let Some(tid) = map.next_key::<String>()? {
            let value = map.next_value::<Value>()?;
            let Some(versions) = value.as_object() else {
                continue;
            };
            let latest = versions
                .keys()
                .filter_map(|s| s.parse::<i64>().ok())
                .max()
                .unwrap_or(0);
            let id = tid.to_ascii_uppercase();
            if let Some(e) = self.0.get_mut(&id) {
                e.version = e.version.max(latest);
            } else {
                self.0.insert(
                    id.clone(),
                    Entry {
                        title_id: id,
                        version: latest,
                        ..Entry::default()
                    },
                );
            }
        }
        Ok(())
    }
}

struct KoreanSeed<'a>(&'a mut HashMap<String, Entry>);
impl<'de> DeserializeSeed<'de> for KoreanSeed<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<(), D::Error> {
        deserializer.deserialize_map(KoreanVisitor(self.0))
    }
}
struct KoreanVisitor<'a>(&'a mut HashMap<String, Entry>);
impl<'de> Visitor<'de> for KoreanVisitor<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("regional title database object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        while map.next_key::<serde::de::IgnoredAny>()?.is_some() {
            let value = map.next_value::<Value>()?;
            let Some(o) = value.as_object() else {
                continue;
            };
            let id = first(o, &["id", "titleId"]).to_ascii_uppercase();
            if id.is_empty() {
                continue;
            }
            let name = first(o, &["name", "title"]);
            let description = text(o.get("description"));
            let entry = self.0.entry(id.clone()).or_insert_with(|| Entry {
                title_id: id,
                ..Entry::default()
            });
            if !name.is_empty() {
                entry.name = name;
            }
            if !description.is_empty() {
                entry.description = description;
            }
        }
        Ok(())
    }
}

fn title_mapping(path: &Path) -> Result<(Mmap, fs::File)> {
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ: reject concurrent writers/truncation while the
        // read-only mapped bytes are being parsed. Keep this handle open
        // until the mapping has been dropped.
        fs::OpenOptions::new().read(true).share_mode(0x1).open(path)
    };
    #[cfg(not(windows))]
    let file = fs::File::open(path);
    let file = file.with_context(|| format!("{} 파일을 읽지 못했습니다", path.display()))?;
    anyhow::ensure!(
        (1..=MAX_TITLE_DB_BYTES).contains(&file.metadata()?.len()),
        "{} 파일이 너무 큽니다",
        path.display()
    );
    // Windows keeps the read-only mapped pages file-backed. This eliminates
    // the 165-MB JSON byte Vec without the per-byte I/O overhead of Serde's
    // from_reader() deserializer; the mapping is dropped immediately after
    // one file is parsed. NSL never mutates title DB files during an import.
    Ok((unsafe { Mmap::map(&file)? }, file))
}

fn load_streamed_titles(path: &Path) -> Result<HashMap<String, Entry>> {
    let mut entries = HashMap::new();
    let mut patches = HashMap::new();
    let mapping = title_mapping(path)?;
    let mut de = serde_json::Deserializer::from_slice(&mapping.0);
    TitlesSeed {
        entries: &mut entries,
        patches: &mut patches,
    }
    .deserialize(&mut de)
    .with_context(|| format!("{} JSON 형식이 잘못되었습니다", path.display()))?;
    de.end()
        .with_context(|| format!("{} JSON 끝부분이 잘못되었습니다", path.display()))?;
    for (base, version) in patches {
        entries
            .entry(base.clone())
            .and_modify(|entry| entry.version = entry.version.max(version))
            .or_insert_with(|| Entry {
                title_id: base,
                version,
                ..Entry::default()
            });
    }
    Ok(entries)
}

fn merge_streamed_versions(path: &Path, entries: &mut HashMap<String, Entry>) -> Result<()> {
    let mapping = title_mapping(path)?;
    let mut de = serde_json::Deserializer::from_slice(&mapping.0);
    VersionSeed(entries)
        .deserialize(&mut de)
        .with_context(|| format!("{} JSON 형식이 잘못되었습니다", path.display()))?;
    de.end()
        .context("versions.json 파일 끝부분이 잘못되었습니다")?;
    Ok(())
}

fn merge_streamed_korean(path: &Path, entries: &mut HashMap<String, Entry>) -> Result<()> {
    let mapping = title_mapping(path)?;
    let mut de = serde_json::Deserializer::from_slice(&mapping.0);
    KoreanSeed(entries)
        .deserialize(&mut de)
        .with_context(|| format!("{} JSON 형식이 잘못되었습니다", path.display()))?;
    de.end()
        .context("KR.ko.json 파일 끝부분이 잘못되었습니다")?;
    Ok(())
}

#[cfg(test)]
pub fn load_local(dir: &Path) -> Result<Vec<Entry>> {
    load_local_with_progress(dir, None)
}

pub fn load_local_with_progress(dir: &Path, progress: Option<&TaskProgress>) -> Result<Vec<Entry>> {
    if let Some(progress) = progress {
        progress.begin("타이틀 DB 파일 분석", Some(3), false);
    }
    let mut titles = load_streamed_titles(&dir.join("titles.json"))?;
    anyhow::ensure!(
        !titles.is_empty()
            && titles
                .keys()
                .any(|id| id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())),
        "타이틀 DB가 비어 있거나 유효한 타이틀 ID가 없습니다. 이전 DB를 유지합니다"
    );
    if let Some(progress) = progress {
        progress.set(1);
    }
    let versions = dir.join("versions.json");
    if versions.exists() {
        merge_streamed_versions(&versions, &mut titles)?;
    }
    if let Some(progress) = progress {
        progress.set(2);
    }
    let korean = dir.join("KR.ko.json");
    if korean.exists() {
        merge_streamed_korean(&korean, &mut titles)?;
    }
    if let Some(progress) = progress {
        progress.set(3);
    }
    Ok(titles.into_values().collect())
}

const TITLE_FILES: [&str; 3] = ["titles.json", "versions.json", "KR.ko.json"];
const MAX_TITLE_DB_BYTES: u64 = 512 * 1024 * 1024;
const ROLLBACK_FOLDER: &str = ".nsl-title-rollback";
const CACHE_FILE: &str = ".nsl-title-http-cache.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateOutcome {
    AlreadyLatest,
    Updated(usize),
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct RemoteHeader {
    sha256: String,
    etag: Option<String>,
    last_modified: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct RemoteCache {
    source: String,
    files: BTreeMap<String, RemoteHeader>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RollbackMarker {
    token: String,
    existed: Vec<String>,
}

struct RemoteReply {
    not_modified: bool,
    etag: Option<String>,
    last_modified: Option<String>,
}

fn title_urls(source: &str) -> [(&'static str, &'static str); 3] {
    [
        (
            "titles.json",
            if source == "custom" {
                "https://github.com/UzuCore/NSL/releases/download/titledb/titles.json"
            } else {
                "https://tinfoil.media/repo/db/titles.json"
            },
        ),
        (
            "versions.json",
            "https://raw.githubusercontent.com/blawar/titledb/master/versions.json",
        ),
        (
            "KR.ko.json",
            "https://raw.githubusercontent.com/blawar/titledb/master/KR.ko.json",
        ),
    ]
}

fn file_hash(path: &Path) -> Result<Option<String>> {
    let mut reader = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(Some(format!("{:x}", hasher.finalize())))
}

fn revision_from_hashes(hashes: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for name in TITLE_FILES {
        hasher.update(name.as_bytes());
        hasher.update(b"=");
        hasher.update(
            hashes
                .get(name)
                .map(String::as_str)
                .unwrap_or("missing")
                .as_bytes(),
        );
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

pub fn local_revision(dir: &Path) -> Result<String> {
    let mut hashes = BTreeMap::new();
    for name in TITLE_FILES {
        let hash = file_hash(&dir.join(name))?.unwrap_or_else(|| "missing".into());
        hashes.insert(name.to_owned(), hash);
    }
    anyhow::ensure!(
        hashes
            .get("titles.json")
            .is_some_and(|hash| hash != "missing"),
        "로컬 titles.json 파일이 없습니다"
    );
    Ok(revision_from_hashes(&hashes))
}

pub fn import_local_if_needed(
    dir: &Path,
    db: &crate::database::Database,
    progress: Option<&TaskProgress>,
) -> Result<UpdateOutcome> {
    recover_transaction(dir, db)?;
    if let Some(progress) = progress {
        progress.begin("로컬 타이틀 DB 변경 확인", None, false);
    }
    let revision = local_revision(dir)?;
    if db.stats()?.title_db_count > 0 && db.title_revision()?.as_deref() == Some(&revision) {
        return Ok(UpdateOutcome::AlreadyLatest);
    }
    let entries = load_local_with_progress(dir, progress)?;
    if let Some(progress) = progress {
        progress.begin("로컬 타이틀 DB 레코드 반영", None, false);
    }
    db.load_titles_with_revision(&entries, None, Some(&revision))?;
    Ok(UpdateOutcome::Updated(entries.len()))
}

fn copy_replacing(source: &Path, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .context("타이틀 DB 파일 경로가 잘못되었습니다")?;
    let mut replacement = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut fs::File::open(source)?, &mut replacement)?;
    replacement.as_file().sync_all()?;
    replacement.persist(destination).map_err(|e| e.error)?;
    Ok(())
}

/// If a previous process died midway through a 3-file update, synchronize
/// files against the SQLite generation committed in the same DB transaction.
pub fn recover_transaction(dir: &Path, db: &crate::database::Database) -> Result<bool> {
    let backup = dir.join(ROLLBACK_FOLDER);
    if !backup.exists() {
        return Ok(false);
    }
    let marker_path = backup.join("ready.json");
    if !marker_path.is_file() {
        // No files can have been replaced before this marker is written.
        fs::remove_dir_all(&backup)?;
        return Ok(false);
    }
    let marker: RollbackMarker = serde_json::from_slice(&fs::read(&marker_path)?)
        .context("타이틀 DB 복구 정보가 손상되어 자동 복구를 중단했습니다")?;
    anyhow::ensure!(
        TITLE_FILES.iter().all(|name| {
            marker.existed.iter().any(|v| v == name) || !backup.join(name).exists()
        }),
        "타이틀 DB 복구 기록에 알 수 없는 파일이 있습니다"
    );
    // The database token is written atomically with the title rows. A matching
    // token proves that all file replacements and the DB update succeeded.
    if db.title_sync_token()?.as_deref() != Some(marker.token.as_str()) {
        for name in TITLE_FILES {
            let target = dir.join(name);
            if marker.existed.iter().any(|old| old == name) {
                let old = backup.join(name);
                anyhow::ensure!(
                    old.is_file(),
                    "타이틀 DB 이전 파일 {name}이 없어 안전한 복구를 진행할 수 없습니다"
                );
                copy_replacing(&old, &target)?;
            } else if target.exists() {
                fs::remove_file(&target)?;
            }
        }
    }
    fs::remove_dir_all(backup)?;
    Ok(true)
}

fn create_rollback(dir: &Path, token: &str) -> Result<()> {
    let backup = dir.join(ROLLBACK_FOLDER);
    anyhow::ensure!(
        !backup.exists(),
        "타이틀 DB의 이전 복구가 완료되지 않았습니다"
    );
    fs::create_dir(&backup)?;
    let mut existed = Vec::new();
    for name in TITLE_FILES {
        let path = dir.join(name);
        if path.is_file() {
            let old = backup.join(name);
            let mut output = fs::File::create(&old)?;
            std::io::copy(&mut fs::File::open(&path)?, &mut output)?;
            output.sync_all()?;
            existed.push(name.to_owned());
        }
    }
    crate::database::atomic_write(
        &backup.join("ready.json"),
        &serde_json::to_vec(&RollbackMarker {
            token: token.to_owned(),
            existed,
        })?,
    )?;
    Ok(())
}

fn remote_download(
    name: &str,
    url: &str,
    target: &Path,
    old: Option<&RemoteHeader>,
    progress: Option<&TaskProgress>,
) -> Result<RemoteReply> {
    if let Some(progress) = progress {
        progress.begin(format!("{name} 연결 중"), None, true);
    }
    let mut request = ureq::get(url)
        .timeout(Duration::from_secs(120))
        .set(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/124.0.0.0 Safari/537.36",
        )
        .set("Accept", "application/json, text/plain, */*");
    if let Some(header) = old {
        if let Some(etag) = &header.etag {
            request = request.set("If-None-Match", etag);
        } else if let Some(date) = &header.last_modified {
            request = request.set("If-Modified-Since", date);
        }
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(304, _)) if old.is_some() => {
            if let Some(progress) = progress {
                progress.begin(format!("{name} 최신 확인"), None, false);
            }
            let header = old.context("304 응답에 대응하는 기존 타이틀 파일이 없습니다")?;
            return Ok(RemoteReply {
                not_modified: true,
                etag: header.etag.clone(),
                last_modified: header.last_modified.clone(),
            });
        }
        Err(error) => anyhow::bail!("{name} 다운로드 실패: {error}"),
    };
    let etag = response.header("ETag").map(str::to_owned);
    let last_modified = response.header("Last-Modified").map(str::to_owned);
    if let Some(progress) = progress {
        let length = response
            .header("Content-Length")
            .and_then(|v| v.parse::<u64>().ok());
        progress.begin(format!("{name} 다운로드"), length, true);
    }
    let mut output = fs::File::create(target)?;
    let mut source = response.into_reader().take(MAX_TITLE_DB_BYTES + 1);
    let bytes = if let Some(progress) = progress {
        std::io::copy(&mut MeteredReader::new(&mut source, progress), &mut output)
    } else {
        std::io::copy(&mut source, &mut output)
    }
    .with_context(|| format!("{name} 다운로드 실패"))?;
    anyhow::ensure!(
        bytes > 0 && bytes <= MAX_TITLE_DB_BYTES,
        "{name} 응답 크기가 잘못되었습니다"
    );
    output.sync_all()?;
    Ok(RemoteReply {
        not_modified: false,
        etag,
        last_modified,
    })
}

/// Check remote JSONs first. An unchanged result never rewrites the title DB,
/// while a changed result stages *all three files* before a recoverable commit.
/// HTTP validators avoid fetching large JSON files on repeated checks.
pub fn download_and_apply(
    dir: &Path,
    source: &str,
    db: &crate::database::Database,
    progress: Option<&TaskProgress>,
) -> Result<UpdateOutcome> {
    update_with_fetch(dir, source, db, progress, remote_download)
}

fn update_with_fetch<F>(
    dir: &Path,
    source: &str,
    db: &crate::database::Database,
    progress: Option<&TaskProgress>,
    mut fetch: F,
) -> Result<UpdateOutcome>
where
    F: FnMut(
        &str,
        &str,
        &Path,
        Option<&RemoteHeader>,
        Option<&TaskProgress>,
    ) -> Result<RemoteReply>,
{
    fs::create_dir_all(dir)?;
    recover_transaction(dir, db)?;
    let cached: RemoteCache = fs::read(dir.join(CACHE_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let stage = tempfile::Builder::new()
        .prefix(".nsl-title-stage-")
        .tempdir_in(dir)?;
    let mut next_cache = RemoteCache {
        source: source.to_owned(),
        files: BTreeMap::new(),
    };
    let mut any_changed = false;
    for (name, url) in title_urls(source) {
        let target = dir.join(name);
        let prior_hash = file_hash(&target)?;
        let cached_header = cached.files.get(name).filter(|header| {
            cached.source == source && prior_hash.as_deref() == Some(header.sha256.as_str())
        });
        let staged = stage.path().join(name);
        let response = fetch(name, url, &staged, cached_header, progress)?;
        let hash = if response.not_modified {
            anyhow::ensure!(
                cached_header.is_some(),
                "변경 없음 응답의 캐시가 유효하지 않습니다"
            );
            prior_hash
                .clone()
                .context("기존 파일이 없어 변경 없음 응답을 처리하지 못했습니다")?
        } else {
            file_hash(&staged)?.context("다운로드 파일이 없습니다")?
        };
        any_changed |= prior_hash.as_deref() != Some(hash.as_str());
        next_cache.files.insert(
            name.to_owned(),
            RemoteHeader {
                sha256: hash,
                etag: response.etag,
                last_modified: response.last_modified,
            },
        );
        if let Some(progress) = progress {
            progress.begin(format!("{name} 변경 확인"), None, false);
        }
    }

    let stored_rows = db.stats()?.title_db_count;
    let revision = revision_from_hashes(
        &next_cache
            .files
            .iter()
            .map(|(name, header)| (name.clone(), header.sha256.clone()))
            .collect::<BTreeMap<_, _>>(),
    );
    if !any_changed && stored_rows > 0 && db.title_revision()?.as_deref() == Some(revision.as_str())
    {
        let _ = crate::database::atomic_write(
            &dir.join(CACHE_FILE),
            &serde_json::to_vec_pretty(&next_cache)?,
        );
        return Ok(UpdateOutcome::AlreadyLatest);
    }

    // HTTP 304 responses did not create staged files. If any remote file
    // changed, fill those gaps with their currently validated local copies.
    if any_changed {
        for name in TITLE_FILES {
            if !stage.path().join(name).exists() {
                fs::copy(dir.join(name), stage.path().join(name))?;
            }
        }
    }
    let input_dir = if any_changed { stage.path() } else { dir };
    let entries = load_local_with_progress(input_dir, progress)?;
    // Reject catastrophic accidental truncation even when the upstream JSON
    // remains syntactically valid (e.g. a partial provider dataset).
    if stored_rows >= 1000 {
        anyhow::ensure!(
            (entries.len() as u64) * 10 >= (stored_rows as u64) * 7,
            "타이틀 DB가 비정상적으로 축소되어 이전 {}건을 보존했습니다 (새 데이터 {}건)",
            stored_rows,
            entries.len(),
        );
    }
    if !any_changed {
        // Files are current, but the DB has never been imported or was cleared.
        db.load_titles_with_revision(&entries, None, Some(&revision))?;
    } else {
        let token = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        create_rollback(dir, &token)?;
        let apply_result = (|| -> Result<()> {
            for name in TITLE_FILES {
                copy_replacing(&stage.path().join(name), &dir.join(name))?;
            }
            if let Some(progress) = progress {
                progress.begin("타이틀 DB 레코드 반영", None, false);
            }
            db.load_titles_with_revision(&entries, Some(&token), Some(&revision))?;
            Ok(())
        })();
        if let Err(error) = apply_result {
            recover_transaction(dir, db)?;
            return Err(error);
        }
        // If a process crashes before the directory is removed, recovery
        // checks the SQLite generation token and keeps the successful update.
        let _ = fs::remove_dir_all(dir.join(ROLLBACK_FOLDER));
    }
    let _ = crate::database::atomic_write(
        &dir.join(CACHE_FILE),
        &serde_json::to_vec_pretty(&next_cache)?,
    );
    Ok(UpdateOutcome::Updated(entries.len()))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_title_parsing_matches_original_object_wrapper_and_array() {
        let cases = [
            r#"{"0100D2C0115B6000":{"name":"Base One","description":"  First  second  ","screenshots":["https://a.test/a.png","bad"],"releaseDate":"2026"},"0100D2C0115B6800":{"version":131072},"0100D2C0115B7001":{"id":"0100D2C0115B7001","name":"Extra DLC"}}"#,
            r#"{"titledb":{"0100D2C0115B6000":{"name":"Base One"},"0100D2C0115B6800":{"version":131072}}}"#,
            r#"[{"id":"0100D2C0115B6000","name":"  Name  ","description":["a","b"],"releaseDate":"2026"},{"id":"0100D2C0115B6800","version":131072}]"#,
        ];
        for (index, json) in cases.iter().enumerate() {
            let expected = parse_titles(serde_json::from_str(json).unwrap());
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("sample-{index}.json"));
            fs::write(&path, json).unwrap();
            let actual = load_streamed_titles(&path).unwrap();
            assert_eq!(actual.len(), expected.len());
            for (id, baseline) in expected {
                let result = &actual[&id];
                assert_eq!(result.title_id, baseline.title_id);
                assert_eq!(result.name, baseline.name);
                assert_eq!(result.version, baseline.version);
                assert_eq!(result.description, baseline.description);
                assert_eq!(result.screenshots, baseline.screenshots);
                assert_eq!(result.release_date, baseline.release_date);
                assert_eq!(result.icon_url, baseline.icon_url);
                assert_eq!(result.languages, baseline.languages);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn mapped_title_reader_denies_writers_during_decode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("titles.json");
        fs::write(&path, b"{}").unwrap();
        let mapping = title_mapping(&path).unwrap();
        assert_eq!(&mapping.0[..], b"{}");
        assert!(fs::OpenOptions::new().write(true).open(&path).is_err());
        drop(mapping);
        assert!(fs::OpenOptions::new().write(true).open(&path).is_ok());
    }

    #[test]
    #[ignore = "large read-only local title DB benchmark"]
    fn real_title_database_import_perf_and_parity() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("titledb");
        let started = std::time::Instant::now();
        let entries = load_local(&dir).unwrap();
        assert!(entries.len() > 40_000, "expected the real title database");
        eprintln!(
            "LOCAL_TITLEDB_PARSE entries={} elapsed={:.3}s",
            entries.len(),
            started.elapsed().as_secs_f64()
        );
    }

    fn fixture(name: &str, title: &str) -> Vec<u8> {
        match name {
            "titles.json" => serde_json::to_vec(&serde_json::json!({
                "0100D2C0115B6000": {"id":"0100D2C0115B6000","name":title}
            }))
            .unwrap(),
            "versions.json" => serde_json::to_vec(&serde_json::json!({
                "0100D2C0115B6000": {"65536": {}}
            }))
            .unwrap(),
            "KR.ko.json" => serde_json::to_vec(&serde_json::json!({
                "0": {"id":"0100D2C0115B6000","name":title}
            }))
            .unwrap(),
            _ => panic!("unexpected fixture"),
        }
    }

    fn fixture_db(dir: &Path) -> crate::database::Database {
        let db = crate::database::Database::open(dir.join("library.db")).unwrap();
        db.init().unwrap();
        db
    }

    #[test]
    fn identical_downloads_report_latest_without_touching_db_or_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("titledb");
        let db = fixture_db(temp.path());
        let fetch = |name: &str,
                     _url: &str,
                     target: &Path,
                     _old: Option<&RemoteHeader>,
                     _: Option<&TaskProgress>| {
            fs::write(target, fixture(name, "한국어 이름"))?;
            Ok(RemoteReply {
                not_modified: false,
                etag: Some("etag-1".into()),
                last_modified: None,
            })
        };
        assert_eq!(
            update_with_fetch(&dir, "custom", &db, None, fetch).unwrap(),
            UpdateOutcome::Updated(1)
        );
        let old_token = db.title_sync_token().unwrap();
        let old_titles = fs::read(dir.join("titles.json")).unwrap();
        let mut conditional_requests = 0;
        let fetch =
            |_: &str, _: &str, _: &Path, old: Option<&RemoteHeader>, _: Option<&TaskProgress>| {
                if old.is_some() {
                    conditional_requests += 1;
                }
                Ok(RemoteReply {
                    not_modified: true,
                    etag: Some("etag-1".into()),
                    last_modified: None,
                })
            };
        assert_eq!(
            update_with_fetch(&dir, "custom", &db, None, fetch).unwrap(),
            UpdateOutcome::AlreadyLatest
        );
        assert_eq!(conditional_requests, 3);
        assert_eq!(db.title_sync_token().unwrap(), old_token);
        assert_eq!(fs::read(dir.join("titles.json")).unwrap(), old_titles);
        assert_eq!(db.stats().unwrap().title_db_count, 1);
    }

    #[test]
    fn unknown_prior_database_revision_requires_one_verified_import() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("titledb");
        fs::create_dir_all(&dir).unwrap();
        let db = fixture_db(tmp.path());
        for name in TITLE_FILES {
            fs::write(dir.join(name), fixture(name, "Original")).unwrap();
        }
        db.load_titles(&[Entry {
            title_id: "0100D2C0115B6000".into(),
            name: "Outdated".into(),
            ..Default::default()
        }])
        .unwrap();
        assert_eq!(db.title_revision().unwrap(), None);
        let outcome = update_with_fetch(&dir, "custom", &db, None, |name, _, target, _, _| {
            fs::write(target, fixture(name, "Original"))?;
            Ok(RemoteReply {
                not_modified: false,
                etag: None,
                last_modified: None,
            })
        })
        .unwrap();
        assert_eq!(outcome, UpdateOutcome::Updated(1));
        assert!(db.title_revision().unwrap().is_some());
        assert_eq!(db.stats().unwrap().title_db_count, 1);
    }

    #[test]
    fn invalid_or_missing_third_download_keeps_all_old_files_and_db() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("titledb");
        let db = fixture_db(temp.path());
        update_with_fetch(&dir, "custom", &db, None, |name, _, target, _, _| {
            fs::write(target, fixture(name, "Original"))?;
            Ok(RemoteReply {
                not_modified: false,
                etag: None,
                last_modified: None,
            })
        })
        .unwrap();
        let before = TITLE_FILES
            .iter()
            .map(|name| fs::read(dir.join(name)).unwrap())
            .collect::<Vec<_>>();
        let result = update_with_fetch(&dir, "custom", &db, None, |name, _, target, _, _| {
            if name == "KR.ko.json" {
                anyhow::bail!("injected network failure");
            }
            fs::write(target, fixture(name, "Changed"))?;
            Ok(RemoteReply {
                not_modified: false,
                etag: None,
                last_modified: None,
            })
        });
        assert!(result.unwrap_err().to_string().contains("injected"));
        for (name, expected) in TITLE_FILES.iter().zip(before) {
            assert_eq!(fs::read(dir.join(name)).unwrap(), expected);
        }
        assert!(!dir.join(ROLLBACK_FOLDER).exists());
        assert_eq!(db.stats().unwrap().title_db_count, 1);
    }

    #[test]
    fn failed_sqlite_commit_restores_previous_files_and_title_rows() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("titledb");
        let db = fixture_db(temp.path());
        update_with_fetch(&dir, "custom", &db, None, |name, _, target, _, _| {
            fs::write(target, fixture(name, "Original"))?;
            Ok(RemoteReply {
                not_modified: false,
                etag: None,
                last_modified: None,
            })
        })
        .unwrap();
        let original = TITLE_FILES
            .iter()
            .map(|name| fs::read(dir.join(name)).unwrap())
            .collect::<Vec<_>>();
        let conn = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_changed BEFORE INSERT ON title_db
            WHEN NEW.name='Changed' BEGIN SELECT RAISE(ABORT,'injected DB failure'); END;",
        )
        .unwrap();
        let result = update_with_fetch(&dir, "custom", &db, None, |name, _, target, _, _| {
            fs::write(target, fixture(name, "Changed"))?;
            Ok(RemoteReply {
                not_modified: false,
                etag: None,
                last_modified: None,
            })
        });
        assert!(result.is_err());
        for (name, expected) in TITLE_FILES.iter().zip(original) {
            assert_eq!(fs::read(dir.join(name)).unwrap(), expected);
        }
        assert_eq!(db.stats().unwrap().title_db_count, 1);
        assert!(!dir.join(ROLLBACK_FOLDER).exists());
    }

    #[test]
    fn recovery_keeps_committed_generation_but_rolls_back_incomplete_one() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("titledb");
        fs::create_dir_all(&dir).unwrap();
        let db = fixture_db(temp.path());
        for name in TITLE_FILES {
            fs::write(dir.join(name), fixture(name, "Original")).unwrap();
        }
        create_rollback(&dir, "not-committed").unwrap();
        fs::write(
            dir.join("titles.json"),
            fixture("titles.json", "Interrupted"),
        )
        .unwrap();
        assert!(recover_transaction(&dir, &db).unwrap());
        assert_eq!(
            fs::read(dir.join("titles.json")).unwrap(),
            fixture("titles.json", "Original")
        );
        let entries = load_local_with_progress(&dir, None).unwrap();
        create_rollback(&dir, "committed").unwrap();
        db.load_titles_with_token(&entries, Some("committed"))
            .unwrap();
        fs::write(dir.join("titles.json"), fixture("titles.json", "Committed")).unwrap();
        assert!(recover_transaction(&dir, &db).unwrap());
        assert_eq!(
            fs::read(dir.join("titles.json")).unwrap(),
            fixture("titles.json", "Committed")
        );
    }

    #[test]
    fn blank_valid_json_is_rejected_without_erasing_database() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("titledb");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("titles.json"), "{}").unwrap();
        let db = fixture_db(temp.path());
        db.load_titles(&[Entry {
            title_id: "0100D2C0115B6000".into(),
            name: "Original".into(),
            ..Default::default()
        }])
        .unwrap();
        assert!(load_local_with_progress(&dir, None).is_err());
        assert!(db.load_titles(&[]).is_err());
        assert_eq!(db.stats().unwrap().title_db_count, 1);
    }

    #[test]
    fn local_matching_skips_identical_files_until_revision_changes() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("titledb");
        fs::create_dir_all(&dir).unwrap();
        let db = fixture_db(temp.path());
        for name in TITLE_FILES {
            fs::write(dir.join(name), fixture(name, "Old Title")).unwrap();
        }
        assert_eq!(
            import_local_if_needed(&dir, &db, None).unwrap(),
            UpdateOutcome::Updated(1)
        );
        assert_eq!(
            import_local_if_needed(&dir, &db, None).unwrap(),
            UpdateOutcome::AlreadyLatest
        );
        fs::write(dir.join("titles.json"), fixture("titles.json", "New Title")).unwrap();
        assert_eq!(
            import_local_if_needed(&dir, &db, None).unwrap(),
            UpdateOutcome::Updated(1)
        );
        db.clear_titles().unwrap();
        assert_eq!(
            import_local_if_needed(&dir, &db, None).unwrap(),
            UpdateOutcome::Updated(1)
        );
    }

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
