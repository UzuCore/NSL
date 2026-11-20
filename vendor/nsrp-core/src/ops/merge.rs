use crate::{
    formats::{ncz_stream::decompress_ncz_region_to_nca, pfs0::build_from_paths},
    keys::KeySet,
    ops::{
        cnmt::{
            CnmtInfo, ContentMetaType, base_title_id, discover_game_metadata,
            get_title_from_control_nca, resolve_content_entry,
        },
        compress::{CompressOptions, compress_nsp, decompress_nsz},
        container::ContainerEntry,
        convert::game_info_preset_data,
        split::sanitize_filename,
        xci_output::{build_xci_verified, publish_verified_xci},
        xcz::{XczOptions, compress_xci, decompress_xcz},
    },
    process::run_tool,
};
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeFormat {
    Nsp,
    Xci,
}

#[derive(Debug, Clone)]
pub struct MergeOptions {
    pub output_format: MergeFormat,
    pub compress_after: bool,
    pub use_game_title: bool,
    pub decompression_threads: usize,
    pub compression: CompressOptions,
}

#[derive(Debug, Clone)]
struct SourceInfo {
    original: PathBuf,
    path: PathBuf,
    entries: Vec<ContainerEntry>,
    cnmts: Vec<CnmtInfo>,
}

#[derive(Debug, Clone)]
struct Candidate {
    source: usize,
    cnmt: CnmtInfo,
}

