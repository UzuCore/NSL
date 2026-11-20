"""Validate native NSZ/XCZ repacker input index rules without decoding ROMs.

This is an independent Python model, NOT an execution of Rust code or a test
of decompression correctness. Input files and the library DB are read-only.

    python -B scripts/qa-native-package-index-readonly.py --require-corpus
"""

from __future__ import annotations

import argparse
import collections
import json
import random
import runpy
import sqlite3
import struct
from io import BytesIO
from pathlib import Path

HERE = Path(__file__).resolve().parent
CORE = runpy.run_path(str(HERE / "qa-rom-index-readonly.py"))
parse_index = CORE["parse_index"]
make_fixture = CORE["make_fixture"]

MAX_ENTRIES = 16_384
MAX_NAMES = 16 * 1024 * 1024


def inspect_ncz_layout(source, start: int, size: int, failures=None):
    """Read NCZ section/block metadata only; never decode, decrypt or copy.

    Independently models ncz.rs::inspect's structural bounds. The section
    keys/counters are deliberately skipped rather than read into memory.
    An accepted header is NOT proof that its zstd payload can be decoded.
    """
    def reject(reason):
        if failures is not None:
            failures["ncz_failure_" + reason] += 1
        return None
    header = 0x4000
    if size < header + 16:
        return reject("too_small")
    limit = start + size
    source.seek(start + header)
    magic = source.read(16)
    if len(magic) != 16 or magic[:8] != b"NCZSECTN":
        return reject("section_magic")
    sections = struct.unpack_from("<Q", magic, 8)[0]
    if not 1 <= sections <= 32768 or header + 16 + sections * 64 > size:
        return reject("section_count_or_bounds")
    expanded = header
    for _ in range(sections):
        prefix = source.read(24)
        if len(prefix) != 24:
            return reject("section_truncated")
        raw_offset, raw_length, crypto = struct.unpack("<QQQ", prefix)
        # Skip reserved 8 bytes and 16-byte key/counter; never materialize them.
        source.seek(40, 1)
        raw_end = raw_offset + raw_length
        # NCA section descriptors may begin at 0xC00, inside the 0x4000
        # header already copied verbatim. Only the tail belongs to NCZ data.
        offset = header if expanded == header and raw_offset == 0xC00 and raw_end > header else raw_offset
        length = raw_end - offset
        if offset < expanded or length <= 0 or crypto > 4:
            return reject("section_address_or_crypto")
        expanded = raw_end
        if expanded > 512 * 1024**3:
            return reject("expanded_limit")
    magic_start = source.tell()
    if magic_start + 8 > limit:
        return reject("missing_payload")
    marker = source.read(8)
    if marker != b"NCZBLOCK":
        # The solid NCZ stream starts at magic_start. It is not decompressed.
        return {"encoding": "solid", "expanded": expanded, "sections": sections, "blocks": 0,
                "plain_tail": 0} \
            if magic_start < limit else reject("missing_solid_payload")

    if source.tell() + 16 > limit:
        return reject("block_header_truncated")
    block_header = source.read(16)
    if len(block_header) != 16:
        return reject("block_header_short")
    version, type_, _reserved, exponent, blocks, unpacked = struct.unpack("<BBBBIQ", block_header)
    if not (1 <= version <= 2 and 1 <= type_ <= 2 and 14 <= exponent <= 25):
        return reject("block_header_params")
    block_size = 1 << exponent
    encrypted_end = expanded - header
    if unpacked < encrypted_end or header + unpacked > 512 * 1024**3:
        return reject("block_output_short_or_oversized")
    expected = unpacked
    expected_blocks = (expected + block_size - 1) // block_size
    if not (0 < blocks <= 1_000_000) or blocks != expected_blocks:
        return reject("block_count_or_expanded")
    if source.tell() + blocks * 4 > limit:
        return reject("block_table_bounds")

    compressed_total = 0
    i = 0
    while i < blocks:
        amount = min(4096, blocks - i)
        buffer = source.read(amount * 4)
        if len(buffer) != amount * 4:
            return reject("block_table_truncated")
        for (compressed,) in struct.iter_unpack("<I", buffer):
            remaining = expected - i * block_size
            plain = min(remaining, block_size)
            if compressed == 0 or compressed > plain:
                return reject("compressed_block_size")
            compressed_total += compressed
            i += 1
    if source.tell() + compressed_total > limit or source.tell() >= limit:
        return reject("compressed_payload_bounds")
    return {"encoding": "block", "expanded": header + unpacked, "sections": sections,
            "blocks": blocks, "plain_tail": unpacked - encrypted_end}


