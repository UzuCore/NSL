"""Read-only NSL/NSRP ROM index regression, without compiling or launching NSL.

Runs synthetic corruption tests, then optionally compares an existing SQLite
library catalog to the strict and metadata-tolerant *format rules*. Never
reads keys, decrypts content, extracts ROMs, or modifies input files.

    python scripts/qa-rom-index-readonly.py
    python scripts/qa-rom-index-readonly.py --db D:\\somewhere\\library.db

This is an independent Python rule check, NOT execution of the Rust parser.
"""

from __future__ import annotations

import argparse
import collections
import json
import random
import sqlite3
import struct
import unicodedata
from bisect import bisect_left
from pathlib import Path

MAX_FILES = 16_384
MAX_STRINGS = 64 * 1024 * 1024
SCAN_OFFSETS = (0, 0x200, 0x400, 0x1000, 0x4000, 0x8000)
CARD_PARTITIONS = {"secure", "normal", "update", "logo"}


def parse_index(source, base: int, limit: int, kind=None, strict=True):
    """Mirror container_index.rs bounds and validated-entry selection."""
    if base < 0 or base + 16 > limit:
        return None
    source.seek(base)
    head = source.read(16)
    if len(head) != 16 or head[:4] not in (b"PFS0", b"HFS0"):
        return None
    actual_kind = head[:4].decode("ascii")
    if kind is not None and kind != actual_kind:
        return None
    count, string_size = struct.unpack_from("<II", head, 4)
    if not 1 <= count <= MAX_FILES or string_size > MAX_STRINGS:
        return None
    entry_size = 24 if actual_kind == "PFS0" else 64
    data_start = base + 16 + count * entry_size + string_size
    if data_start > limit:
        return None
    table = source.read(count * entry_size)
    strings = source.read(string_size)
    if len(table) != count * entry_size or len(strings) != string_size:
        return None

    entries = []
    seen = set()
    # Keep the validated nonempty extents ordered for overlap detection.
    occupied = []
    rejected = 0
    for i in range(count):
        row = table[i * entry_size:(i + 1) * entry_size]
        offset, length, name_at = struct.unpack_from("<QQI", row)
        name = None
        if name_at < string_size:
            name_end = strings.find(b"\0", name_at)
            if name_end != -1:
                try:
                    name = strings[name_at:name_end].decode("utf-8")
                except UnicodeDecodeError:
                    pass

        invalid = (
            not name or name in (".", "..")
            or any(c in "/\\:" or unicodedata.category(c) == "Cc" for c in name)
            or name.lower() in seen
        )
        start = data_start + offset
        end = start + length
        if not invalid and end > limit:
            # Only named HFS0 card partitions have limited final-sector slack.
            if (actual_kind == "HFS0" and name.lower() in CARD_PARTITIONS
                    and end - limit <= 0x200 and start < limit):
                length = limit - start
            else:
                invalid = True
        if not invalid and length > 0:
            position = bisect_left(occupied, (start, -1))
            if (position > 0 and occupied[position - 1][1] > start) or (
                position < len(occupied) and occupied[position][0] < start + length
            ):
                invalid = True
        if invalid:
            if strict:
                return None
            rejected += 1
            continue
        assert end <= limit or (actual_kind == "HFS0" and name.lower() in CARD_PARTITIONS)
        seen.add(name.lower())
        if length > 0:
            occupied.insert(bisect_left(occupied, (start, -1)), (start, start + length))
        entries.append((name, start, length))
    if not entries:
        return None
    return {"kind": actual_kind, "entries": entries, "rejected": rejected}


def make_fixture(kind="PFS0", entries=(("game.nca", b"game"),)):
    data = bytearray()
    table = bytearray()
    strings = bytearray()
    for name, payload in entries:
        table.extend(struct.pack("<QQII", len(data), len(payload), len(strings), 0))
        if kind == "HFS0":
            table.extend(bytes(40))
        strings.extend(name.encode("utf-8"))
        strings.append(0)
        data.extend(payload)
    return bytearray(kind.encode() + struct.pack("<III", len(entries), len(strings), 0)
                     + table + strings + data)


