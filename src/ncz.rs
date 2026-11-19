//! Self-contained, streaming NCZ reader. This is an independent implementation
//! of the public NSZ/NCZ layout specification, not a translation of the NSZ
//! reference project's source code. Input files and cryptographic keys are
//! never modified. The NCZ header itself contains section re-encryption data.
//!
//! Supports solid and independent Zstandard blocks, including uncompressed
//! blocks. The output is the original encrypted NCA byte stream.

use aes::{
    cipher::{BlockEncrypt, KeyInit},
    Aes128,
};
use anyhow::{Context, Result};
use std::io::{self, Read, Seek, SeekFrom, Write};

const HEADER: u64 = 0x4000;
const IO_CHUNK: usize = 256 * 1024;
const MAX_SECTION_COUNT: u64 = 4096;
const MAX_NCA_SIZE: u64 = 512 * 1024 * 1024 * 1024;
const MAX_BLOCK_SIZE: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
struct Section {
    start: u64,
    size: u64,
    crypto: u64,
    key: [u8; 16],
    counter: [u8; 16],
}

#[derive(Clone, Debug)]
enum Encoding {
    Solid,
    Blocks {
        block_size: usize,
        unpacked_size: u64,
        sizes: Vec<u32>,
    },
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub expanded_size: u64,
    header_end: u64,
    sections: Vec<Section>,
    encoding: Encoding,
}

fn read_u64(reader: &mut impl Read) -> Result<u64> {
    let mut raw = [0u8; 8];
    reader.read_exact(&mut raw)?;
    Ok(u64::from_le_bytes(raw))
}

fn read_u32(reader: &mut impl Read) -> Result<u32> {
    let mut raw = [0u8; 4];
    reader.read_exact(&mut raw)?;
    Ok(u32::from_le_bytes(raw))
}

/// Inspect the NCZ metadata without inflating its contents. All offsets are
/// bounded by the containing PFS0/HFS0 entry, including block-size tables.
pub fn inspect<R: Read + Seek>(source: &mut R, offset: u64, size: u64) -> Result<Layout> {
    anyhow::ensure!(size >= HEADER + 16, "NCZ 컨테이너가 너무 작습니다");
    let limit = offset
        .checked_add(size)
        .context("NCZ 파일 오프셋 범위 초과")?;
    source.seek(SeekFrom::Start(offset + HEADER))?;
    let mut signature = [0u8; 8];
    source.read_exact(&mut signature)?;
    anyhow::ensure!(&signature == b"NCZSECTN", "NCZSECTN 헤더가 없습니다");
    let count = read_u64(source)?;
    anyhow::ensure!(
        (1..=MAX_SECTION_COUNT).contains(&count),
        "NCZ 섹션 개수가 잘못되었습니다"
    );
    anyhow::ensure!(
        HEADER + 16 + count * 64 <= size,
        "NCZ 섹션 헤더가 파일 크기를 초과합니다"
    );
    let mut sections = Vec::with_capacity(count as usize);
    let mut expanded_size = HEADER;
    for _ in 0..count {
        let start = read_u64(source)?;
        let length = read_u64(source)?;
        let crypto = read_u64(source)?;
        let _reserved = read_u64(source)?;
        let mut key = [0u8; 16];
        let mut counter = [0u8; 16];
        source.read_exact(&mut key)?;
        source.read_exact(&mut counter)?;
        anyhow::ensure!(
            start >= expanded_size && length > 0,
            "NCZ 섹션 주소가 중복되거나 뒤바뀌었습니다"
        );
        anyhow::ensure!(
            matches!(crypto, 0..=4),
            "지원하지 않는 NCZ 암호화 방식: {crypto}"
        );
        expanded_size = start
            .checked_add(length)
            .context("NCZ 섹션 크기 범위 초과")?;
        anyhow::ensure!(expanded_size <= MAX_NCA_SIZE, "NCZ 출력 크기 제한 초과");
        sections.push(Section {
            start,
            size: length,
            crypto,
            key,
            counter,
        });
    }
    let magic_start = source.stream_position()?;
    anyhow::ensure!(
        magic_start.checked_add(8).is_some_and(|v| v <= limit),
        "NCZ 데이터가 없습니다"
    );
    source.read_exact(&mut signature)?;
    let encoding = if &signature == b"NCZBLOCK" {
        anyhow::ensure!(
            source.stream_position()? + 16 <= limit,
            "NCZBLOCK 헤더가 잘렸습니다"
        );
        let mut info = [0u8; 4];
        source.read_exact(&mut info)?;
        anyhow::ensure!(
            (1..=2).contains(&info[0])
                && (1..=2).contains(&info[1])
                && (14..=25).contains(&info[3]),
            "지원하지 않는 NCZBLOCK 버전·블록 크기"
        );
        let block_size = 1usize << info[3];
        anyhow::ensure!(block_size <= MAX_BLOCK_SIZE, "NCZBLOCK 블록 크기 제한 초과");
        let block_count = read_u32(source)? as u64;
        let unpacked_size = read_u64(source)?;
        let expected = expanded_size - HEADER;
        anyhow::ensure!(
            unpacked_size == expected,
            "NCZBLOCK 출력 크기와 섹션 정보가 다릅니다"
        );
        let expected_count = unpacked_size.div_ceil(block_size as u64);
        anyhow::ensure!(
            block_count == expected_count && block_count > 0 && block_count <= 1_000_000,
            "NCZBLOCK 블록 개수가 잘못되었습니다"
        );
        anyhow::ensure!(
            source.stream_position()? + block_count * 4 <= limit,
            "NCZBLOCK 크기 목록이 잘렸습니다"
        );
        let mut sizes = Vec::with_capacity(block_count as usize);
        let mut compressed_total = 0u64;
        for i in 0..block_count {
            let compressed = read_u32(source)?;
            let plain = expected
                .saturating_sub(i * block_size as u64)
                .min(block_size as u64);
            anyhow::ensure!(
                compressed > 0 && compressed as u64 <= plain,
                "NCZBLOCK 압축 크기 오류"
            );
            compressed_total = compressed_total
                .checked_add(compressed as u64)
                .context("NCZBLOCK 크기 범위 초과")?;
            sizes.push(compressed);
        }
        anyhow::ensure!(
            source.stream_position()? + compressed_total <= limit,
            "NCZBLOCK 입력 데이터가 누락되었습니다"
        );
        Encoding::Blocks {
            block_size,
            unpacked_size,
            sizes,
        }
    } else {
        source.seek(SeekFrom::Start(magic_start))?;
        Encoding::Solid
    };
    let header_end = source.stream_position()?;
    anyhow::ensure!(header_end < limit, "NCZ 압축 데이터가 없습니다");
    Ok(Layout {
        expanded_size,
        header_end,
        sections,
        encoding,
    })
}