def repacker_index(source, start: int, end: int, kind: str):
    """Model native_package.rs::read_index's structural input requirements.

    Unlike the generic read-only index, the XCZ converter must support empty
    HFS0 partitions and additionally preserve/validate their hash fields.
    """
    if start < 0 or start + 16 > end:
        return None
    source.seek(start)
    header = source.read(16)
    if len(header) != 16 or header[:4] != kind.encode("ascii"):
        return None
    count, names_size = struct.unpack_from("<II", header, 4)
    if count > MAX_ENTRIES or names_size > MAX_NAMES:
        return None
    row_size = 64 if kind == "HFS0" else 24
    data_start = start + 16 + count * row_size + names_size
    if data_start > end:
        return None
    if count == 0:
        # HFS0 update/normal partitions in real XCZ samples commonly have an
        # empty table. The packer intentionally handles these as empty plans.
        return {"entries": [], "empty": True}

    index = parse_index(source, start, end, kind=kind, strict=True)
    if index is None or len(index["entries"]) != count:
        return None
    source.seek(start + 16)
    table = source.read(count * row_size)
    if len(table) != count * row_size:
        return None
    for i, (_, absolute, actual_len) in enumerate(index["entries"]):
        row = table[i * row_size:(i + 1) * row_size]
        offset, declared = struct.unpack_from("<QQ", row)
        # The converter never uses the generic HFS0 last-sector tolerance:
        # the declared byte range must fit its containing partition as-is.
        if absolute != data_start + offset or actual_len != declared:
            return None
        if kind == "HFS0":
            hash_size = struct.unpack_from("<I", row, 20)[0]
            if hash_size > declared:
                return None
    return index


def restored_name(name: str) -> str:
    """Mirror Rust's NCZ -> NCA extension normalization, including case."""
    if name.lower().endswith(".ncz"):
        return name[:-1] + ("A" if name.endswith("Z") else "a")
    return name


def restored_names_unique(entries) -> bool:
    """No two output names may collide after valid input names are restored."""
    seen = set()
    for name, _, _ in entries:
        normalized = restored_name(name).lower()
        if normalized in seen:
            return False
        seen.add(normalized)
    return True


def check_ncz_headers(source, entries, counts):
    for name, start, size in entries:
        if not name.lower().endswith(".ncz"):
            continue
        counts["ncz_header_read"] += 1
        header = inspect_ncz_layout(source, start, size, counts)
        if header is None:
            counts["ncz_header_rejected"] += 1
            continue
        counts["ncz_header_valid"] += 1
        counts["ncz_encoding_" + header["encoding"]] += 1
        counts["ncz_sections"] += header["sections"]
        counts["ncz_block_count"] += header["blocks"]
        counts["ncz_expanded_bytes"] += header["expanded"]
        if header["plain_tail"]:
            counts["ncz_plain_tail_entries"] += 1
            counts["ncz_plain_tail_bytes"] += header["plain_tail"]