pub fn merge_packages(
    inputs: &[PathBuf],
    output_dir: &Path,
    output_dir_explicit: bool,
    resource_dir: &Path,
    hactool: &Path,
    prod_keys: &Path,
    keys: &mut KeySet,
    options: &MergeOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
    log: &mut dyn FnMut(String),
) -> Result<Vec<PathBuf>> {
    if inputs.len() < 2 {
        bail!("병합에는 최소 2개 입력 파일이 필요합니다");
    }
    fs::create_dir_all(output_dir)?;
    let temp = crate::workspace::temp_workspace("nsrp_merge_")?;
    let prepared_inputs = prepare_merge_inputs(
        inputs,
        temp.path(),
        options.decompression_threads,
        cancel,
        log,
    )?;
    let mut sources = Vec::new();
    for (index, (original, path)) in prepared_inputs.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("작업이 중단되었습니다");
        }
        // Merge always preserves the decompressed NCA's source header. A
        // ticketless source can boot even when restoring its canonical Rights
        // ID from CNMT makes it fail at runtime (whether or not a title key
        // happens to be present). The archival repair policy is not suitable
        // for an implicit launchability-preserving game update operation.
        if original
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("nsz") || ext.eq_ignore_ascii_case("xcz"))
        {
            log("병합 원본의 NCA 헤더를 그대로 보존합니다 (Rights ID 강제 복원 없음)".to_owned());
        }
        let (entries, cnmts, legacy_partition_fallback) = discover_game_metadata(
            path,
            &temp.path().join(format!("metadata_{index}")),
            hactool,
            prod_keys,
            cancel,
        )
        .with_context(|| format!("컨테이너 분석 실패: {}", path.display()))?;
        if legacy_partition_fallback {
            log(format!(
                "구형 XCI 파티션 배치 감지: {} — Update/DLC 메타데이터를 추가 탐색했습니다",
                original
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("input")
            ));
        }
        sources.push(SourceInfo {
            original: original.clone(),
            path: path.clone(),
            entries,
            cnmts,
        });
        progress(0.20 * (index + 1) as f32 / inputs.len() as f32);
    }

    let mut groups: BTreeMap<String, BTreeMap<String, Candidate>> = BTreeMap::new();
    for (source_index, source) in sources.iter().enumerate() {
        for cnmt in &source.cnmts {
            let base = base_title_id(&cnmt.title_id)?;
            let by_title = groups.entry(base).or_default();
            match by_title.get(&cnmt.title_id) {
                Some(old) if old.cnmt.version >= cnmt.version => {}
                _ => {
                    by_title.insert(
                        cnmt.title_id.clone(),
                        Candidate {
                            source: source_index,
                            cnmt: cnmt.clone(),
                        },
                    );
                }
            }
        }
    }
    let groups: Vec<(String, Vec<Candidate>)> = groups
        .into_iter()
        .filter_map(|(base, map)| {
            let values: Vec<_> = map.into_values().collect();
            let has_base = values
                .iter()
                .any(|c| c.cnmt.meta_type == ContentMetaType::Base);
            if has_base && has_multiple_sources(&values) {
                Some((base, values))
            } else {
                None
            }
        })
        .collect();
    if groups.is_empty() {
        bail!("본편과 업데이트/DLC가 함께 있는 병합 그룹을 찾지 못했습니다");
    }

    let mut outputs = Vec::new();
    let group_count = groups.len();
    for (group_index, (base_id, mut candidates)) in groups.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("작업이 중단되었습니다");
        }
        candidates.sort_by_key(|c| (meta_order(c.cnmt.meta_type), c.cnmt.title_id.clone()));
        let group_dir = temp.path().join(format!("group_{group_index}"));
        fs::create_dir_all(&group_dir)?;

        let group_output_dir = if output_dir_explicit {
            output_dir.to_path_buf()
        } else if let Some(base) = candidates
            .iter()
            .find(|candidate| candidate.cnmt.meta_type == ContentMetaType::Base)
        {
            sources[base.source]
                .original
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        } else {
            output_dir.to_path_buf()
        };
        fs::create_dir_all(&group_output_dir)?;

        let mut game_title = None;
        if options.use_game_title
            && let Some(base) = candidates
                .iter()
                .find(|c| c.cnmt.meta_type == ContentMetaType::Base)
            && let Some(control_name) = base.cnmt.control_nca_name()
        {
            let source = &sources[base.source];
            if let Some(entry) = resolve_content_entry(&source.entries, &control_name) {
                let control = group_dir.join("control.nca");
                materialize_entry(&source.path, entry, &control, cancel)?;
                game_title = get_title_from_control_nca(
                    &control,
                    hactool,
                    prod_keys,
                    &group_dir.join("title"),
                    cancel,
                )
                .unwrap_or(None);
            }
        }
        let fallback = candidates
            .iter()
            .find(|candidate| candidate.cnmt.meta_type == ContentMetaType::Base)
            .or_else(|| candidates.first())
            .and_then(|candidate| sources.get(candidate.source))
            .and_then(|source| source.original.file_stem().and_then(|stem| stem.to_str()))
            .map(strip_bracket_tags)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Merged".to_owned());
        let game_title = sanitize_filename(game_title.as_deref().unwrap_or(&fallback));

        let mut files = Vec::<(String, PathBuf)>::new();
        let mut names = HashSet::<String>::new();
        for (candidate_index, candidate) in candidates.iter().enumerate() {
            let source = &sources[candidate.source];
            add_named_entry(
                &source.path,
                &source.entries,
                &candidate.cnmt.cnmt_nca_name,
                &group_dir,
                &mut files,
                &mut names,
                cancel,
            )?;
            for content in &candidate.cnmt.contents {
                // Type 6 is an optional download delta fragment and is not
                // required to install or boot a materialized Update NCA set.
                // Repacked NSPs often omit it even though CNMT lists it.
                if content.content_type == 6 {
                    continue;
                }
                let wanted = format!("{}.nca", content.nca_id);
                if !try_add_content(
                    &wanted,
                    candidate.source,
                    &sources,
                    &group_dir,
                    &mut files,
                    &mut names,
                    cancel,
                )? {
                    bail!("병합 필수 콘텐츠 누락: {wanted}");
                }
            }
            let prefix = candidate.cnmt.title_id[..16.min(candidate.cnmt.title_id.len())]
                .to_ascii_lowercase();
            for entry in source.entries.iter().filter(|e| {
                let n = e.name.to_ascii_lowercase();
                (n.ends_with(".tik") || n.ends_with(".cert")) && n.starts_with(&prefix)
            }) {
                let key = entry.name.to_ascii_lowercase();
                if names.insert(key) {
                    let path = group_dir.join(format!("extra_{candidate_index}_{}", files.len()));
                    materialize_raw(&source.path, entry, &path, cancel)?;
                    files.push((entry.name.clone(), path));
                }
            }
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let tag = merge_content_tag(&candidates);
        let base_name = match tag {
            Some(tag) => sanitize_filename(&format!("[Merged] {game_title} {tag}")),
            None => sanitize_filename(&format!("[Merged] {game_title}")),
        };
        let intermediate_nsp = group_dir.join(format!("{base_name}.nsp"));
        build_from_paths(&files, &intermediate_nsp)?;
        log(format!(
            "병합 패키지 구성 완료: {base_id} ({}개 엔트리)",
            files.len()
        ));

        let intermediate = match options.output_format {
            MergeFormat::Nsp => intermediate_nsp,
            MergeFormat::Xci => {
                let xci = group_dir.join(format!("{base_name}.xci"));
                build_merged_xci(
                    &xci,
                    &candidates,
                    &files,
                    resource_dir,
                    prod_keys,
                    cancel,
                    log,
                )?;
                xci
            }
        };
        let final_path = if options.compress_after {
            match options.output_format {
                MergeFormat::Nsp => {
                    let path = group_output_dir.join(format!("{base_name}.nsz"));
                    compress_nsp(
                        &intermediate,
                        &path,
                        keys,
                        &options.compression,
                        cancel,
                        &mut |_| {},
                        log,
                    )?;
                    path
                }
                MergeFormat::Xci => {
                    let path = group_output_dir.join(format!("{base_name}.xcz"));
                    compress_xci(
                        &intermediate,
                        &path,
                        keys,
                        &XczOptions {
                            level: options.compression.level,
                            threads: options.compression.threads,
                            block_mode: options.compression.block_mode,
                            long_mode: options.compression.long_mode,
                        },
                        cancel,
                        &mut |_| {},
                        log,
                    )?;
                    path
                }
            }
        } else {
            let ext = match options.output_format {
                MergeFormat::Nsp => "nsp",
                MergeFormat::Xci => "xci",
            };
            let path = group_output_dir.join(format!("{base_name}.{ext}"));
            if matches!(options.output_format, MergeFormat::Xci) {
                publish_verified_xci(&intermediate, &path, cancel)?;
            } else {
                if path.exists() {
                    fs::remove_file(&path)?;
                }
                move_file(&intermediate, &path)?;
            }
            path
        };
        outputs.push(final_path);
        progress(0.20 + 0.80 * (group_index + 1) as f32 / group_count as f32);
    }
    progress(1.0);
    Ok(outputs)
}