/// A sequential decoder for NCZBLOCK, which stores either an independent
/// zstd frame or an uncompressed chunk per block.
struct BlockReader<R: Read> {
    source: R,
    sizes: Vec<u32>,
    block_size: usize,
    total: u64,
    emitted: u64,
    index: usize,
    compressed: Vec<u8>,
    buffer: Vec<u8>,
    cursor: usize,
    decoder: zstd::bulk::Decompressor<'static>,
}

impl<R: Read> BlockReader<R> {
    fn new(source: R, sizes: Vec<u32>, block_size: usize, total: u64) -> io::Result<Self> {
        Ok(Self {
            source,
            sizes,
            block_size,
            total,
            emitted: 0,
            index: 0,
            compressed: Vec::with_capacity(block_size.min(1024 * 1024)),
            buffer: Vec::with_capacity(block_size.min(1024 * 1024)),
            cursor: 0,
            decoder: zstd::bulk::Decompressor::new()?,
        })
    }

    fn next_block(&mut self) -> io::Result<()> {
        if self.index >= self.sizes.len() {
            self.buffer.clear();
            self.cursor = 0;
            return Ok(());
        }
        let expected = (self.total - self.emitted).min(self.block_size as u64) as usize;
        let compressed_len = self.sizes[self.index] as usize;
        self.compressed.resize(compressed_len, 0);
        self.source.read_exact(&mut self.compressed)?;
        self.buffer.resize(expected, 0);
        let decoded = if expected == compressed_len {
            self.buffer.copy_from_slice(&self.compressed);
            expected
        } else {
            self.decoder
                .decompress_to_buffer(&self.compressed, &mut self.buffer[..])?
        };
        if decoded != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "NCZBLOCK decompressed size mismatch",
            ));
        }
        self.index += 1;
        self.emitted += expected as u64;
        self.cursor = 0;
        Ok(())
    }
}

impl<R: Read> Read for BlockReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.cursor >= self.buffer.len() {
            self.next_block()?;
        }
        let available = self.buffer.len().saturating_sub(self.cursor);
        let count = available.min(output.len());
        if count > 0 {
            output[..count].copy_from_slice(&self.buffer[self.cursor..self.cursor + count]);
            self.cursor += count;
        }
        Ok(count)
    }
}

