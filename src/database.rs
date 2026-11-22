use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::{Path, PathBuf},
};

/// Create a consistent, read-only SQLite snapshot without a Python runtime
/// or an unsafe copy of a live WAL database. The destination must be new.
pub fn backup_read_only(source: &Path, destination: &Path) -> Result<()> {
    anyhow::ensure!(
        source.is_file(),
        "source database is missing: {}",
        source.display()
    );
    anyhow::ensure!(
        !destination.exists(),
        "refusing to overwrite an existing database: {}",
        destination.display()
    );
    let source_db = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open source DB read-only: {}", source.display()))?;
    // create_new rejects a concurrent writer after the preceding check.
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("reserve new snapshot path: {}", destination.display()))?;
    let mut target_db = Connection::open(destination)?;
    let backup = rusqlite::backup::Backup::new(&source_db, &mut target_db)?;
    backup.run_to_completion(128, std::time::Duration::from_millis(10), None)?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct FileRecord {
    pub path: String,
    pub filename: String,
    pub size: i64,
    pub mtime: Option<f64>,
    pub file_type: String,
    pub scan_folder: String,
    pub fast_hash: String,
    pub nacp_ko: String,
    pub nacp_en: String,
    pub nacp_ja: String,
    pub contents: Vec<crate::scanner::Content>,
}

#[derive(Clone, Debug)]
pub struct GameFile {
    pub id: i64,
    pub path: String,
    pub filename: String,
    pub size: i64,
    pub file_type: String,
    pub content_type: String,
    pub title_id: String,
    pub title_name: String,
    pub version: i64,
    pub has_base: bool,
    /// Only decoded CNMT metadata can authorize a batch merge candidate.
    pub verified_base: bool,
    pub has_update: bool,
    pub update_version: i64,
    pub latest_version: i64,
    pub dlc_count: usize,
    pub db_dlc_count: usize,
    pub region: String,
    pub icon_url: String,
    pub description: String,
    pub screenshots: String,
    pub languages: String,
    pub nacp_ko: String,
    pub nacp_en: String,
    pub nacp_ja: String,
    pub owned_dlcs: Vec<(String, String)>,
    pub missing_dlcs: Vec<(String, String)>,
}

/// Store metadata for owned and unowned catalog titles. Loaded in indexed
/// batches in a worker, not on the UI frame/render thread.
#[derive(Clone, Debug, Default)]
pub struct CatalogTitleDetails {
    pub name: String,
    pub version: i64,
    pub release_date: String,
    pub num_players: String,
    pub rating: String,
    pub region: String,
    pub screenshots: String,
    pub languages: String,
}

#[derive(Clone)]
struct Content {
    title_id: Option<String>,
    version: i64,
    kind: String,
    source: String,
}
#[derive(Default)]
struct Group {
    max_version: i64,
    has_update: bool,
    dlcs: HashSet<String>,
}
#[derive(Default, Clone)]
struct Title {
    name: String,
    version: i64,
    region: String,
    description: String,
    screenshots: String,
    languages: String,
    icon_url: String,
}

fn base_tid(tid: &str) -> String {
    u64::from_str_radix(tid, 16)
        .map(|v| format!("{:016X}", v & 0xFFFFFFFFFFFFE000))
        .unwrap_or_else(|_| {
            if tid.len() >= 13 {
                format!("{}000", &tid[..13]).to_ascii_uppercase()
            } else {
                String::new()
            }
        })
}

fn contains_cjk(s: &str) -> bool {
    s.chars().any(|c| matches!(c as u32, 0x3040..=0x30ff | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0xff65..=0xff9f))
}

fn fallback_name(filename: &str) -> String {
    std::path::Path::new(filename)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("Unknown")
        .to_owned()
}

/// One case/slash-independent key for Windows file paths. Preserve the
/// original spelling in the database for display and disk operations.
fn library_path_identity(path: &str) -> String {
    let normalized = path.replace('/', "\\");
    normalized
        .strip_prefix("\\\\?\\")
        .unwrap_or(&normalized)
        .trim_end_matches('\\')
        .to_lowercase()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub scan_folders: Vec<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// mtime, size, fast hash for a path already present in the database.
pub type KnownFile = (Option<f64>, i64, String);

#[derive(Clone, Copy, Debug, Default)]
pub struct LibraryStats {
    pub file_count: i64,
    pub total_size: i64,
    pub base_count: i64,
    pub update_count: i64,
    pub dlc_count: i64,
    pub title_db_count: i64,
}

pub fn load_config(path: &Path) -> Result<Config> {
    let backup = path.with_extension("json.bak");
    if !path.exists() {
        if backup.exists() {
            return serde_json::from_slice(&std::fs::read(&backup)?)
                .with_context(|| format!("설정 백업을 읽을 수 없습니다: {}", backup.display()));
        }
        return Ok(Config::default());
    }
    let bytes = std::fs::read(path)?;
    match serde_json::from_slice(&bytes) {
        Ok(config) => Ok(config),
        Err(error) => {
            // Never silently reset a user's settings when a write was interrupted.
            if let Ok(bytes) = std::fs::read(&backup) {
                if let Ok(config) = serde_json::from_slice(&bytes) {
                    return Ok(config);
                }
            }
            Err(error).with_context(|| format!("설정 파일이 손상되었습니다: {}", path.display()))
        }
    }
}

pub fn save_config(path: &Path, config: &Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        // Back up only valid data: a corrupt primary must not replace its
        // last good backup on the first save after recovery.
        let old = std::fs::read(path)?;
        if serde_json::from_slice::<Config>(&old).is_ok() {
            let backup = path.with_extension("json.bak");
            atomic_write(&backup, &old)?;
        }
    }
    atomic_write(path, &serde_json::to_vec_pretty(config)?)?;
    Ok(())
}