def synthetic_tests(mutation_count=500):
    from io import BytesIO

    def parse(bytes_, kind=None, strict=True, base=0, end=None):
        return parse_index(BytesIO(bytes_), base, len(bytes_) if end is None else end,
                           kind, strict)

    checks = 0
    base = make_fixture("PFS0", (("valid.cnmt.nca", b"good"), ("bad.nca", b"bad")))
    assert len(parse(base)["entries"]) == 2
    checks += 1

    partial = bytearray(base)
    partial[16 + 24 + 8:16 + 24 + 16] = struct.pack("<Q", 4096)
    assert parse(partial) is None
    salvage = parse(partial, strict=False)
    assert salvage and salvage["rejected"] == 1 and len(salvage["entries"]) == 1
    checks += 1

    for names in (("DUP.NCA", "dup.nca"), ("../file", "safe.nca"),
                  ("a:b", "safe.nca"), ("back\\slash", "safe.nca")):
        bad = make_fixture("PFS0", [(name, b"a") for name in names])
        assert parse(bad) is None
        checks += 1

    for index in (16, 24):
        corrupt = make_fixture()
        corrupt[index:index + 8] = struct.pack("<Q", (1 << 64) - 1)
        assert parse(corrupt) is None
        checks += 1

    hfs = make_fixture("HFS0", (("secure", b"12345678"),))
    hfs[24:32] = struct.pack("<Q", 8 + 512)
    assert parse(hfs, "HFS0")["entries"][0][2] == 8
    assert parse(hfs, "PFS0") is None
    checks += 1
    hfs[24:32] = struct.pack("<Q", 8 + 513)
    assert parse(hfs, "HFS0") is None
    checks += 1

    # Adjacent regions are valid, overlapping ones are not. In metadata mode
    # the first valid region survives and the aliased entry is discarded.
    for index_kind, entry_size in (("PFS0", 24), ("HFS0", 64)):
        two_files = make_fixture(index_kind, (("first.nca", b"abcd"), ("second.nca", b"efgh")))
        assert len(parse(two_files, index_kind)["entries"]) == 2
        two_files[16 + entry_size:16 + entry_size + 8] = struct.pack("<Q", 2)
        assert parse(two_files, index_kind) is None
        valid = parse(two_files, index_kind, strict=False)
        assert valid and valid["rejected"] == 1 and len(valid["entries"]) == 1
        checks += 1
    zero_length = make_fixture(
        "PFS0", (("empty-1", b""), ("empty-2", b""), ("data.nca", b"x"))
    )
    assert len(parse(zero_length, "PFS0")["entries"]) == 3
    checks += 1

    # Reject impossible table counts and string lengths before allocating a
    # table, plus a file name without a terminating NUL in its string table.
    for field_at, value in ((4, MAX_FILES + 1), (8, MAX_STRINGS + 1)):
        invalid = make_fixture()
        invalid[field_at:field_at + 4] = struct.pack("<I", value)
        assert parse(invalid) is None
        checks += 1
    no_nul = make_fixture()
    no_nul[16 + 24 + len("game.nca")] = ord("X")
    assert parse(no_nul) is None
    checks += 1

    # Nested PFS0 offset must stay inside its own 'secure' partition. A
    # malicious cross-partition entry would fit the complete XCI image, but
    # must not be visible in the secure partition's index.
    embedded = make_fixture("PFS0", (("icon.png", b"icon"),))
    outer = bytearray(b"\x00" * 0x400)
    outer.extend(make_fixture("HFS0", (("secure", embedded),)))
    outer.extend(b"\x00" * 0x1000)
    root = parse(outer, "HFS0", base=0x400)
    assert root is not None
    _, secure_start, secure_size = root["entries"][0]
    assert parse(outer, "PFS0", base=secure_start, end=secure_start + secure_size)
    embedded[24:32] = struct.pack("<Q", 0x400)
    outer = bytearray(b"\x00" * 0x400)
    outer.extend(make_fixture("HFS0", (("secure", embedded),)))
    outer.extend(b"\x00" * 0x1000)
    root = parse(outer, "HFS0", base=0x400)
    assert root is not None
    _, secure_start, secure_size = root["entries"][0]
    assert parse(outer, "PFS0", base=secure_start)
    assert parse(outer, "PFS0", base=secure_start, end=secure_start + secure_size) is None
    checks += 1

    # Deterministic mutations exercise invalid count/string size, table,
    # name and size offsets. The parser must never return an out-of-bounds
    # record or allocate based on an unchecked header.
    randomizer = random.Random(0x4E535250)
    for i in range(mutation_count):
        if i % 2:
            data = make_fixture("HFS0", (("secure", b"abcd"), ("normal", b"123")))
        else:
            data = make_fixture("PFS0", (("valid.nca", b"abcd"), ("two.ncz", b"123")))
        for _ in range(randomizer.randint(1, 5)):
            at = randomizer.randrange(len(data))
            data[at] ^= 1 << randomizer.randrange(8)
        result = parse(data, strict=randomizer.choice((True, False)))
        if result:
            for name, start, length in result["entries"]:
                assert start + length <= len(data)
                assert start >= 0
                assert length >= 0
                assert name and name not in (".", "..")
                assert not any(c in "/\\:" or unicodedata.category(c) == "Cc" for c in name)
        checks += 1
    return checks