/// AES-CTR's counter uses the first eight NCZ counter bytes and the absolute
/// 16-byte NCA block number (big endian), not a per-section relative counter.
fn reencrypt_ctr_with_cipher(cipher: &Aes128, data: &mut [u8], offset: u64, counter: &[u8; 16]) {
    // Encrypt independent counters in parallel via the AES backend, instead
    // of allocating and encrypting a block for every 16 bytes. The same
    // expanded key is reused for all chunks in each encrypted NCA section.
    const BATCH: usize = 64;
    let mut keystream = [aes::Block::default(); BATCH];
    let mut position = 0usize;
    while position < data.len() {
        let absolute = offset + position as u64;
        let block = absolute / 16;
        let within_block = (absolute % 16) as usize;
        let full_blocks = if within_block == 0 {
            ((data.len() - position) / 16).min(BATCH)
        } else {
            0
        };
        if full_blocks == 0 {
            let state = &mut keystream[0];
            state[..8].copy_from_slice(&counter[..8]);
            state[8..].copy_from_slice(&block.to_be_bytes());
            cipher.encrypt_block(state);
            let count = (16 - within_block).min(data.len() - position);
            for (byte, mask) in data[position..position + count]
                .iter_mut()
                .zip(&state[within_block..within_block + count])
            {
                *byte ^= *mask;
            }
            position += count;
            continue;
        }
        for (index, state) in keystream[..full_blocks].iter_mut().enumerate() {
            state[..8].copy_from_slice(&counter[..8]);
            state[8..].copy_from_slice(&(block + index as u64).to_be_bytes());
        }
        cipher.encrypt_blocks(&mut keystream[..full_blocks]);
        let count = full_blocks * 16;
        for (target, mask) in data[position..position + count]
            .as_chunks_mut::<16>()
            .0
            .iter_mut()
            .zip(&keystream[..full_blocks])
        {
            for (byte, mask) in target.iter_mut().zip(mask.iter()) {
                *byte ^= *mask;
            }
        }
        position += count;
    }
}

#[cfg(test)]
fn reencrpyt_ctr(data: &mut [u8], offset: u64, key: &[u8; 16], counter: &[u8; 16]) -> Result<()> {
    let cipher = Aes128::new_from_slice(key).context("NCZ AES 키 크기 오류")?;
    reencrypt_ctr_with_cipher(&cipher, data, offset, counter);
    Ok(())
}

pub fn decompress<R: Read + Seek, W: Write>(
    source: &mut R,
    start: u64,
    compressed_size: u64,
    target: &mut W,
) -> Result<u64> {
    let layout = inspect(source, start, compressed_size)?;
    source.seek(SeekFrom::Start(start))?;
    let mut header = vec![0u8; HEADER as usize];
    source.read_exact(&mut header)?;
    target.write_all(&header)?;
    source.seek(SeekFrom::Start(layout.header_end))?;
    let remaining = compressed_size - (layout.header_end - start);
    let compressed = source.take(remaining);
    let mut reader: Box<dyn Read + '_> = match layout.encoding {
        Encoding::Solid => Box::new(zstd::stream::read::Decoder::new(compressed)?),
        Encoding::Blocks {
            block_size,
            unpacked_size,
            sizes,
        } => Box::new(BlockReader::new(
            compressed,
            sizes,
            block_size,
            unpacked_size,
        )?),
    };
    let mut buffer = vec![0u8; IO_CHUNK];
    let mut position = HEADER;
    for section in &layout.sections {
        let cipher = matches!(section.crypto, 3 | 4)
            .then(|| Aes128::new_from_slice(&section.key))
            .transpose()
            .context("NCZ AES 키 크기 오류")?;
        if position < section.start {
            let mut left = section.start - position;
            while left > 0 {
                let len = left.min(IO_CHUNK as u64) as usize;
                reader
                    .read_exact(&mut buffer[..len])
                    .context("NCZ 섹션 앞 데이터 손상")?;
                target.write_all(&buffer[..len])?;
                position += len as u64;
                left -= len as u64;
            }
        }
        let mut left = section.size;
        while left > 0 {
            let len = left.min(IO_CHUNK as u64) as usize;
            reader
                .read_exact(&mut buffer[..len])
                .context("NCZ 압축 데이터가 잘렸습니다")?;
            if let Some(cipher) = cipher.as_ref() {
                reencrypt_ctr_with_cipher(cipher, &mut buffer[..len], position, &section.counter);
            }
            target.write_all(&buffer[..len])?;
            position += len as u64;
            left -= len as u64;
        }
    }
    anyhow::ensure!(
        position == layout.expanded_size,
        "NCZ 출력 크기가 맞지 않습니다"
    );
    let mut unexpected = [0u8; 1];
    anyhow::ensure!(
        reader.read(&mut unexpected)? == 0,
        "NCZ 압축 데이터에 예상치 못한 여분 데이터가 있습니다"
    );
    Ok(position)
}