/// Flush a same-directory temporary file, then atomically replace the target.
/// `NamedTempFile::persist` replaces an existing target on Windows as well.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[derive(Clone)]
pub struct Database {
    path: PathBuf,
}
impl Database {
    pub fn open(path: PathBuf) -> Result<Self> {
        Ok(Self { path })
    }
    fn connect(&self) -> Result<Connection> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&self.path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Keep the per-connection SQLite page cache bounded. The library may
        // have read and background-scan connections open simultaneously.
        conn.pragma_update(None, "cache_size", -16384)?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        Ok(conn)
    }
    pub fn init(&self) -> Result<()> {
        let conn = self.connect()?;
        // WAL mode is persistent: setting it once on startup avoids needless
        // journal-mode checks and potential locks on every read-only query.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;
        CREATE TABLE IF NOT EXISTS game_files (
            id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT UNIQUE NOT NULL,
            filename TEXT NOT NULL, size INTEGER DEFAULT 0, mtime REAL, fast_hash TEXT,
            file_type TEXT, last_seen TEXT, scan_folder TEXT,
            nacp_ko TEXT DEFAULT '', nacp_en TEXT DEFAULT '', nacp_ja TEXT DEFAULT '');
        CREATE TABLE IF NOT EXISTS file_contents (
            id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL, title_id TEXT,
            version INTEGER DEFAULT 0, content_type TEXT DEFAULT 'unknown',
            source TEXT DEFAULT 'filename', UNIQUE(path,title_id,content_type));
        CREATE INDEX IF NOT EXISTS idx_fc_path ON file_contents(path);
        CREATE INDEX IF NOT EXISTS idx_fc_title_id ON file_contents(title_id);
        CREATE TABLE IF NOT EXISTS title_db (
            title_id TEXT PRIMARY KEY, name TEXT, version INTEGER DEFAULT 0,
            release_date TEXT, num_players TEXT, rating TEXT, size INTEGER DEFAULT 0,
            region TEXT, icon_url TEXT, screenshots TEXT DEFAULT '[]', description TEXT, languages TEXT);
        CREATE TABLE IF NOT EXISTS nsl_sync_meta (
            key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS idx_titledb_base ON title_db(UPPER(SUBSTR(title_id,1,13))||'000');
        CREATE INDEX IF NOT EXISTS idx_titledb_family ON title_db(UPPER(SUBSTR(title_id,1,12)));")?;
        // Existing 1.4.x databases may predate localized NACP columns.
        if let Ok(conn) = self.connect() {
            for col in ["nacp_ko", "nacp_en", "nacp_ja"] {
                let _ = conn.execute_batch(&format!(
                    "ALTER TABLE game_files ADD COLUMN {col} TEXT DEFAULT ''"
                ));
            }
        }
        Ok(())
    }
    pub fn stats(&self) -> Result<LibraryStats> {
        let conn = self.connect()?;
        Ok(conn.query_row(
            "SELECT
                (SELECT COUNT(*) FROM game_files),
                (SELECT COALESCE(SUM(size), 0) FROM game_files),
                (SELECT COUNT(*) FROM file_contents WHERE content_type='base'),
                (SELECT COUNT(*) FROM file_contents WHERE content_type='update'),
                (SELECT COUNT(*) FROM file_contents WHERE content_type='dlc'),
                (SELECT COUNT(*) FROM title_db)",
            [],
            |row| {
                Ok(LibraryStats {
                    file_count: row.get(0)?,
                    total_size: row.get(1)?,
                    base_count: row.get(2)?,
                    update_count: row.get(3)?,
                    dlc_count: row.get(4)?,
                    title_db_count: row.get(5)?,
                })
            },
        )?)
    }
    /// A Store catalog can contain thousands of uninstalled titles. Resolve
    /// their metadata by indexed title_id lookups in bounded SQLite batches.
    /// Full descriptions are intentionally excluded from Store metadata.
    pub fn catalog_title_details(
        &self,
        base_title_ids: &HashSet<String>,
    ) -> Result<HashMap<String, CatalogTitleDetails>> {
        let mut ids = base_title_ids
            .iter()
            .filter(|id| id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(|id| id.to_ascii_uppercase())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        let conn = self.connect()?;
        let mut result = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT title_id,COALESCE(name,''),COALESCE(version,0),\
                 COALESCE(release_date,''),COALESCE(num_players,''),COALESCE(rating,''),\
                 COALESCE(region,''),COALESCE(screenshots,'[]'),\
                 COALESCE(languages,'') FROM title_db WHERE title_id IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    CatalogTitleDetails {
                        name: row.get(1)?,
                        version: row.get(2)?,
                        release_date: row.get(3)?,
                        num_players: row.get(4)?,
                        rating: row.get(5)?,
                        region: row.get(6)?,
                        screenshots: row.get(7)?,
                        languages: row.get(8)?,
                    },
                ))
            })?;
            for item in rows {
                let (id, details) = item?;
                result.insert(id.to_ascii_uppercase(), details);
            }
        }
        Ok(result)
    }

    pub fn files(&self, name_mode: &str) -> Result<Vec<GameFile>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare("SELECT gf.id,gf.path,gf.filename,gf.size,COALESCE(gf.file_type,''),COALESCE(gf.nacp_ko,''),COALESCE(gf.nacp_en,''),COALESCE(gf.nacp_ja,''),fc.title_id,COALESCE(fc.version,0),COALESCE(fc.content_type,'unknown'),COALESCE(fc.source,'none') FROM game_files gf LEFT JOIN file_contents fc ON gf.path=fc.path ORDER BY gf.path")?;
        let raw = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                Content {
                    title_id: r.get(8)?,
                    version: r.get(9)?,
                    kind: r.get(10)?,
                    source: r.get(11)?,
                },
            ))
        })?;
        let mut by_path: HashMap<String, Vec<Content>> = HashMap::new();
        let mut file_data = HashMap::new();
        for record in raw {
            let (id, path, filename, size, file_type, ko, en, ja, c) = record?;
            file_data.entry(path.clone()).or_insert((
                id,
                path.clone(),
                filename,
                size,
                file_type,
                ko,
                en,
                ja,
            ));
            if c.title_id.is_some() || c.kind != "unknown" {
                by_path.entry(path).or_default().push(c);
            }
        }
        let mut titles = HashMap::new();
        // Only load metadata for installed titles and their potential DLC.
        // A DLC id can differ in its 13th hex digit from its base (e.g. 6xxx
        // versus 7xxx). Matching the first 12 digits retains every DLC while
        // avoiding materializing unrelated descriptions/screenshots (~45 MB
        // on the reference library) at each refresh.
        let mut tstmt = conn.prepare("SELECT title_id,COALESCE(name,''),COALESCE(version,0),COALESCE(region,''),COALESCE(description,''),COALESCE(screenshots,'[]'),COALESCE(languages,''),COALESCE(icon_url,'') FROM title_db WHERE UPPER(SUBSTR(title_id,1,12)) IN (SELECT DISTINCT UPPER(SUBSTR(title_id,1,12)) FROM file_contents WHERE LENGTH(title_id)=16)")?;
        for row in tstmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Title {
                    name: r.get(1)?,
                    version: r.get(2)?,
                    region: r.get(3)?,
                    description: r.get(4)?,
                    screenshots: r.get(5)?,
                    languages: r.get(6)?,
                    icon_url: r.get(7)?,
                },
            ))
        })? {
            let (id, t) = row?;
            titles.insert(id, t);
        }
        drop(tstmt);
        drop(stmt);
        drop(conn);
        let mut groups: HashMap<String, Group> = HashMap::new();
        for cs in by_path.values() {
            for c in cs {
                if let Some(tid) = &c.title_id {
                    if tid.len() >= 13 {
                        let g = groups.entry(base_tid(tid)).or_default();
                        match c.kind.as_str() {
                            "update" => {
                                g.has_update = true;
                                g.max_version = g.max_version.max(c.version)
                            }
                            "base" => g.max_version = g.max_version.max(c.version),
                            "dlc" => {
                                g.dlcs.insert(tid.clone());
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        // DLC lookups also test ownership, so use O(1) membership instead of
        // linear scans over potentially hundreds of add-ons per game.
        let mut db_dlcs: HashMap<String, HashSet<String>> = HashMap::new();
        for tid in titles.keys() {
            if let Ok(v) = u64::from_str_radix(tid, 16) {
                if tid.len() == 16 && (v & 0x1fff) >= 0x1000 {
                    db_dlcs
                        .entry(format!("{:016X}", v & 0xFFFFFFFFFFFFE000))
                        .or_default()
                        .insert(tid.clone());
                }
            }
        }
        let mut result = Vec::new();
        for (path, (id, _, filename, size, file_type, ko, en, ja)) in file_data {
            let cs = by_path.get(&path).cloned().unwrap_or_default();
            let best = cs
                .iter()
                .find(|c| c.kind == "base")
                .or_else(|| cs.iter().find(|c| c.kind == "update"))
                .or_else(|| cs.first());
            let tid = best.and_then(|c| c.title_id.clone()).unwrap_or_default();
            let version = best.map(|c| c.version).unwrap_or(0);
            let content_type = best
                .map(|c| c.kind.clone())
                .unwrap_or_else(|| "unknown".into());
            let base = if tid.len() >= 13 {
                base_tid(&tid)
            } else {
                String::new()
            };
            let t = titles.get(&base).cloned().unwrap_or_default();
            let g = groups.get(&base);
            let has_base = cs.iter().any(|c| c.kind == "base");
            // A filename, ticket, inferred base, or legacy cache record is
            // display-only and must never authorize an automatic merge.
            let verified_base = cs.iter().any(|c| {
                c.kind == "base"
                    && matches!(
                        c.source.as_str(),
                        "header_cnmt" | "header_native" | "header_native_card"
                    )
                    && c.title_id.as_deref() == Some(base.as_str())
            });
            let has_update =
                cs.iter().any(|c| c.kind == "update") || g.map(|x| x.has_update).unwrap_or(false);
            let update_version = cs
                .iter()
                .filter(|c| c.kind == "update")
                .map(|c| c.version)
                .max()
                .unwrap_or(0)
                .max(g.map(|x| x.max_version).unwrap_or(0));
            let dlc_count = cs
                .iter()
                .filter(|c| c.kind == "dlc")
                .count()
                .max(g.map(|x| x.dlcs.len()).unwrap_or(0));
            let title_name = match name_mode {
                "ko_en_db" => {
                    if !ko.is_empty() {
                        ko.clone()
                    } else if !en.is_empty() {
                        en.clone()
                    } else if !t.name.is_empty() && !contains_cjk(&t.name) {
                        t.name.clone()
                    } else if !ja.is_empty() {
                        ja.clone()
                    } else if !t.name.is_empty() {
                        t.name.clone()
                    } else {
                        fallback_name(&filename)
                    }
                }
                "ko_ja_db" => {
                    if !ko.is_empty() {
                        ko.clone()
                    } else if !ja.is_empty() {
                        ja.clone()
                    } else if !t.name.is_empty() {
                        t.name.clone()
                    } else {
                        fallback_name(&filename)
                    }
                }
                _ => {
                    if !t.name.is_empty() {
                        t.name.clone()
                    } else {
                        fallback_name(&filename)
                    }
                }
            };
            let mut owned_dlcs = Vec::new();
            let mut missing_dlcs = Vec::new();
            let owned = g.map(|x| &x.dlcs);
            for dlc_tid in db_dlcs.get(&base).into_iter().flatten() {
                let name = titles
                    .get(dlc_tid)
                    .map(|x| x.name.clone())
                    .filter(|x| !x.is_empty())
                    .unwrap_or_else(|| "Unknown DLC".into());
                if owned.map(|s| s.contains(dlc_tid)).unwrap_or(false) {
                    owned_dlcs.push((dlc_tid.clone(), name))
                } else {
                    missing_dlcs.push((dlc_tid.clone(), name))
                }
            }
            if let Some(owned) = owned {
                for tid in owned {
                    if !db_dlcs.get(&base).map(|v| v.contains(tid)).unwrap_or(false) {
                        owned_dlcs.push((tid.clone(), "Unknown DLC (DB 미등록)".into()));
                    }
                }
            }
            owned_dlcs.sort_by(|a, b| a.0.cmp(&b.0));
            missing_dlcs.sort_by(|a, b| a.0.cmp(&b.0));
            result.push(GameFile {
                id,
                path,
                filename,
                size,
                file_type,
                content_type,
                title_id: tid,
                title_name,
                version,
                has_base,
                verified_base,
                has_update,
                update_version,
                latest_version: t.version,
                dlc_count,
                db_dlc_count: db_dlcs.get(&base).map(|v| v.len()).unwrap_or(0),
                region: t.region,
                icon_url: t.icon_url,
                description: t.description,
                screenshots: t.screenshots,
                languages: t.languages,
                nacp_ko: ko,
                nacp_en: en,
                nacp_ja: ja,
                owned_dlcs,
                missing_dlcs,
            });
        }
        result.sort_by_key(|x| x.title_name.to_lowercase());
        Ok(result)
    }
    pub fn upsert_files(&self, files: &[FileRecord]) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        // Reconcile legacy mixed-case and slash-style paths before inserting.
        // Otherwise the same physical file appears twice after re-scanning.
        let mut old_paths = HashMap::<String, String>::new();
        {
            let mut stmt = tx.prepare("SELECT path FROM game_files")?;
            for path in stmt.query_map([], |row| row.get::<_, String>(0))? {
                let path = path?;
                old_paths.insert(library_path_identity(&path), path);
            }
        }
        // Prepare each SQL statement only once per batch. A full scan can
        // produce thousands of content rows, and cloning the whole Content
        // vector (plus formatting the timestamp per file) was avoidable.
        let last_seen = chrono::Utc::now().to_rfc3339();
        {
            let mut file_stmt = tx.prepare("INSERT INTO game_files(path,filename,size,mtime,fast_hash,file_type,last_seen,scan_folder,nacp_ko,nacp_en,nacp_ja)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(path) DO UPDATE SET
                filename=excluded.filename,size=excluded.size,mtime=excluded.mtime,
                fast_hash=excluded.fast_hash,file_type=excluded.file_type,last_seen=excluded.last_seen,scan_folder=excluded.scan_folder,
                nacp_ko=excluded.nacp_ko,nacp_en=excluded.nacp_en,nacp_ja=excluded.nacp_ja")?;
            let mut clear_stmt = tx.prepare("DELETE FROM file_contents WHERE path=?1")?;
            let mut delete_file = tx.prepare("DELETE FROM game_files WHERE path=?1")?;
            let mut content_stmt = tx.prepare("INSERT OR IGNORE INTO file_contents(path,title_id,version,content_type,source) VALUES(?1,?2,?3,?4,?5)")?;
            for f in files {
                let key = library_path_identity(&f.path);
                if let Some(previous) = old_paths.get(&key) {
                    if previous != &f.path {
                        clear_stmt.execute([previous])?;
                        delete_file.execute([previous])?;
                    }
                }
                old_paths.insert(key, f.path.clone());
                file_stmt.execute(params![
                    f.path,
                    f.filename,
                    f.size,
                    f.mtime,
                    f.fast_hash,
                    f.file_type,
                    last_seen,
                    f.scan_folder,
                    f.nacp_ko,
                    f.nacp_en,
                    f.nacp_ja
                ])?;
                clear_stmt.execute([&f.path])?;
                if f.contents.is_empty() {
                    content_stmt.execute(params![
                        f.path,
                        Option::<&str>::None,
                        0,
                        "unknown",
                        "none"
                    ])?;
                } else {
                    for c in &f.contents {
                        content_stmt.execute(params![
                            f.path,
                            c.title_id.as_deref(),
                            c.version,
                            c.content_type,
                            c.source
                        ])?;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
    #[cfg(test)]
    pub fn load_titles(&self, entries: &[crate::titledb::Entry]) -> Result<()> {
        self.load_titles_with_token(entries, None)
    }

    /// The generation marker is committed in the very same SQLite transaction
    /// as the new title rows. On crash recovery it tells us whether to keep
    /// the swapped files or restore the previous version.
    #[cfg(test)]
    pub fn load_titles_with_token(
        &self,
        entries: &[crate::titledb::Entry],
        generation: Option<&str>,
    ) -> Result<()> {
        self.load_titles_with_revision(entries, generation, None)
    }

    pub fn load_titles_with_revision(
        &self,
        entries: &[crate::titledb::Entry],
        generation: Option<&str>,
        revision: Option<&str>,
    ) -> Result<()> {
        anyhow::ensure!(!entries.is_empty(), "빈 타이틀 DB 반영을 차단했습니다");
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM title_db", [])?;
        {
            let mut stmt = tx.prepare("INSERT OR REPLACE INTO title_db(title_id,name,version,release_date,num_players,rating,size,region,icon_url,screenshots,description,languages) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)")?;
            for e in entries {
                stmt.execute(params![
                    e.title_id,
                    e.name,
                    e.version,
                    e.release_date,
                    e.num_players,
                    e.rating,
                    e.size,
                    e.region,
                    e.icon_url,
                    if e.screenshots.is_empty() {
                        "[]"
                    } else {
                        &e.screenshots
                    },
                    e.description,
                    e.languages
                ])?;
            }
        }
        if let Some(generation) = generation {
            tx.execute(
                "INSERT INTO nsl_sync_meta(key,value) VALUES('title_db_generation',?1) \
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [generation],
            )?;
        }
        if let Some(revision) = revision {
            tx.execute(
                "INSERT INTO nsl_sync_meta(key,value) VALUES('title_db_revision',?1) \
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [revision],
            )?;
        } else {
            tx.execute(
                "DELETE FROM nsl_sync_meta WHERE key='title_db_revision'",
                [],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn title_sync_token(&self) -> Result<Option<String>> {
        self.sync_meta_value("title_db_generation")
    }

    pub fn title_revision(&self) -> Result<Option<String>> {
        self.sync_meta_value("title_db_revision")
    }

    fn sync_meta_value(&self, key: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .connect()?
            .query_row(
                "SELECT value FROM nsl_sync_meta WHERE key=?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .optional()?)
    }
    pub fn clear_titles(&self) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM title_db", [])?;
        tx.execute(
            "DELETE FROM nsl_sync_meta WHERE key='title_db_revision'",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn clear_library_data(&self) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM file_contents", [])?;
        tx.execute("DELETE FROM game_files", [])?;
        tx.execute("DELETE FROM title_db", [])?;
        tx.execute(
            "DELETE FROM nsl_sync_meta WHERE key='title_db_revision'",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn content_title_ids(&self) -> Result<HashSet<String>> {
        let conn = self.connect()?;
        let mut stmt =
            conn.prepare("SELECT DISTINCT title_id FROM file_contents WHERE title_id IS NOT NULL")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        Ok(ids)
    }
    pub fn delete_missing_files_within(&self, paths: &[String], roots: &[String]) -> Result<usize> {
        // Empty `paths` is meaningful: the scanned roots are reachable but contain
        // no game files, so stale rows under those roots should be pruned.
        if roots.is_empty() {
            return Ok(0);
        }
        let keep: std::collections::HashSet<_> = paths
            .iter()
            .map(|path| library_path_identity(path))
            .collect();
        let roots: Vec<String> = roots
            .iter()
            .map(|root| library_path_identity(root))
            .collect();
        let conn = self.connect()?;
        let tx = conn.unchecked_transaction()?;
        let mut stmt = tx.prepare("SELECT path FROM game_files")?;
        let all = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let dead: Vec<_> = all
            .into_iter()
            .filter(|path| {
                let normalized = library_path_identity(path);
                if keep.contains(&normalized) {
                    return false;
                }
                roots
                    .iter()
                    .any(|root| normalized == *root || normalized.starts_with(&format!("{root}\\")))
            })
            .collect();
        for p in &dead {
            tx.execute("DELETE FROM file_contents WHERE path=?1", [p])?;
            tx.execute("DELETE FROM game_files WHERE path=?1", [p])?;
        }
        tx.commit()?;
        Ok(dead.len())
    }
    pub fn file_contents(&self, path: &str) -> Result<Vec<crate::scanner::Content>> {
        let conn = self.connect()?;
        let mut stmt=conn.prepare("SELECT title_id,COALESCE(version,0),COALESCE(content_type,'unknown'),COALESCE(source,'none') FROM file_contents WHERE path=?1")?;
        let rows = stmt.query_map([path], |r| {
            Ok(crate::scanner::Content {
                title_id: r.get(0)?,
                version: r.get(1)?,
                content_type: r.get(2)?,
                source: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn known_files(&self) -> Result<HashMap<String, KnownFile>> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare("SELECT path,mtime,COALESCE(size,0),COALESCE(fast_hash,'') FROM game_files")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        let mut out = HashMap::new();
        for r in rows {
            let (p, m, s, h) = r?;
            out.insert(p, (m, s, h));
        }
        Ok(out)
    }
    pub fn cached_file_records(&self) -> Result<HashMap<String, FileRecord>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare("SELECT path,filename,COALESCE(size,0),mtime,COALESCE(file_type,''),COALESCE(scan_folder,''),COALESCE(fast_hash,''),COALESCE(nacp_ko,''),COALESCE(nacp_en,''),COALESCE(nacp_ja,'') FROM game_files")?;
        let mut records = HashMap::new();
        for row in stmt.query_map([], |r| {
            let path: String = r.get(0)?;
            Ok((
                path.clone(),
                FileRecord {
                    path,
                    filename: r.get(1)?,
                    size: r.get(2)?,
                    mtime: r.get(3)?,
                    file_type: r.get(4)?,
                    scan_folder: r.get(5)?,
                    fast_hash: r.get(6)?,
                    nacp_ko: r.get(7)?,
                    nacp_en: r.get(8)?,
                    nacp_ja: r.get(9)?,

                    contents: Vec::new(),
                },
            ))
        })? {
            let (path, record) = row?;
            records.insert(path, record);
        }
        let mut stmt = conn.prepare("SELECT path,title_id,COALESCE(version,0),COALESCE(content_type,'unknown'),COALESCE(source,'none') FROM file_contents")?;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                crate::scanner::Content {
                    title_id: r.get(1)?,
                    version: r.get(2)?,
                    content_type: r.get(3)?,
                    source: r.get(4)?,
                },
            ))
        })? {
            let (path, content) = row?;
            if let Some(record) = records.get_mut(&path) {
                record.contents.push(content);
            }
        }
        Ok(records)
    }
    pub fn rename_file_path(&self, old: &str, new: &str) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let found: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM game_files WHERE path=?1)",
            [new],
            |r| r.get(0),
        )?;
        if found {
            anyhow::bail!("새 파일명이 라이브러리에 이미 있습니다")
        }
        let name = Path::new(new)
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or(new);
        if tx.execute(
            "UPDATE game_files SET path=?1,filename=?2 WHERE path=?3",
            params![new, name, old],
        )? != 1
        {
            anyhow::bail!("기존 파일의 라이브러리 기록을 찾지 못했습니다")
        }
        tx.execute(
            "UPDATE file_contents SET path=?1 WHERE path=?2",
            params![new, old],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_sqlite_backup_is_consistent_and_refuses_to_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original.db");
        let copy = temp.path().join("snapshot.db");
        let db = Connection::open(&original).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE protected(value TEXT); INSERT INTO protected(value) VALUES('user data');").unwrap();
        backup_read_only(&original, &copy).unwrap();
        let copied = Connection::open(&copy).unwrap();
        let value: String = copied
            .query_row("SELECT value FROM protected", [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, "user data");
        assert!(backup_read_only(&original, &copy).is_err());
        let still_original: String = db
            .query_row("SELECT value FROM protected", [], |r| r.get(0))
            .unwrap();
        assert_eq!(still_original, "user data");
    }

    fn content(title_id: &str, version: i64, kind: &str) -> crate::scanner::Content {
        crate::scanner::Content {
            title_id: Some(title_id.into()),
            version,
            content_type: kind.into(),
            source: "test_fixture".into(),
        }
    }

    fn file_record(
        path: &str,
        filename: &str,
        contents: Vec<crate::scanner::Content>,
    ) -> FileRecord {
        FileRecord {
            path: path.into(),
            filename: filename.into(),
            size: 1234,
            mtime: Some(1_700_000_000.0),
            file_type: "nsp".into(),
            scan_folder: "C:\\games".into(),
            fast_hash: "fixture-hash".into(),
            nacp_ko: "한국어 제목".into(),
            nacp_en: "English title".into(),
            nacp_ja: "日本語タイトル".into(),

            contents,
        }
    }

    fn title(title_id: &str, name: &str, version: i64) -> crate::titledb::Entry {
        crate::titledb::Entry {
            title_id: title_id.into(),
            name: name.into(),
            version,
            region: "US".into(),
            ..Default::default()
        }
    }

    #[test]
    fn catalog_metadata_reads_only_requested_base_ids_across_sqlite_batches() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("library.db")).unwrap();
        db.init().unwrap();
        let mut first = title("0100D2C0115B6000", "타이틀 DB 이름", 131072);
        first.description = "게임 설명\n두 번째 줄".into();
        first.release_date = "20261119".into();
        first.num_players = "1-4".into();
        first.rating = "E".into();
        first.languages = "Korean, English".into();
        first.screenshots = "[\"https://example.org/one.jpg\"]".into();
        db.load_titles(&[
            first,
            title("0100D2C0115B7001", "DLC", 0),
            title("0100ABCDEF012000", "Another game", 0),
        ])
        .unwrap();
        let mut requested = HashSet::new();
        requested.insert("0100D2C0115B6000".into());
        for i in 0..900 {
            requested.insert(format!("A00000000{i:07X}"));
        }
        // Dynamic IDs are always passed as parameters, never as SQL text.
        requested.insert("0100' OR 1=1 --".into());
        let result = db.catalog_title_details(&requested).unwrap();
        assert_eq!(result.len(), 1);
        let details = &result["0100D2C0115B6000"];
        assert_eq!(details.name, "타이틀 DB 이름");
        assert_eq!(details.version, 131072);
        assert_eq!(details.release_date, "20261119");
        assert_eq!(details.num_players, "1-4");
        assert_eq!(details.rating, "E");
        // Removing Store descriptions must not change the original title DB.
        let source_description: String = db
            .connect()
            .unwrap()
            .query_row(
                "SELECT description FROM title_db WHERE title_id=?1",
                ["0100D2C0115B6000"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_description, "게임 설명\n두 번째 줄");
        assert!(details.screenshots.contains("one.jpg"));
        assert_eq!(details.languages, "Korean, English");
        assert_eq!(db.stats().unwrap().title_db_count, 3);
    }

    #[test]
    fn upsert_groups_base_update_and_owned_missing_dlcs() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("library.db")).unwrap();
        db.init().unwrap();
        db.load_titles(&[
            title("0100D2C0115B6000", "Sample Game", 131072),
            title("0100D2C0115B7001", "Owned DLC", 0),
            title("0100D2C0115B7002", "Missing DLC", 0),
        ])
        .unwrap();
        db.upsert_files(&[
            file_record(
                "C:\\games\\base.nsp",
                "base.nsp",
                vec![
                    content("0100D2C0115B6000", 0, "base"),
                    content("0100D2C0115B7001", 0, "dlc"),
                ],
            ),
            file_record(
                "C:\\games\\update.nsp",
                "update.nsp",
                vec![content("0100D2C0115B6800", 65536, "update")],
            ),
        ])
        .unwrap();

        let files = db.files("ko_en_db").unwrap();
        let base = files.iter().find(|f| f.path.ends_with("base.nsp")).unwrap();
        let update = files
            .iter()
            .find(|f| f.path.ends_with("update.nsp"))
            .unwrap();
        assert!(base.has_base);
        assert!(base.has_update);
        assert!(!update.has_base);
        assert!(update.has_update);
        assert_eq!(update.update_version, 65536);
        assert_eq!(base.title_name, "한국어 제목");
        assert_eq!(base.latest_version, 131072);
        assert_eq!(base.dlc_count, 1);
        assert_eq!(base.db_dlc_count, 2);
        assert_eq!(
            base.owned_dlcs,
            vec![("0100D2C0115B7001".into(), "Owned DLC".into())]
        );
        assert_eq!(
            base.missing_dlcs,
            vec![("0100D2C0115B7002".into(), "Missing DLC".into())]
        );

        let ids = db.content_title_ids().unwrap();
        assert!(ids.contains("0100D2C0115B6000"));
        assert!(ids.contains("0100D2C0115B6800"));
        assert_eq!(db.cached_file_records().unwrap().len(), 2);
    }

    #[test]
    fn stage41_library_requires_decoded_cnmt_for_automatic_merge_eligibility() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("library.db")).unwrap();
        db.init().unwrap();
        let base_id = "0100D2C0115B6000";
        db.load_titles(&[title(base_id, "Catalog Game", 196608)])
            .unwrap();
        for (source, should_verify) in [
            ("filename", false),
            ("derived_base", false),
            ("header_ticket", false),
            ("header_xml", false),
            ("none", false),
            ("header_cnmt", true),
            ("header_native", true),
            ("header_native_card", true),
        ] {
            let mut record = file_record(
                &format!("C:/Games/{source}.xci"),
                &format!("Spoofed [{base_id}][v999999].xci"),
                vec![content(base_id, 0, "base")],
            );
            record.contents[0].source = source.into();
            db.upsert_files(&[record]).unwrap();
            let rows = db.files("db").unwrap();
            let row = rows
                .iter()
                .find(|item| item.path.ends_with(&format!("{source}.xci")))
                .unwrap();
            assert!(
                row.has_base,
                "untrusted records may still be displayed: {source}"
            );
            assert_eq!(
                row.verified_base, should_verify,
                "provenance source={source}"
            );
        }
    }

    #[test]
    fn upsert_rename_and_prune_keep_file_content_links_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("library.db")).unwrap();
        db.init().unwrap();
        let original = "C:\\games\\old.nsp";
        let renamed = "C:\\games\\new.nsp";
        db.upsert_files(&[file_record(
            original,
            "old.nsp",
            vec![content("0100D2C0115B6000", 0, "base")],
        )])
        .unwrap();

        db.rename_file_path(original, renamed).unwrap();
        assert!(db
            .file_contents(renamed)
            .unwrap()
            .iter()
            .any(|c| c.title_id.as_deref() == Some("0100D2C0115B6000")));
        assert!(db.known_files().unwrap().contains_key(renamed));
        assert_eq!(
            db.delete_missing_files_within(&[renamed.into()], &["C:\\games".into()])
                .unwrap(),
            0
        );
        assert_eq!(db.delete_missing_files_within(&[], &[]).unwrap(), 0);
        assert_eq!(
            db.delete_missing_files_within(&["C:\\games\\kept.nsp".into()], &["C:\\games".into()])
                .unwrap(),
            1
        );
        assert!(db.file_contents(renamed).unwrap().is_empty());
        assert!(!db.known_files().unwrap().contains_key(renamed));
    }

    #[test]
    fn case_and_slash_equivalent_paths_do_not_prune_or_duplicate_game_records() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("library.db")).unwrap();
        db.init().unwrap();
        let original = "e:\\Games\\test.nsp";
        db.upsert_files(&[file_record(
            original,
            "test.nsp",
            vec![content("0100D2C0115B6000", 0, "base")],
        )])
        .unwrap();
        let removed = db
            .delete_missing_files_within(&["E:/GAMES/TEST.NSP".into()], &["E:\\Games".into()])
            .unwrap();
        assert_eq!(removed, 0);
        assert_eq!(db.stats().unwrap().file_count, 1);
        let replacement = "E:\\Games\\test.nsp";
        db.upsert_files(&[file_record(
            replacement,
            "test.nsp",
            vec![content("0100D2C0115B6000", 65536, "base")],
        )])
        .unwrap();
        assert_eq!(db.stats().unwrap().file_count, 1);
        assert!(!db.known_files().unwrap().contains_key(original));
        assert!(db.known_files().unwrap().contains_key(replacement));
        assert_eq!(db.file_contents(replacement).unwrap()[0].version, 65536);
    }

    #[test]
    fn scoped_prune_keeps_unreachable_folder_records() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("library.db")).unwrap();
        db.init().unwrap();
        db.upsert_files(&[
            file_record(
                "E:\\games\\a.nsp",
                "a.nsp",
                vec![content("0100D2C0115B6000", 0, "base")],
            ),
            file_record(
                "F:\\archive\\b.nsp",
                "b.nsp",
                vec![content("0100D2C0115B6800", 65536, "update")],
            ),
        ])
        .unwrap();

        // Only E:\ was reachable this pass: its vanished files are pruned, while
        // every F:\ record must survive because that drive was not scanned.
        let removed = db
            .delete_missing_files_within(&[], &["E:\\games".into()])
            .unwrap();
        assert_eq!(removed, 1);
        let remaining = db.known_files().unwrap();
        assert!(!remaining.contains_key("E:\\games\\a.nsp"));
        assert!(remaining.contains_key("F:\\archive\\b.nsp"));
        assert_eq!(db.delete_missing_files_within(&[], &[]).unwrap(), 0);
        assert!(db.known_files().unwrap().contains_key("F:\\archive\\b.nsp"));
    }

    #[test]
    fn prepared_batch_upsert_preserves_empty_content_and_single_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("library.db")).unwrap();
        db.init().unwrap();
        let empty = file_record("C:\\games\\empty.nsp", "empty.nsp", Vec::new());
        let multiple = file_record(
            "C:\\games\\multi.nsp",
            "multi.nsp",
            vec![
                content("0100D2C0115B6000", 0, "base"),
                content("0100D2C0115B6800", 131072, "update"),
            ],
        );
        db.upsert_files(&[empty, multiple.clone()]).unwrap();
        let rows = db.cached_file_records().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows["C:\\games\\empty.nsp"].contents.len(), 1);
        assert!(rows["C:\\games\\empty.nsp"].contents[0].title_id.is_none());
        assert_eq!(
            rows["C:\\games\\empty.nsp"].contents[0].content_type,
            "unknown"
        );
        assert_eq!(rows["C:\\games\\multi.nsp"].contents.len(), 2);
        let conn = db.connect().unwrap();
        let distinct_timestamps: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT last_seen) FROM game_files",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(distinct_timestamps, 1);
        db.upsert_files(&[multiple]).unwrap();
        assert_eq!(
            db.cached_file_records().unwrap()["C:\\games\\multi.nsp"]
                .contents
                .len(),
            2
        );
    }

    #[test]
    fn config_round_trips_known_and_extra_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert!(load_config(&path).unwrap().scan_folders.is_empty());
        let config: Config = serde_json::from_value(serde_json::json!({
            "settings": {"theme": "dark"},
            "scan_folders": ["C:\\games"],
            "future_setting": true
        }))
        .unwrap();

        save_config(&path, &config).unwrap();
        let restored = load_config(&path).unwrap();
        assert_eq!(restored.scan_folders, vec!["C:\\games"]);
        assert_eq!(restored.settings.get("theme").unwrap(), "dark");
        assert_eq!(restored.extra.get("future_setting").unwrap(), true);
    }

    #[test]
    fn broken_config_recovers_backup_without_resetting_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = Config::default();
        config.settings.insert("theme".into(), "dark".into());
        save_config(&path, &config).unwrap();
        save_config(&path, &config).unwrap();
        std::fs::write(&path, b"{truncated").unwrap();
        assert_eq!(
            load_config(&path).unwrap().settings.get("theme").unwrap(),
            "dark"
        );
        std::fs::write(path.with_extension("json.bak"), b"invalid too").unwrap();
        assert!(load_config(&path).is_err());
    }

    #[test]
    fn title_db_import_failure_rolls_back_all_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("library.db")).unwrap();
        db.init().unwrap();
        db.load_titles(&[crate::titledb::Entry {
            title_id: "ORIGINAL".into(),
            name: "Kept".into(),
            ..Default::default()
        }])
        .unwrap();
        db.connect()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER prevent_bad BEFORE INSERT ON title_db
             WHEN NEW.title_id = 'BLOCKED' BEGIN SELECT RAISE(ABORT, 'simulated failure'); END;",
            )
            .unwrap();
        let updated = vec![
            crate::titledb::Entry {
                title_id: "NEW".into(),
                ..Default::default()
            },
            crate::titledb::Entry {
                title_id: "BLOCKED".into(),
                ..Default::default()
            },
        ];
        assert!(db.load_titles(&updated).is_err());
        let conn = db.connect().unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM title_db", [], |r| r.get(0))
            .unwrap();
        let original: String = conn
            .query_row(
                "SELECT name FROM title_db WHERE title_id='ORIGINAL'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(original, "Kept");
    }
}
