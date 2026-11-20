use anyhow::{Context, Result, bail};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_FILES: u32 = 16_384;
const MAX_STRING_TABLE: u32 = 64 * 1024 * 1024;

/// A transformed NCA is stored in a temporary file, while an unchanged
/// container entry can be read directly from its original package. Avoid
/// writing unchanged entries to temporary files only to read them again.
#[derive(Debug, Clone)]
pub enum Pfs0Source {
    File(PathBuf),
    Region {
        container: PathBuf,
        offset: u64,
        size: u64,
    },
}

impl Pfs0Source {
    fn size(&self) -> Result<u64> {
        match self {
            Self::File(path) => Ok(fs::metadata(path)
                .with_context(|| format!("입력 파일 없음: {}", path.display()))?
                .len()),
            Self::Region {
                container,
                offset,
                size,
            } => {
                let len = fs::metadata(container)
                    .with_context(|| format!("입력 컨테이너 없음: {}", container.display()))?
                    .len();
                let end = offset
                    .checked_add(*size)
                    .context("PFS0 입력 범위 오버플로")?;
                if end > len {
                    bail!("PFS0 원본 영역이 컨테이너 범위를 벗어납니다");
                }
                Ok(*size)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pfs0Entry {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct Pfs0Index {
    pub data_start: u64,
    pub entries: Vec<Pfs0Entry>,
}

impl Pfs0Index {
    pub fn parse(path: &Path) -> Result<Self> {
        let mut file = BufReader::new(File::open(path)?);
        let file_size = file.get_ref().metadata()?.len();
        Self::parse_reader(&mut file, 0, file_size)
    }

    pub fn parse_reader<R: Read + Seek>(
        reader: &mut R,
        base: u64,
        container_size: u64,
    ) -> Result<Self> {
        reader.seek(SeekFrom::Start(base))?;
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if &magic != b"PFS0" {
            bail!("PFS0 magic 불일치");
        }
        let count = reader.read_u32::<LittleEndian>()?;
        let string_size = reader.read_u32::<LittleEndian>()?;
        let _reserved = reader.read_u32::<LittleEndian>()?;
        if count == 0 || count > MAX_FILES {
            bail!("비정상 PFS0 파일 수: {count}");
        }
        if string_size > MAX_STRING_TABLE {
            bail!("PFS0 문자열 테이블이 너무 큽니다");
        }

        let mut raw = vec![0u8; count as usize * 24];
        reader.read_exact(&mut raw)?;
        let mut strings = vec![0u8; string_size as usize];
        reader.read_exact(&mut strings)?;
        let data_start = reader.stream_position()?;
        let limit = base
            .checked_add(container_size)
            .context("PFS0 크기 오버플로")?;

        let mut entries = Vec::with_capacity(count as usize);
        let mut seen = HashSet::new();
        for i in 0..count as usize {
            let p = &raw[i * 24..(i + 1) * 24];
            let mut c = io::Cursor::new(p);
            let offset = c.read_u64::<LittleEndian>()?;
            let size = c.read_u64::<LittleEndian>()?;
            let string_offset = c.read_u32::<LittleEndian>()? as usize;
            let _reserved = c.read_u32::<LittleEndian>()?;
            if string_offset >= strings.len() {
                bail!("PFS0 문자열 오프셋 오류");
            }
            let end = strings[string_offset..]
                .iter()
                .position(|&b| b == 0)
                .map(|n| string_offset + n)
                .context("PFS0 파일명 종료 문자가 없습니다")?;
            let name = std::str::from_utf8(&strings[string_offset..end])
                .context("PFS0 파일명이 UTF-8이 아닙니다")?
                .to_owned();
            validate_name(&name)?;
            let folded = name.to_lowercase();
            if !seen.insert(folded) {
                bail!("중복 PFS0 파일명: {name}");
            }
            let abs_end = data_start
                .checked_add(offset)
                .and_then(|v| v.checked_add(size))
                .context("PFS0 엔트리 크기 오버플로")?;
            if abs_end > limit {
                bail!("PFS0 엔트리가 컨테이너 범위를 벗어납니다: {name}");
            }
            entries.push(Pfs0Entry { name, offset, size });
        }
        Ok(Self {
            data_start,
            entries,
        })
    }

    pub fn entry(&self, name: &str) -> Option<&Pfs0Entry> {
        self.entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
    }

    pub fn read_entry(&self, container: &Path, entry: &Pfs0Entry) -> Result<Vec<u8>> {
        let size: usize = entry
            .size
            .try_into()
            .context("엔트리가 메모리 한도를 넘습니다")?;
        let mut file = BufReader::new(File::open(container)?);
        file.seek(SeekFrom::Start(self.data_start + entry.offset))?;
        let mut data = vec![0u8; size];
        file.read_exact(&mut data)?;
        Ok(data)
    }

    pub fn extract_all(&self, container: &Path, out_dir: &Path) -> Result<Vec<PathBuf>> {
        fs::create_dir_all(out_dir)?;
        let mut source = BufReader::new(File::open(container)?);
        let mut output = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            let dst = out_dir.join(&entry.name);
            source.seek(SeekFrom::Start(self.data_start + entry.offset))?;
            let mut out = BufWriter::new(File::create(&dst)?);
            copy_n(&mut source, &mut out, entry.size)?;
            out.flush()?;
            output.push(dst);
        }
        Ok(output)
    }
}

pub fn build_from_dir(dir: &Path, out_path: &Path) -> Result<()> {
    let mut paths: Vec<(String, PathBuf)> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    paths.sort_by(|a, b| a.0.cmp(&b.0));
    build_from_paths(&paths, out_path)
}

pub fn build_from_paths(entries: &[(String, PathBuf)], out_path: &Path) -> Result<()> {
    let sources = entries
        .iter()
        .map(|(name, path)| (name.clone(), Pfs0Source::File(path.clone())))
        .collect::<Vec<_>>();
    build_from_sources(&sources, out_path, &AtomicBool::new(false))
}

/// Stream transformed entries directly into a PFS0 package. This avoids
/// writing compressed NCZ files to disk merely to copy them into the package.
/// The entry table is backfilled when the final entry sizes are known.
pub fn build_from_streams<F>(
    names: &[String],
    out_path: &Path,
    cancel: &AtomicBool,
    mut write_entry: F,
) -> Result<()>
where
    F: FnMut(usize, &mut BufWriter<File>) -> Result<()>,
{
    if names.is_empty() || names.len() > MAX_FILES as usize {
        bail!("PFS0 빌드 대상 파일 수가 유효하지 않습니다");
    }
    let mut strings = Vec::new();
    let mut string_offsets = Vec::with_capacity(names.len());
    let mut seen = HashSet::new();
    for name in names {
        validate_name(name)?;
        if !seen.insert(name.to_lowercase()) {
            bail!("중복 PFS0 파일명: {name}");
        }
        string_offsets.push(u32::try_from(strings.len()).context("PFS0 문자열 테이블 크기 초과")?);
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        if strings.len() > MAX_STRING_TABLE as usize {
            bail!("PFS0 문자열 테이블이 너무 큽니다");
        }
    }
    let header_size = 16usize + names.len() * 24 + strings.len();
    let aligned_header = (header_size + 1 + 0x1f) & !0x1f;
    strings.resize(strings.len() + aligned_header - header_size, 0);

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let partial = out_path.with_extension(format!(
        "{}.partial",
        out_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("pfs0")
    ));
    let result = (|| -> Result<()> {
        let mut out = BufWriter::with_capacity(4 * 1024 * 1024, File::create(&partial)?);
        out.write_all(b"PFS0")?;
        out.write_u32::<LittleEndian>(names.len() as u32)?;
        out.write_u32::<LittleEndian>(strings.len() as u32)?;
        out.write_u32::<LittleEndian>(0)?;
        out.write_all(&vec![0u8; names.len() * 24])?;
        out.write_all(&strings)?;

        let mut lengths = Vec::with_capacity(names.len());
        let mut total_data = 0u64;
        for index in 0..names.len() {
            if cancel.load(Ordering::Relaxed) {
                bail!("작업이 중단되었습니다");
            }
            let before = out.stream_position()?;
            write_entry(index, &mut out)?;
            let after = out.stream_position()?;
            let len = after
                .checked_sub(before)
                .context("PFS0 출력 위치가 역행했습니다")?;
            total_data = total_data
                .checked_add(len)
                .context("PFS0 데이터 크기 오버플로")?;
            lengths.push(len);
        }
        if cancel.load(Ordering::Relaxed) {
            bail!("작업이 중단되었습니다");
        }
        let end = out.stream_position()?;
        out.seek(SeekFrom::Start(16))?;
        let mut rel = 0u64;
        for (index, len) in lengths.iter().enumerate() {
            out.write_u64::<LittleEndian>(rel)?;
            out.write_u64::<LittleEndian>(*len)?;
            out.write_u32::<LittleEndian>(string_offsets[index])?;
            out.write_u32::<LittleEndian>(0)?;
            rel += len;
        }
        out.flush()?;
        drop(out);
        let actual = fs::metadata(&partial)?.len();
        if actual != end || actual != aligned_header as u64 + total_data {
            bail!("PFS0 패키지 길이 검증 실패");
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }
    if out_path.exists() {
        fs::remove_file(out_path)?;
    }
    fs::rename(&partial, out_path)?;
    Ok(())
}

/// Mixed direct-region and generated-file PFS0 assembly. All source ranges
/// are checked before creating the destination, and cancellation works during
/// the potentially multi-gigabyte final copy.
pub fn build_from_sources(
    entries: &[(String, Pfs0Source)],
    out_path: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    if entries.is_empty() {
        bail!("PFS0 빌드 대상이 비어 있습니다");
    }
    if entries.len() > MAX_FILES as usize {
        bail!("PFS0 파일 수가 한도를 넘습니다");
    }
    let mut string_offsets = Vec::with_capacity(entries.len());
    let mut strings = Vec::new();
    let mut sizes = Vec::with_capacity(entries.len());
    let mut seen = HashSet::new();
    for (name, source) in entries {
        validate_name(name)?;
        if !seen.insert(name.to_lowercase()) {
            bail!("중복 PFS0 파일명: {name}");
        }
        string_offsets.push(strings.len() as u32);
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        sizes.push(source.size()?);
    }
    let full_header = 16usize + entries.len() * 24 + strings.len();
    let aligned_header = (full_header + 1 + 0x1f) & !0x1f;
    strings.resize(strings.len() + (aligned_header - full_header), 0);

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let partial = out_path.with_extension(format!(
        "{}.partial",
        out_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("pfs0")
    ));
    let result = (|| -> Result<()> {
        let mut out = BufWriter::with_capacity(4 * 1024 * 1024, File::create(&partial)?);
        out.write_all(b"PFS0")?;
        out.write_u32::<LittleEndian>(entries.len() as u32)?;
        out.write_u32::<LittleEndian>(strings.len() as u32)?;
        out.write_u32::<LittleEndian>(0)?;
        let mut rel = 0u64;
        for (i, size) in sizes.iter().enumerate() {
            out.write_u64::<LittleEndian>(rel)?;
            out.write_u64::<LittleEndian>(*size)?;
            out.write_u32::<LittleEndian>(string_offsets[i])?;
            out.write_u32::<LittleEndian>(0)?;
            rel = rel
                .checked_add(*size)
                .context("PFS0 데이터 크기 오버플로")?;
        }
        out.write_all(&strings)?;
        let mut buf = vec![0u8; 2 * 1024 * 1024];
        let mut current_region: Option<(PathBuf, BufReader<File>)> = None;
        for ((_, source), &size) in entries.iter().zip(&sizes) {
            match source {
                Pfs0Source::File(path) => {
                    let mut src = BufReader::with_capacity(buf.len(), File::open(path)?);
                    copy_bounded(&mut src, &mut out, size, &mut buf, cancel)?;
                }
                Pfs0Source::Region {
                    container, offset, ..
                } => {
                    if current_region
                        .as_ref()
                        .is_none_or(|(path, _)| path != container)
                    {
                        current_region = Some((
                            container.clone(),
                            BufReader::with_capacity(buf.len(), File::open(container)?),
                        ));
                    }
                    let (_, reader) = current_region.as_mut().expect("region file available");
                    reader.seek(SeekFrom::Start(*offset))?;
                    copy_bounded(reader, &mut out, size, &mut buf, cancel)?;
                }
            }
        }
        out.flush()?;
        drop(out);
        let expected = aligned_header as u64 + sizes.iter().sum::<u64>();
        let actual = fs::metadata(&partial)?.len();
        if actual != expected {
            bail!("PFS0 결과 크기 검증 실패: {actual} != {expected}");
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&partial);
        return Err(e);
    }
    if out_path.exists() {
        fs::remove_file(out_path)?;
    }
    fs::rename(&partial, out_path)?;
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        bail!("안전하지 않은 컨테이너 파일명: {name:?}");
    }
    Ok(())
}

fn copy_bounded<R: Read, W: Write>(
    source: &mut R,
    output: &mut W,
    mut remaining: u64,
    scratch: &mut [u8],
    cancel: &AtomicBool,
) -> Result<()> {
    while remaining > 0 {
        if cancel.load(Ordering::Relaxed) {
            bail!("작업이 중단되었습니다");
        }
        let take = remaining.min(scratch.len() as u64) as usize;
        source.read_exact(&mut scratch[..take])?;
        output.write_all(&scratch[..take])?;
        remaining -= take as u64;
    }
    Ok(())
}

pub fn copy_n<R: Read, W: Write>(src: &mut R, dst: &mut W, mut remaining: u64) -> Result<()> {
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    while remaining > 0 {
        let take = usize::try_from(remaining.min(buf.len() as u64)).unwrap();
        let n = src.read(&mut buf[..take])?;
        if n == 0 {
            bail!("컨테이너 입력이 예상보다 일찍 끝났습니다");
        }
        dst.write_all(&buf[..n])?;
        remaining -= n as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_entries_match_file_packaging_byte_for_byte() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let paths = [
            ("first.nca".to_string(), dir.path().join("first.bin")),
            ("second.tik".to_string(), dir.path().join("second.bin")),
        ];
        fs::write(&paths[0].1, vec![0x61u8; 4 * 1024 * 1024 + 37])?;
        fs::write(&paths[1].1, b"abcdefg")?;
        let staged = dir.path().join("staged.nsp");
        let streamed = dir.path().join("streamed.nsp");
        build_from_paths(&paths, &staged)?;
        let names = paths
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        build_from_streams(&names, &streamed, &AtomicBool::new(false), |i, out| {
            let mut file = File::open(&paths[i].1)?;
            io::copy(&mut file, out)?;
            Ok(())
        })?;
        assert_eq!(fs::read(staged)?, fs::read(streamed)?);
        Ok(())
    }

    #[test]
    fn streamed_failure_cleans_partial_without_replacing_existing_output() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let dest = dir.path().join("existing.nsz");
        fs::write(&dest, b"previous valid package")?;
        let names = vec!["entry.ncz".to_string()];
        let err = build_from_streams(&names, &dest, &AtomicBool::new(false), |_, out| {
            out.write_all(b"half written")?;
            bail!("simulated encoder failure");
        });
        assert!(err.is_err());
        assert_eq!(fs::read(&dest)?, b"previous valid package");
        assert!(!dest.with_extension("nsz.partial").exists());
        let canceled = AtomicBool::new(true);
        assert!(build_from_streams(&names, &dest, &canceled, |_, _| Ok(())).is_err());
        assert_eq!(fs::read(dest)?, b"previous valid package");
        Ok(())
    }

    #[test]
    fn direct_regions_match_staged_package_byte_for_byte() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let paths = [
            ("가.nca".into(), tmp.path().join("a.nca")),
            ("b.tik".into(), tmp.path().join("b.tik")),
        ];
        fs::write(&paths[0].1, vec![7u8; 4 * 1024 * 1024 + 37])?;
        fs::write(&paths[1].1, b"small ticket")?;
        let original = tmp.path().join("input.nsp");
        build_from_paths(&paths, &original)?;
        let index = Pfs0Index::parse(&original)?;
        let regions = index
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.name.clone(),
                    Pfs0Source::Region {
                        container: original.clone(),
                        offset: index.data_start + entry.offset,
                        size: entry.size,
                    },
                )
            })
            .collect::<Vec<_>>();
        let direct = tmp.path().join("direct.nsp");
        build_from_sources(&regions, &direct, &AtomicBool::new(false))?;
        assert_eq!(fs::read(&original)?, fs::read(&direct)?);
        let mixed = tmp.path().join("mixed.nsp");
        let mixed_sources = vec![
            regions[0].clone(),
            (
                index.entries[1].name.clone(),
                Pfs0Source::File(paths[1].1.clone()),
            ),
        ];
        build_from_sources(&mixed_sources, &mixed, &AtomicBool::new(false))?;
        assert_eq!(fs::read(&original)?, fs::read(&mixed)?);
        Ok(())
    }

    #[test]
    fn invalid_region_and_cancel_leave_no_output() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let src = tmp.path().join("source.nca");
        fs::write(&src, vec![0x55u8; 4 * 1024 * 1024])?;
        let invalid = tmp.path().join("invalid.nsp");
        let range = vec![(
            "invalid.nca".to_owned(),
            Pfs0Source::Region {
                container: src.clone(),
                offset: 4 * 1024 * 1024 - 1,
                size: 2,
            },
        )];
        assert!(build_from_sources(&range, &invalid, &AtomicBool::new(false)).is_err());
        assert!(!invalid.exists());
        let canceled = tmp.path().join("cancel.nsp");
        assert!(
            build_from_sources(
                &[("cancel.nca".to_owned(), Pfs0Source::File(src))],
                &canceled,
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert!(!canceled.exists());
        assert!(!canceled.with_extension("nsp.partial").exists());
        Ok(())
    }

    #[test]
    fn pfs0_roundtrip() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("a.nca"), b"abc")?;
        fs::write(temp.path().join("b.cnmt.nca"), vec![7u8; 10_003])?;
        let out = temp.path().join("sample.nsp");
        build_from_dir(temp.path(), &out)?;
        let idx = Pfs0Index::parse(&out)?;
        assert_eq!(idx.entries.len(), 2);
        assert_eq!(idx.read_entry(&out, idx.entry("a.nca").unwrap())?, b"abc");
        Ok(())
    }
}