/// Bounded metadata/control NCA extraction for direct CNMT/NACP parsing.
/// Never inflates large program/data archives while scanning a library.
pub(crate) fn small_nca_bytes<R: Read + Seek>(
    source: &mut R,
    offset: u64,
    size: u64,
    max_expanded_bytes: u64,
) -> Option<Vec<u8>> {
    let length = inspect(source, offset, size).ok()?.expanded_size;
    if length > max_expanded_bytes || length > usize::MAX as u64 {
        return None;
    }
    let mut output = Vec::with_capacity(length as usize);
    decompress(source, offset, size, &mut output).ok()?;
    (output.len() as u64 == length).then_some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sample_ncz(original: &[u8], sections: &[Section], blocks: bool) -> Vec<u8> {
        let mut plain = original[HEADER as usize..].to_vec();
        for section in sections {
            if matches!(section.crypto, 3 | 4) {
                let lo = section.start as usize - HEADER as usize;
                let hi = lo + section.size as usize;
                reencrpyt_ctr(
                    &mut plain[lo..hi],
                    section.start,
                    &section.key,
                    &section.counter,
                )
                .unwrap();
            }
        }
        let mut output = original[..HEADER as usize].to_vec();
        output.extend_from_slice(b"NCZSECTN");
        output.extend_from_slice(&(sections.len() as u64).to_le_bytes());
        for s in sections {
            output.extend_from_slice(&s.start.to_le_bytes());
            output.extend_from_slice(&s.size.to_le_bytes());
            output.extend_from_slice(&s.crypto.to_le_bytes());
            output.extend_from_slice(&0u64.to_le_bytes());
            output.extend_from_slice(&s.key);
            output.extend_from_slice(&s.counter);
        }
        if blocks {
            let block_size = 1 << 14;
            let encoded: Vec<_> = plain
                .chunks(block_size)
                .map(|chunk| {
                    let compressed = zstd::bulk::compress(chunk, 3).unwrap();
                    if compressed.len() < chunk.len() {
                        compressed
                    } else {
                        chunk.to_vec()
                    }
                })
                .collect();
            output.extend_from_slice(b"NCZBLOCK");
            output.extend_from_slice(&[1, 1, 0, 14]);
            output.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
            output.extend_from_slice(&(plain.len() as u64).to_le_bytes());
            for chunk in &encoded {
                output.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            }
            for chunk in &encoded {
                output.extend_from_slice(chunk);
            }
        } else {
            output.extend_from_slice(&zstd::bulk::compress(&plain, 3).unwrap());
        }
        output
    }

    #[test]
    fn solid_and_block_ncz_restore_original_ctr_encrypted_nca() {
        let mut original = vec![0x99u8; 0x4000];
        original.extend((0..0x21234u32).map(|v| (v % 251) as u8));
        let sections = vec![
            Section {
                start: HEADER,
                size: 0x8000,
                crypto: 3,
                key: [0x5a; 16],
                counter: [0x32; 16],
            },
            Section {
                start: HEADER + 0x8000,
                size: 0x19234,
                crypto: 4,
                key: [0x14; 16],
                counter: [0x11; 16],
            },
        ];
        for is_block in [false, true] {
            let encoded = sample_ncz(&original, &sections, is_block);
            let mut ncz = Cursor::new(encoded);
            let size = ncz.get_ref().len() as u64;
            let layout = inspect(&mut ncz, 0, size).unwrap();
            assert_eq!(layout.expanded_size, original.len() as u64);
            let mut restored = Vec::new();
            let size = ncz.get_ref().len() as u64;
            assert_eq!(
                decompress(&mut ncz, 0, size, &mut restored).unwrap(),
                original.len() as u64
            );
            assert_eq!(restored, original);
        }
    }

    #[test]
    fn corrupted_ncz_headers_and_oversized_blocks_are_rejected() {
        let mut data = vec![0u8; 0x4100];
        assert!(inspect(&mut Cursor::new(&data), 0, data.len() as u64).is_err());
        data[0x4000..0x4008].copy_from_slice(b"NCZSECTN");
        data[0x4008..0x4010].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(inspect(&mut Cursor::new(&data), 0, data.len() as u64).is_err());
    }
}
