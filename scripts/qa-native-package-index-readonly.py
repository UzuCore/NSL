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


def validate_library(db: Path, max_files: int):
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
                else:
                    counts["nsz_valid"] += 1
                    counts["nsz_ncz_entries"] += sum(
                        name.lower().endswith(".ncz")
                        for name, _, _ in index["entries"]
                    )
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
                    elif child.get("empty", False):
                        counts["xcz_child_empty"] += 1
                    else:
                        counts["xcz_child_valid"] += 1
                        counts["xcz_child_entries"] += len(child["entries"])
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
    args = parser.parse_args()
    if args.max_files < 0:
        parser.error("--max-files must be nonnegative")
    if not 0 <= args.mutations <= 100000:
        parser.error("--mutations must be between 0 and 100000")
    corpus = validate_library(args.db, args.max_files)
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


if __name__ == "__main__":
    main()