def synthetic_tests(mutations: int = 500):
    checks = 0
    for kind, row_size in (("PFS0", 24), ("HFS0", 64)):
        payload = make_fixture(kind, (("first.nca", b"abcd"), ("second.nca", b"efgh")))
        assert len(repacker_index(BytesIO(payload), 0, len(payload), kind)["entries"]) == 2
        checks += 1

        aliased = bytearray(payload)
        aliased[16 + row_size:16 + row_size + 8] = struct.pack("<Q", 2)
        assert repacker_index(BytesIO(aliased), 0, len(aliased), kind) is None
        checks += 1

        no_terminator = bytearray(payload)
        no_terminator[16 + 2 * row_size + len(b"first.nca\0second.nca")] = ord("X")
        assert repacker_index(BytesIO(no_terminator), 0, len(no_terminator), kind) is None
        checks += 1

        if kind == "HFS0":
            bad_hash = bytearray(payload)
            bad_hash[16 + 20:16 + 24] = struct.pack("<I", 5)
            assert repacker_index(BytesIO(bad_hash), 0, len(bad_hash), kind) is None
            checks += 1

    empty_hfs = make_fixture("HFS0", ())
    assert repacker_index(BytesIO(empty_hfs), 0, len(empty_hfs), "HFS0") == {
        "entries": [], "empty": True
    }
    assert repacker_index(BytesIO(empty_hfs), 0, len(empty_hfs), "PFS0") is None
    checks += 1

    # The read-only parser may tolerate a missing final game-card sector.
    # The repacker must not silently rebuild a potentially truncated ROM.
    missing_tail = make_fixture("HFS0", (("secure", b"data"),))
    missing_tail[24:32] = struct.pack("<Q", 4 + 411)
    assert parse_index(BytesIO(missing_tail), 0, len(missing_tail), kind="HFS0")
    assert repacker_index(BytesIO(missing_tail), 0, len(missing_tail), "HFS0") is None
    checks += 1

    # Two distinct source file names may become identical after .ncz -> .nca.
    # The input index remains valid, but the native output plan must reject it.
    for kind in ("PFS0", "HFS0"):
        for first, second in (("first.ncz", "first.nca"), ("first.NCZ", "FIRST.NCA")):
            collision = make_fixture(kind, ((first, b"abcd"), (second, b"efgh")))
            parsed = repacker_index(BytesIO(collision), 0, len(collision), kind)
            assert parsed is not None and len(parsed["entries"]) == 2
            assert not restored_names_unique(parsed["entries"])
            checks += 1
    assert restored_name("game.ncz") == "game.nca"
    assert restored_name("GAME.NCZ") == "GAME.NCA"
    assert restored_name("Game.NcZ") == "Game.NcA"
    assert restored_name("game.nca") == "game.nca"
    checks += 1

    # Independent NCZ header-only checks: no actual zstd decoding is involved.
    section = b"NCZSECTN" + struct.pack("<Q", 1)
    section += struct.pack("<QQQQ", 0x4000, 1024, 0, 0) + bytes(32)
    ncz_prefix = bytes(0x4000) + section
    solid = ncz_prefix + b"not-a-real-zstd-frame"
    assert inspect_ncz_layout(BytesIO(solid), 0, len(solid))["encoding"] == "solid"
    checks += 1
    block = ncz_prefix + b"NCZBLOCK" + struct.pack("<BBBBIQ", 1, 1, 0, 14, 1, 1024)
    block += struct.pack("<I", 500) + bytes(500)
    result = inspect_ncz_layout(BytesIO(block), 0, len(block))
    assert result and result["encoding"] == "block" and result["expanded"] == 0x4400
    checks += 1
    # Sections may describe only encrypted data; NCZBLOCK can contain an
    # additional plain tail. Its explicit full-size/table bounds are checked.
    plain_tail = ncz_prefix + b"NCZBLOCK" + struct.pack("<BBBBIQ", 1, 1, 0, 14, 1, 2048)
    plain_tail += struct.pack("<I", 2048) + bytes(2048)
    with_tail = inspect_ncz_layout(BytesIO(plain_tail), 0, len(plain_tail))
    assert with_tail and with_tail["expanded"] == 0x4800 and with_tail["plain_tail"] == 1024
    checks += 1
    too_short = bytearray(plain_tail)
    declared_at = len(ncz_prefix) + 8 + 4 + 4
    too_short[declared_at:declared_at + 8] = struct.pack("<Q", 512)
    assert inspect_ncz_layout(BytesIO(too_short), 0, len(too_short)) is None
    checks += 1
    overlap_header = bytearray(block)
    overlap_header[0x4010:0x4018] = struct.pack("<Q", 0xC00)
    overlap_header[0x4018:0x4020] = struct.pack("<Q", 1024 + 0x3400)
    overlap_layout = inspect_ncz_layout(BytesIO(overlap_header), 0, len(overlap_header))
    assert overlap_layout and overlap_layout["expanded"] == result["expanded"]
    checks += 1
    overlap_header[0x4010:0x4018] = struct.pack("<Q", 0x800)
    assert inspect_ncz_layout(BytesIO(overlap_header), 0, len(overlap_header)) is None
    checks += 1
    assert inspect_ncz_layout(BytesIO(block[:-1]), 0, len(block) - 1) is None
    checks += 1
    bad_block_size = bytearray(block)
    bad_block_size[len(ncz_prefix) + 8 + 16:len(ncz_prefix) + 8 + 20] = struct.pack("<I", 1025)
    assert inspect_ncz_layout(BytesIO(bad_block_size), 0, len(bad_block_size)) is None
    checks += 1
    bad_section = bytearray(solid)
    bad_section[0x4000 + 16 + 16:0x4000 + 16 + 24] = struct.pack("<Q", 9)
    assert inspect_ncz_layout(BytesIO(bad_section), 0, len(bad_section)) is None
    checks += 1
    bad_section_count = bytearray(solid)
    bad_section_count[0x4000 + 8:0x4000 + 16] = struct.pack("<Q", 32769)
    assert inspect_ncz_layout(BytesIO(bad_section_count), 0, len(bad_section_count)) is None
    checks += 1

    large_section_count = 4097
    long_index = bytearray(0x4000)
    long_index.extend(b"NCZSECTN" + struct.pack("<Q", large_section_count))
    for i in range(large_section_count):
        long_index.extend(struct.pack("<QQQQ", 0x4000 + i, 1, 0, 0))
        long_index.extend(bytes(32))  # synthetic key/counter placeholders
    long_index.extend(b"synthetic-solid-payload")
    large_layout = inspect_ncz_layout(BytesIO(long_index), 0, len(long_index))
    assert large_layout and large_layout["sections"] == large_section_count
    assert large_layout["expanded"] == 0x4000 + large_section_count
    checks += 1

    rng = random.Random(0x4E534C)
    for index in range(mutations):
        kind = "PFS0" if index % 2 == 0 else "HFS0"
        damaged = make_fixture(kind, (("first.nca", b"abcd"), ("second.ncz", b"123")))
        for _ in range(rng.randint(1, 5)):
            offset = rng.randrange(len(damaged))
            damaged[offset] ^= 1 << rng.randrange(8)
        decoded = repacker_index(BytesIO(damaged), 0, len(damaged), kind)
        if decoded:
            occupied = sorted((start, start + size) for _, start, size in decoded["entries"] if size)
            assert all(start >= 0 and stop <= len(damaged) for start, stop in occupied)
            assert all(a_end <= b_start for (_, a_end), (b_start, _) in zip(occupied, occupied[1:]))
        checks += 1
    return checks