fn prepare_merge_inputs(
    inputs: &[PathBuf],
    temp_root: &Path,
    threads: usize,
    cancel: &AtomicBool,
    log: &mut dyn FnMut(String),
) -> Result<Vec<(PathBuf, PathBuf)>> {
    let compressed_count = inputs
        .iter()
        .filter(|path| {
            path.extension()
                .and_then(|value| value.to_str())
                .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "nsz" | "xcz"))
        })
        .count();
    if compressed_count == 0 {
        return Ok(inputs
            .iter()
            .cloned()
            .map(|path| (path.clone(), path))
            .collect());
    }

    let worker_count = threads.max(1).min(inputs.len().max(1));
    log(format!(
        "압축 파일 사전 해제: {compressed_count}개 (스레드: {worker_count})"
    ));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_count)
        .build()
        .context("병합 압축 해제 스레드 풀 생성 실패")?;

    let results = pool.install(|| {
        inputs
            .par_iter()
            .enumerate()
            .map(|(index, input)| -> Result<(PathBuf, PathBuf)> {
                if cancel.load(Ordering::Relaxed) {
                    bail!("작업이 중단되었습니다");
                }
                let ext = input
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match ext.as_str() {
                    "nsz" => {
                        let output = temp_root.join(format!("prepared_{index}.nsp"));
                        decompress_nsz(input, &output, cancel, &mut |_| {}, &mut |_| {})?;
                        Ok((input.clone(), output))
                    }
                    "xcz" => {
                        let output = temp_root.join(format!("prepared_{index}.xci"));
                        decompress_xcz(input, &output, cancel, &mut |_| {}, &mut |_| {})?;
                        Ok((input.clone(), output))
                    }
                    _ => Ok((input.clone(), input.clone())),
                }
            })
            .collect::<Vec<_>>()
    });

    let mut prepared = Vec::with_capacity(results.len());
    for result in results {
        let (original, path) = result?;
        if original != path {
            log(format!(
                "압축 해제 완료: {} → {}",
                original
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("input"),
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("prepared")
            ));
        }
        prepared.push((original, path));
    }
    Ok(prepared)
}

