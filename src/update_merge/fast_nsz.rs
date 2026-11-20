//! Stream an existing NSZ's unchanged NCZ payloads into a merged NSZ.
//!
//! The ordinary NSRP route inflates the entire base NSZ to a temporary NSP,
//! extracts each NCA again, builds an intermediate NSP and then recompresses
//! it. This path keeps the original compressed NCA bytes and only compresses
//! the (usually much smaller) update NSP. Both CNMTs are still decoded with
//! the native metadata reader and the final package is verified by the caller.

use super::{compression_options, validate_selected_cnmts};
use anyhow::{ensure, Context, Result};
use nsrp_core::{
    formats::pfs0::{build_from_sources, Pfs0Entry, Pfs0Index, Pfs0Source},
    keys::KeySet,
    ops::{
        cnmt::{self, CnmtInfo, ContentMetaType},
        compress,
    },
};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone)]
struct Candidate {
    source: usize,
    cnmt: CnmtInfo,
}

struct IndexedSource {
    path: PathBuf,
    index: Pfs0Index,
}

impl IndexedSource {
    fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            index: Pfs0Index::parse(path)?,
        })
    }

    fn exact(&self, name: &str) -> Option<&Pfs0Entry> {
        self.index.entry(name)
    }

    fn content(&self, name: &str) -> Option<&Pfs0Entry> {
        self.exact(name).or_else(|| {
            let stem = name.strip_suffix(".nca")?;
            self.exact(&format!("{stem}.ncz"))
        })
    }

    fn region(&self, entry: &Pfs0Entry) -> Result<Pfs0Source> {
        let offset = self
            .index
            .data_start
            .checked_add(entry.offset)
            .context("NSZ 원본 엔트리 오프셋 오버플로")?;
        Ok(Pfs0Source::Region {
            container: self.path.clone(),
            offset,
            size: entry.size,
        })
    }
}

fn latest_candidates(
    base_id: &str,
    version: i64,
    base_cnmts: &[CnmtInfo],
    patch_cnmts: &[CnmtInfo],
) -> Result<Option<Vec<Candidate>>> {
    let mut by_title = BTreeMap::<String, Candidate>::new();
    for (source, cnmts) in [(0, base_cnmts), (1, patch_cnmts)] {
        for cnmt in cnmts {
            if !cnmt::base_title_id(&cnmt.title_id)?.eq_ignore_ascii_case(base_id) {
                continue;
            }
            if !matches!(
                cnmt.meta_type,
                ContentMetaType::Base | ContentMetaType::Update | ContentMetaType::Dlc
            ) {
                continue;
            }
            match by_title.get(&cnmt.title_id) {
                Some(previous) if previous.cnmt.version >= cnmt.version => {}
                _ => {
                    by_title.insert(
                        cnmt.title_id.clone(),
                        Candidate {
                            source,
                            cnmt: cnmt.clone(),
                        },
                    );
                }
            }
        }
    }
    let mut selected: Vec<_> = by_title.into_values().collect();
    if selected
        .iter()
        .map(|item| item.source)
        .collect::<HashSet<_>>()
        .len()
        != 2
    {
        return Ok(None);
    }
    let cnmts = selected
        .iter()
        .map(|item| item.cnmt.clone())
        .collect::<Vec<_>>();
    validate_selected_cnmts(&cnmts, base_id, version)?;
    selected.sort_by_key(|item| {
        (
            match item.cnmt.meta_type {
                ContentMetaType::Base => 0,
                ContentMetaType::Update => 1,
                ContentMetaType::Dlc => 2,
                ContentMetaType::Other(_) => 3,
            },
            item.cnmt.title_id.clone(),
        )
    });
    Ok(Some(selected))
}

/// Add an NCA under its *physical* name (.ncz when already compressed), but
/// deduplicate by its logical NCA ID so the same content cannot be inserted
/// once as .nca and once as .ncz.
fn add_entry(
    files: &mut Vec<(String, Pfs0Source)>,
    seen: &mut HashSet<String>,
    wanted: &str,
    sources: &[&IndexedSource],
    allow_ncz: bool,
) -> Result<bool> {
    let key = wanted.to_ascii_lowercase();
    if seen.contains(&key) {
        return Ok(true);
    }
    for source in sources {
        let entry = if allow_ncz {
            source.content(wanted)
        } else {
            source.exact(wanted)
        };
        if let Some(entry) = entry {
            files.push((entry.name.clone(), source.region(entry)?));
            seen.insert(key);
            return Ok(true);
        }
    }
    Ok(false)
}