def is_named_card_tail_only(source, start: int, limit: int) -> bool:
    """Diagnose a bounded tail-sector shortage without accepting the file.

    The read-only parser accepts HFS0's declared last sector with up to 512
    missing bytes; the converter deliberately refuses to write that source.
    Unrelated name/hash failures must never be misclassified as padding.
    """
    index = parse_index(source, start, limit, kind="HFS0", strict=True)
    if not index:
        return False
    source.seek(start)
    header = source.read(16)
    count, name_bytes = struct.unpack_from("<II", header, 4)
    if count > MAX_ENTRIES or name_bytes > MAX_NAMES:
        return False
    source.seek(start + 16)
    table = source.read(count * 64)
    if len(table) != count * 64:
        return False
    data_start = start + 16 + count * 64 + name_bytes
    clipped = 0
    for i, (name, absolute, read_size) in enumerate(index["entries"]):
        row = table[i * 64:(i + 1) * 64]
        offset, declared = struct.unpack_from("<QQ", row)
        hash_size = struct.unpack_from("<I", row, 20)[0]
        if absolute != data_start + offset or hash_size > read_size:
            return False
        if declared != read_size:
            if name.lower() not in ("secure", "normal", "update", "logo"):
                return False
            if declared - read_size > 0x200:
                return False
            clipped += 1
    return clipped == 1