fn build_merged_xci(
    output: &Path,
    candidates: &[Candidate],
    files: &[(String, PathBuf)],
    resource_dir: &Path,
    prod_keys: &Path,
    cancel: &AtomicBool,
    log: &mut dyn FnMut(String),
) -> Result<()> {
    let temp = crate::workspace::temp_workspace("nsrp_merge_xci_")?;
    let work_root = temp.path().join("work");
    for partition in ["secure", "update", "normal"] {
        fs::create_dir_all(work_root.join(partition))?;
    }

    let file_map = files
        .iter()
        .map(|(name, path)| (name.to_ascii_lowercase(), (name, path)))
        .collect::<BTreeMap<_, _>>();
    let mut placed = HashSet::<String>::new();
    for candidate in candidates {
        let partition = partition_for_meta(candidate.cnmt.meta_type);
        let destination = work_root.join(partition);
        let mut required = candidate.cnmt.required_nca_names();
        let tid_prefix =
            candidate.cnmt.title_id[..16.min(candidate.cnmt.title_id.len())].to_ascii_lowercase();
        required.extend(files.iter().filter_map(|(name, _)| {
            let lower = name.to_ascii_lowercase();
            ((lower.ends_with(".tik") || lower.ends_with(".cert"))
                && lower.starts_with(&tid_prefix))
            .then(|| name.clone())
        }));

        for wanted in required {
            let lower = wanted.to_ascii_lowercase();
            let Some((actual_name, source)) = file_map.get(&lower) else {
                bail!("XCI 파티션 구성에 필요한 파일이 없습니다: {wanted}");
            };
            let placement_key = format!("{partition}|{lower}");
            if !placed.insert(placement_key) {
                continue;
            }
            let target = destination.join(actual_name);
            if fs::hard_link(source, &target).is_err() {
                fs::copy(source, &target)?;
            }
        }
    }

    if fs::read_dir(work_root.join("secure"))?.next().is_none() {
        bail!("XCI secure 파티션이 비었습니다");
    }

    let preset_data = game_info_preset_data(resource_dir)?;
    fs::write(work_root.join("game_info.ini"), &preset_data)?;

    let tool_dir = temp.path().join("tools");
    fs::create_dir_all(&tool_dir)?;
    let hacbuild_src = resource_dir.join("bin").join("hacbuild.exe");
    let hacbuild = tool_dir.join("hacbuild.exe");
    fs::copy(&hacbuild_src, &hacbuild).context("hacbuild.exe 복사 실패")?;
    fs::write(tool_dir.join("game_info_preset.ini"), &preset_data)?;
    fs::copy(prod_keys, tool_dir.join("keys.txt"))?;

    let expected_ncas = files
        .iter()
        .filter(|(name, _)| name.to_ascii_lowercase().ends_with(".nca"))
        .map(|(name, path)| Ok((name.clone(), fs::metadata(path)?.len())))
        .collect::<Result<Vec<_>>>()?;
    build_xci_verified(output, &expected_ncas, cancel, |staged| {
        let args = vec![
            OsString::from("xci_auto"),
            work_root.as_os_str().to_owned(),
            staged.as_os_str().to_owned(),
        ];
        let tool = run_tool(&hacbuild, args, Some(&tool_dir), cancel)?;
        if !tool.success() || !staged.is_file() {
            bail!("XCI 빌드 실패: {}", tool.combined());
        }
        Ok(())
    })?;
    log(format!("XCI 생성 완료: {}", output.display()));
    Ok(())
}