fn compose_nsz(
    candidates: &[Candidate],
    base: &IndexedSource,
    patch_raw: &IndexedSource,
    patch_compressed: &IndexedSource,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<usize> {
    let metadata = [base, patch_raw];
    let content = [base, patch_compressed];
    let mut files = Vec::<(String, Pfs0Source)>::new();
    let mut seen = HashSet::new();
    let mut ncz_count = 0usize;
    for item in candidates {
        ensure!(
            !cancel.load(Ordering::Relaxed),
            "병합 작업이 중단되었습니다"
        );
        // Keep CNMT as original NCA, not a compressed .cnmt.ncz. The native
        // metadata parser and launch loaders expect a directly readable meta.
        ensure!(
            add_entry(
                &mut files,
                &mut seen,
                &item.cnmt.cnmt_nca_name,
                &[metadata[item.source]],
                false,
            )?,
            "병합 필수 CNMT 누락: {}",
            item.cnmt.cnmt_nca_name
        );
        for part in &item.cnmt.contents {
            if part.content_type == 6 {
                continue; // Optional download DeltaFragment, not an NCA.
            }
            let wanted = format!("{}.nca", part.nca_id);
            let preferred = item.source;
            let other = 1 - preferred;
            ensure!(
                add_entry(
                    &mut files,
                    &mut seen,
                    &wanted,
                    &[content[preferred], content[other]],
                    true,
                )?,
                "병합 필수 콘텐츠 누락: {wanted}"
            );
        }
        let prefix = item.cnmt.title_id.to_ascii_lowercase();
        for entry in &metadata[item.source].index.entries {
            let name = entry.name.to_ascii_lowercase();
            if name.starts_with(&prefix) && (name.ends_with(".tik") || name.ends_with(".cert")) {
                add_entry(
                    &mut files,
                    &mut seen,
                    &entry.name,
                    &[metadata[item.source]],
                    false,
                )?;
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, _) in &files {
        if name.to_ascii_lowercase().ends_with(".ncz") {
            ncz_count += 1;
        }
    }
    ensure!(ncz_count > 0, "압축된 NCA가 없는 NSZ 병합 결과");
    // PFS0 validates every source range, streams in 2MiB bounded chunks,
    // checks cancellation, and removes its .partial output on failure.
    build_from_sources(&files, output, cancel)?;
    Ok(ncz_count)
}

/// Immutable input and configuration for the NSZ fast merger. Group the
/// request here so the GUI wrapper and opt-in QA use the same contract.
pub(super) struct FastNszRequest<'a> {
    pub(super) original: &'a Path,
    pub(super) patch: &'a Path,
    pub(super) base_id: &'a str,
    pub(super) version: i64,
    pub(super) work: &'a Path,
    pub(super) disabled_tool: &'a Path,
    pub(super) prod_keys: &'a Path,
}

/// Returns None for formats/layouts that must use the existing NSRP route.
/// Any error after entering this path is a hard failure, not a silent fallback.
pub(super) fn try_merge_nsz_fast(
    request: FastNszRequest<'_>,
    keys: &mut KeySet,
    cancel: &AtomicBool,
) -> Result<Option<PathBuf>> {
    let FastNszRequest {
        original,
        patch,
        base_id,
        version,
        work,
        disabled_tool,
        prod_keys,
    } = request;
    let ext = |path: &Path| {
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
    };
    if ext(original) != "nsz" || ext(patch) != "nsp" {
        return Ok(None);
    }
    let base = IndexedSource::open(original)?;
    // A compressed CNMT requires materializing the metadata NCA first.
    // The traditional NSRP path already handles that uncommon layout.
    if !base
        .index
        .entries
        .iter()
        .any(|entry| entry.name.to_ascii_lowercase().ends_with(".ncz"))
        || base
            .index
            .entries
            .iter()
            .any(|entry| entry.name.to_ascii_lowercase().ends_with(".cnmt.ncz"))
    {
        return Ok(None);
    }
    let (_, base_cnmts, _) = cnmt::discover_game_metadata(
        original,
        &work.join("fast_meta_base"),
        disabled_tool,
        prod_keys,
        cancel,
    )
    .context("NSZ 원본 CNMT 분석 실패")?;
    let (_, patch_cnmts, _) = cnmt::discover_game_metadata(
        patch,
        &work.join("fast_meta_patch"),
        disabled_tool,
        prod_keys,
        cancel,
    )
    .context("업데이트 NSP CNMT 분석 실패")?;
    let Some(candidates) = latest_candidates(base_id, version, &base_cnmts, &patch_cnmts)? else {
        return Ok(None);
    };

    let patch_nsz = work.join("fast_patch.nsz");
    compress::compress_nsp(
        patch,
        &patch_nsz,
        keys,
        &compression_options(),
        cancel,
        &mut |_| {},
        &mut |_| {},
    )
    .context("업데이트 NSP 독립 압축 실패")?;
    let raw_patch = IndexedSource::open(patch)?;
    let compressed_patch = IndexedSource::open(&patch_nsz)?;
    let output = work.join("fast_merged.nsz");
    let count = compose_nsz(
        &candidates,
        &base,
        &raw_patch,
        &compressed_patch,
        &output,
        cancel,
    )?;
    ensure!(count > 0, "최종 NSZ에 NCZ 압축 엔트리가 없습니다");
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsrp_core::formats::pfs0::build_from_paths;

    fn fake_cnmt(source: usize, title_id: &str, nca_id: &str, version: u32) -> Candidate {
        Candidate {
            source,
            cnmt: CnmtInfo {
                cnmt_nca_name: format!("{title_id}.cnmt.nca"),
                title_id: title_id.to_owned(),
                version,
                meta_type: if source == 0 {
                    ContentMetaType::Base
                } else {
                    ContentMetaType::Update
                },
                required_system_version: 0,
                contents: vec![cnmt::CnmtContent {
                    hash: [0; 32],
                    nca_id: nca_id.to_owned(),
                    size: 42,
                    content_type: 0,
                }],
                raw: Vec::new(),
            },
        }
    }

    #[test]
    fn directly_reuses_compressed_ncz_regions_and_preserves_raw_meta() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let base_id = "0100111122220000";
        let update_id = "0100111122220800";
        let base_nca = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let update_nca = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let base_payload = temp.path().join("b.ncz");
        let update_payload = temp.path().join("u.ncz");
        let meta_base = temp.path().join("meta_base");
        let meta_patch = temp.path().join("meta_patch");
        let ticket = temp.path().join("ticket");
        std::fs::write(&base_payload, b"already-compressed-original-bytes")?;
        std::fs::write(&update_payload, b"compressed-new-update-bytes")?;
        std::fs::write(&meta_base, b"base-cnmt-remains-uncompressed")?;
        std::fs::write(&meta_patch, b"patch-cnmt-remains-uncompressed")?;
        std::fs::write(&ticket, b"original-ticket-bytes")?;
        let base_path = temp.path().join("base.nsz");
        let patch_path = temp.path().join("patch.nsp");
        let patch_nsz = temp.path().join("patch.nsz");
        build_from_paths(
            &[
                (format!("{base_nca}.ncz"), base_payload.clone()),
                (format!("{base_id}.cnmt.nca"), meta_base.clone()),
            ],
            &base_path,
        )?;
        build_from_paths(
            &[
                (format!("{update_id}.cnmt.nca"), meta_patch.clone()),
                (format!("{update_id}0000000000000000.tik"), ticket.clone()),
            ],
            &patch_path,
        )?;
        build_from_paths(
            &[(format!("{update_nca}.ncz"), update_payload.clone())],
            &patch_nsz,
        )?;
        let base = IndexedSource::open(&base_path)?;
        let patch_raw = IndexedSource::open(&patch_path)?;
        let patch_compressed = IndexedSource::open(&patch_nsz)?;
        let output = temp.path().join("merged.nsz");
        let candidates = [
            fake_cnmt(0, base_id, base_nca, 0),
            fake_cnmt(1, update_id, update_nca, 65536),
        ];
        assert_eq!(
            compose_nsz(
                &candidates,
                &base,
                &patch_raw,
                &patch_compressed,
                &output,
                &AtomicBool::new(false),
            )?,
            2
        );
        let index = Pfs0Index::parse(&output)?;
        assert_eq!(index.entries.len(), 5);
        for (name, data) in [
            (format!("{base_nca}.ncz"), std::fs::read(base_payload)?),
            (format!("{update_nca}.ncz"), std::fs::read(update_payload)?),
            (format!("{base_id}.cnmt.nca"), std::fs::read(meta_base)?),
            (format!("{update_id}.cnmt.nca"), std::fs::read(meta_patch)?),
            (
                format!("{update_id}0000000000000000.tik"),
                std::fs::read(ticket)?,
            ),
        ] {
            let entry = index
                .entry(&name)
                .with_context(|| format!("missing: {name}"))?;
            ensure!(
                index.read_entry(&output, entry)? == data,
                "payload changed: {name}"
            );
        }
        Ok(())
    }

    #[test]
    fn missing_mandatory_content_and_cancel_do_not_publish_nsz() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let base_id = "0100111122220000";
        let update_id = "0100111122220800";
        let meta_base = temp.path().join("mb");
        let meta_patch = temp.path().join("mp");
        let nca = temp.path().join("nc");
        std::fs::write(&meta_base, b"base-meta")?;
        std::fs::write(&meta_patch, b"patch-meta")?;
        std::fs::write(&nca, b"original-ncz")?;
        let base_path = temp.path().join("base.nsz");
        let patch_path = temp.path().join("patch.nsp");
        build_from_paths(
            &[
                (format!("{base_id}.cnmt.nca"), meta_base),
                ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ncz".into(), nca),
            ],
            &base_path,
        )?;
        build_from_paths(
            &[(format!("{update_id}.cnmt.nca"), meta_patch)],
            &patch_path,
        )?;
        let base = IndexedSource::open(&base_path)?;
        let patch = IndexedSource::open(&patch_path)?;
        let output = temp.path().join("cannot_publish.nsz");
        let candidates = [
            fake_cnmt(0, base_id, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 0),
            fake_cnmt(1, update_id, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 65536),
        ];
        assert!(compose_nsz(
            &candidates,
            &base,
            &patch,
            &patch,
            &output,
            &AtomicBool::new(false)
        )
        .is_err());
        ensure!(!output.exists(), "missing content published output");
        assert!(compose_nsz(
            &candidates,
            &base,
            &patch,
            &patch,
            &output,
            &AtomicBool::new(true)
        )
        .is_err());
        ensure!(!output.exists(), "cancel published output");
        Ok(())
    }
}