def card_root(file, size):
    positions = []
    for header_base in (0x1000, 0, 0x200):
        if header_base + 0x138 > size:
            continue
        file.seek(header_base + 0x100)
        if file.read(4) != b"HEAD":
            continue
        file.seek(header_base + 0x130)
        address = struct.unpack("<Q", file.read(8))[0]
        if 0 < address < size:
            positions.append(address)
        if 0 < address * 512 < size:
            positions.append(address * 512)
    positions.extend((0x10000, 0xF000, 0x8000, 0x4000, 0x2000, 0x1000, 0x400, 0x200, 0))
    positions.extend(range(0x200, min(size, 0x20000), 0x200))
    for at in dict.fromkeys(positions):
        index = parse_index(file, at, size, kind="HFS0", strict=True)
        if index and any(name.lower() == "secure" for name, _, _ in index["entries"]):
            return index
    return None


def library_corpus(db: Path, max_files: int):
    if not db.is_file():
        return {"status": "skipped, library.db absent"}
    connection = sqlite3.connect(db.resolve().as_uri() + "?mode=ro&immutable=1", uri=True)
    try:
        query = ("SELECT path, lower(file_type) FROM game_files "
                 "WHERE lower(file_type) IN ('nsp','nsz','xci','xcz') ORDER BY size")
        if max_files:
            query += f" LIMIT {max_files:d}"
        rows = connection.execute(query).fetchall()
    finally:
        connection.close()

    counters = collections.Counter()
    for filename, extension in rows:
        path = Path(filename)
        if not path.is_file():
            counters["unavailable"] += 1
            continue
        stat = path.stat()
        with path.open("rb") as file:
            if extension in ("nsp", "nsz"):
                counters["package_read"] += 1
                strict = None
                salvage = None
                for base in SCAN_OFFSETS:
                    strict = parse_index(file, base, stat.st_size, kind="PFS0", strict=True)
                    salvage = parse_index(file, base, stat.st_size, kind="PFS0", strict=False)
                    if strict or salvage:
                        break
                if strict:
                    counters["package_strict"] += 1
                elif salvage:
                    counters["package_partial"] += 1
                    counters["package_rejected_entries"] += salvage["rejected"]
                else:
                    counters["package_unrecognized"] += 1
                if salvage:
                    counters["package_safe_entries"] += len(salvage["entries"])
            else:
                counters["card_read"] += 1
                root = card_root(file, stat.st_size)
                if not root:
                    counters["card_root_rejected"] += 1
                    continue
                counters["card_root_valid"] += 1
                for name, start, length in root["entries"]:
                    if name.lower() not in ("secure", "normal", "update"):
                        continue
                    if length <= 0x1000:
                        counters["card_small_partition"] += 1
                        continue
                    # Mirror the current Rust reader (file limit) and also
                    # check the stricter logical partition end separately.
                    global_index = parse_index(file, start, stat.st_size, strict=True)
                    local_index = parse_index(file, start, start + length, strict=True)
                    if global_index:
                        counters["card_partition_global_valid"] += 1
                        counters["card_partition_entries"] += len(global_index["entries"])
                    else:
                        counters["card_partition_global_rejected"] += 1
                    if local_index:
                        counters["card_partition_bounded_valid"] += 1
                        counters["card_partition_safe_entries"] += len(local_index["entries"])
                        for nested_name, _, _ in local_index["entries"]:
                            nested_lower = nested_name.lower()
                            if nested_lower.endswith((".cnmt.nca", ".cnmt.ncz")):
                                counters[f"card_cnmt_{name.lower()}"] += 1
                    else:
                        counters["card_partition_bounded_rejected"] += 1
        after = path.stat()
        assert (stat.st_size, stat.st_mtime_ns) == (after.st_size, after.st_mtime_ns), (
            "Source file changed during read-only QA", extension
        )
    assert counters["package_unrecognized"] == 0, "Unrecognized NSP/NSZ index"
    assert counters["card_root_rejected"] == 0, "Unrecognized XCI/XCZ root"
    return {"status": "checked", **dict(sorted(counters.items()))}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", type=Path, default=Path(__file__).resolve().parents[1] / "library.db")
    parser.add_argument("--max-files", type=int, default=0,
                        help="limit number of DB rows to inspect (0 = all)")
    parser.add_argument("--mutations", type=int, default=500,
                        help="deterministic synthetic corruptions (default 500; max 100000)")
    parser.add_argument("--require-corpus", action="store_true",
                        help="fail if the library database or real ROM samples are unavailable")
    args = parser.parse_args()
    if args.max_files < 0:
        parser.error("--max-files must be nonnegative")
    if not 0 <= args.mutations <= 100000:
        parser.error("--mutations must be between 0 and 100000")
    corpus = library_corpus(args.db, args.max_files)
    if args.require_corpus and (
        corpus.get("status") != "checked"
        or corpus.get("package_read", 0) + corpus.get("card_read", 0) == 0
    ):
        parser.error("a readable library database with available ROM files is required")
    result = {
        "script": "NSL PFS0/HFS0 read-only rules QA (not Rust execution)",
        "synthetic_checks": synthetic_tests(args.mutations),
        "corpus": corpus,
    }
    print(json.dumps(result, ensure_ascii=False, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