fn strip_bracket_tags(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut depth = 0usize;
    for ch in value.chars() {
        match ch {
            '[' => depth += 1,
            ']' if depth > 0 => depth -= 1,
            _ if depth == 0 => output.push(ch),
            _ => {}
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn merge_content_tag(candidates: &[Candidate]) -> Option<String> {
    let has_base = candidates
        .iter()
        .any(|candidate| candidate.cnmt.meta_type == ContentMetaType::Base);
    let update_version = candidates
        .iter()
        .filter(|candidate| candidate.cnmt.meta_type == ContentMetaType::Update)
        .map(|candidate| candidate.cnmt.version)
        .max();
    let dlc_count = candidates
        .iter()
        .filter(|candidate| candidate.cnmt.meta_type == ContentMetaType::Dlc)
        .count();
    let mut parts = Vec::new();
    if has_base {
        parts.push("B".to_owned());
    }
    if let Some(version) = update_version {
        parts.push(format!("U{version}"));
    }
    if dlc_count > 0 {
        parts.push(format!("{dlc_count}DLC"));
    }
    (!parts.is_empty()).then(|| format!("[{}]", parts.join("+")))
}

fn partition_for_meta(meta_type: ContentMetaType) -> &'static str {
    // Game content belongs in the secure partition. XCI's update partition is
    // reserved for game-card system-update data; placing title Update/DLC there
    // makes NSRP and standard readers intentionally skip that content.
    let _ = meta_type;
    "secure"
}

fn has_multiple_sources(candidates: &[Candidate]) -> bool {
    candidates
        .iter()
        .map(|candidate| candidate.source)
        .collect::<HashSet<_>>()
        .len()
        >= 2
}

fn add_named_entry(
    source_path: &Path,
    entries: &[ContainerEntry],
    wanted: &str,
    dir: &Path,
    files: &mut Vec<(String, PathBuf)>,
    names: &mut HashSet<String>,
    cancel: &AtomicBool,
) -> Result<()> {
    let entry = entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(wanted))
        .with_context(|| format!("필수 엔트리 누락: {wanted}"))?;
    let key = wanted.to_ascii_lowercase();
    if names.insert(key) {
        let path = dir.join(format!("item_{}", files.len()));
        materialize_entry(source_path, entry, &path, cancel)?;
        files.push((wanted.to_owned(), path));
    }
    Ok(())
}

fn try_add_content(
    wanted: &str,
    preferred: usize,
    sources: &[SourceInfo],
    dir: &Path,
    files: &mut Vec<(String, PathBuf)>,
    names: &mut HashSet<String>,
    cancel: &AtomicBool,
) -> Result<bool> {
    if names.contains(&wanted.to_ascii_lowercase()) {
        return Ok(true);
    }
    let order = std::iter::once(preferred).chain((0..sources.len()).filter(|&i| i != preferred));
    for index in order {
        if let Some(entry) = resolve_content_entry(&sources[index].entries, wanted) {
            let path = dir.join(format!("item_{}", files.len()));
            materialize_entry(&sources[index].path, entry, &path, cancel)?;
            names.insert(wanted.to_ascii_lowercase());
            files.push((wanted.to_owned(), path));
            return Ok(true);
        }
    }
    Ok(false)
}

fn materialize_entry(
    input: &Path,
    entry: &ContainerEntry,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    if entry.name.to_ascii_lowercase().ends_with(".ncz") {
        let mut source = BufReader::with_capacity(8 * 1024 * 1024, File::open(input)?);
        let mut out = BufWriter::with_capacity(8 * 1024 * 1024, File::create(output)?);
        decompress_ncz_region_to_nca(
            &mut source,
            entry.offset,
            entry.size,
            &mut out,
            cancel,
            |_, _| {},
        )?;
        out.flush()?;
        Ok(())
    } else {
        materialize_raw(input, entry, output, cancel)
    }
}

fn materialize_raw(
    input: &Path,
    entry: &ContainerEntry,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut source = BufReader::with_capacity(8 * 1024 * 1024, File::open(input)?);
    source.seek(SeekFrom::Start(entry.offset))?;
    let mut out = BufWriter::with_capacity(8 * 1024 * 1024, File::create(output)?);
    let mut remaining = entry.size;
    let mut buf = vec![0u8; 8 * 1024 * 1024];
    while remaining > 0 {
        if cancel.load(Ordering::Relaxed) {
            bail!("작업이 중단되었습니다");
        }
        let take = remaining.min(buf.len() as u64) as usize;
        source.read_exact(&mut buf[..take])?;
        out.write_all(&buf[..take])?;
        remaining -= take as u64;
    }
    out.flush()?;
    Ok(())
}

fn meta_order(value: ContentMetaType) -> u8 {
    match value {
        ContentMetaType::Base => 0,
        ContentMetaType::Update => 1,
        ContentMetaType::Dlc => 2,
        ContentMetaType::Other(_) => 3,
    }
}

fn move_file(src: &Path, dst: &Path) -> Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_rename_error) => {
            fs::copy(src, dst)?;
            fs::remove_file(src)?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::pfs0::Pfs0Index;

    fn candidate(source: usize, title_id: &str, meta_type: ContentMetaType) -> Candidate {
        Candidate {
            source,
            cnmt: CnmtInfo {
                cnmt_nca_name: format!("{title_id}.cnmt.nca"),
                title_id: title_id.to_owned(),
                version: 1,
                meta_type,
                required_system_version: 0,
                contents: Vec::new(),
                raw: Vec::new(),
            },
        }
    }

    #[test]
    fn xci_game_content_stays_in_secure_partition() {
        assert_eq!(partition_for_meta(ContentMetaType::Base), "secure");
        assert_eq!(partition_for_meta(ContentMetaType::Update), "secure");
        assert_eq!(partition_for_meta(ContentMetaType::Dlc), "secure");
        assert_eq!(partition_for_meta(ContentMetaType::Other(0x90)), "secure");
    }

    #[test]
    fn already_integrated_single_source_is_not_a_merge_group() {
        let same_source = vec![
            candidate(0, "0100123456780000", ContentMetaType::Base),
            candidate(0, "0100123456780800", ContentMetaType::Update),
        ];
        assert!(!has_multiple_sources(&same_source));

        let separate_sources = vec![
            candidate(0, "0100123456780000", ContentMetaType::Base),
            candidate(1, "0100123456780800", ContentMetaType::Update),
        ];
        let multiple_sources = has_multiple_sources(&separate_sources);
        assert!(multiple_sources);

        assert_eq!(
            strip_bracket_tags("[0100123456780000] Example Game [v65536]"),
            "Example Game"
        );
        let mut update = candidate(1, "0100123456780800", ContentMetaType::Update);
        update.cnmt.version = 65536;
        let tagged = vec![
            candidate(0, "0100123456780000", ContentMetaType::Base),
            update,
            candidate(2, "0100123456781001", ContentMetaType::Dlc),
        ];
        assert_eq!(
            merge_content_tag(&tagged).as_deref(),
            Some("[B+U65536+1DLC]")
        );
    }

    #[test]
    fn compressed_merge_inputs_are_predecompressed() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let payload = temp.path().join("payload.bin");
        fs::write(&payload, b"merge-predecompression")?;
        let nsz = temp.path().join("compressed.nsz");
        build_from_paths(&[("payload.bin".to_owned(), payload)], &nsz)?;
        let plain = temp.path().join("plain.nsp");
        fs::write(&plain, b"plain-input-marker")?;
        let work = temp.path().join("work");
        fs::create_dir_all(&work)?;
        let cancel = AtomicBool::new(false);
        let mut logs = Vec::new();
        let prepared = prepare_merge_inputs(
            &[nsz.clone(), plain.clone()],
            &work,
            2,
            &cancel,
            &mut |line| logs.push(line),
        )?;
        assert_eq!(prepared.len(), 2);
        assert_eq!(prepared[0].0, nsz);
        assert_ne!(prepared[0].1, prepared[0].0);
        assert_eq!(
            prepared[0].1.extension().and_then(|ext| ext.to_str()),
            Some("nsp")
        );
        let index = Pfs0Index::parse(&prepared[0].1)?;
        assert!(index.entry("payload.bin").is_some());
        assert_eq!(prepared[1], (plain.clone(), plain));
        assert!(logs.iter().any(|line| line.contains("스레드: 2")));
        Ok(())
    }
}