def validate_library(db: Path, max_files: int, inspect_ncz: bool = False):
    if not db.is_file():
        return {"status": "skipped, library.db absent"}
    con = sqlite3.connect(db.resolve().as_uri() + "?mode=ro&immutable=1", uri=True)
    try:
        sql = ("SELECT path, lower(file_type) FROM game_files "
               "WHERE lower(file_type) IN ('nsz','xcz') ORDER BY size")
        if max_files:
            sql += f" LIMIT {max_files}"
        files = con.execute(sql).fetchall()
    finally:
        con.close()

    counts = collections.Counter()
    for filename, ext in files:
        path = Path(filename)
        if not path.is_file():
            counts["unavailable"] += 1
            continue
        before = path.stat()
        with path.open("rb") as source:
            if ext == "nsz":
                counts["nsz_read"] += 1
                index = repacker_index(source, 0, before.st_size, "PFS0")
                if not index or not index["entries"]:
                    counts["nsz_rejected"] += 1
                elif not restored_names_unique(index["entries"]):
                    counts["nsz_output_name_collision"] += 1
                    counts["nsz_rejected"] += 1
                else:
                    counts["nsz_valid"] += 1
                    counts["nsz_ncz_entries"] += sum(
                        name.lower().endswith(".ncz")
                        for name, _, _ in index["entries"]
                    )
                    if inspect_ncz:
                        check_ncz_headers(source, index["entries"], counts)
            else:
                counts["xcz_read"] += 1
                source.seek(0)
                header = source.read(0x200)
                if len(header) != 0x200 or header[0x100:0x104] != b"HEAD":
                    counts["xcz_rejected"] += 1
                    continue
                root_at = struct.unpack_from("<Q", header, 0x130)[0]
                if not (0x200 <= root_at < before.st_size and root_at <= 0x100000):
                    counts["xcz_rejected"] += 1
                    continue
                root = repacker_index(source, root_at, before.st_size, "HFS0")
                if root is None:
                    if is_named_card_tail_only(source, root_at, before.st_size):
                        counts["xcz_declared_tail_short"] += 1
                    else:
                        counts["xcz_rejected"] += 1
                    continue
                if not any(name.lower() == "secure" for name, _, _ in root["entries"]):
                    counts["xcz_rejected"] += 1
                    continue
                if not restored_names_unique(root["entries"]):
                    counts["xcz_output_name_collision"] += 1
                    counts["xcz_rejected"] += 1
                    continue
                counts["xcz_valid"] += 1
                for name, pos, length in root["entries"]:
                    if name.lower() not in ("secure", "update", "normal", "logo") or length < 16:
                        continue
                    source.seek(pos)
                    if source.read(4) != b"HFS0":
                        counts["xcz_nonhfs_partition"] += 1
                        continue
                    child = repacker_index(source, pos, pos + length, "HFS0")
                    if child is None:
                        counts["xcz_child_rejected"] += 1
                    elif not restored_names_unique(child["entries"]):
                        counts["xcz_child_output_name_collision"] += 1
                        counts["xcz_child_rejected"] += 1
                    elif child.get("empty", False):
                        counts["xcz_child_empty"] += 1
                    else:
                        counts["xcz_child_valid"] += 1
                        counts["xcz_child_entries"] += len(child["entries"])
                        counts["xcz_child_ncz_entries"] += sum(
                            name.lower().endswith(".ncz")
                            for name, _, _ in child["entries"]
                        )
                        if inspect_ncz:
                            check_ncz_headers(source, child["entries"], counts)
        after = path.stat()
        assert (before.st_size, before.st_mtime_ns) == (after.st_size, after.st_mtime_ns), (
            "Input changed during read-only inspection", ext
        )
    assert counts["nsz_rejected"] == 0, "Some existing NSZ indexes are no longer accepted"
    assert counts["xcz_rejected"] == 0, "Some existing XCZ roots are no longer accepted"
    assert counts["xcz_child_rejected"] == 0, "Some XCZ child indexes are no longer accepted"
    return {"status": "checked", **dict(sorted(counts.items()))}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", type=Path, default=HERE.parent / "library.db")
    parser.add_argument("--max-files", type=int, default=0)
    parser.add_argument("--require-corpus", action="store_true")
    parser.add_argument("--mutations", type=int, default=500)
    parser.add_argument("--inspect-ncz-headers", action="store_true",
                        help="read NCZ section/block metadata without decompressing contents")
    args = parser.parse_args()
    if args.max_files < 0:
        parser.error("--max-files must be nonnegative")
    if not 0 <= args.mutations <= 100000:
        parser.error("--mutations must be between 0 and 100000")
    corpus = validate_library(args.db, args.max_files, args.inspect_ncz_headers)
    if args.require_corpus and (
        corpus.get("status") != "checked"
        or corpus.get("nsz_read", 0) + corpus.get("xcz_read", 0) == 0
    ):
        parser.error("no available NSZ/XCZ corpus")
    print(json.dumps({
        "model": "independent native_package read-only input index QA (no Rust execution)",
        "synthetic_checks": synthetic_tests(args.mutations),
        "corpus": corpus,
    }, ensure_ascii=False, indent=2, sort_keys=True))
    if args.inspect_ncz_headers and corpus.get("ncz_header_rejected", 0):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
