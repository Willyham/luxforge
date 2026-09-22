#!/usr/bin/env python3
"""Bounded, read-only DNG/TIFF metadata inspector.

This is a developer probe.  It deliberately reports metadata and opcode
layouts; it never decodes pixels or applies corrections.  Unknown or malformed
metadata is reported in ``warnings`` rather than being assigned a default.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import struct
import sys
from pathlib import Path
from typing import Any

MAX_INPUT = 512 * 1024 * 1024
MAX_IFDS = 32
MAX_PAYLOAD = 1024 * 1024

TYPE_SIZES = {1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 6: 1, 7: 1, 8: 2,
              9: 4, 10: 8, 11: 4, 12: 8, 13: 4, 16: 8, 17: 1, 18: 8}
TYPE_NAMES = {1: "BYTE", 2: "ASCII", 3: "SHORT", 4: "LONG", 5: "RATIONAL",
              6: "SBYTE", 7: "UNDEFINED", 8: "SSHORT", 9: "SLONG",
              10: "SRATIONAL", 11: "FLOAT", 12: "DOUBLE", 13: "IFD",
              16: "LONG8", 17: "SLONG8", 18: "IFD8"}

TAGS = {
    254: "NewSubfileType", 256: "ImageWidth", 257: "ImageLength",
    258: "BitsPerSample", 259: "Compression", 262: "PhotometricInterpretation",
    273: "StripOffsets", 277: "SamplesPerPixel", 278: "RowsPerStrip",
    279: "StripByteCounts", 284: "PlanarConfiguration", 322: "TileWidth",
    323: "TileLength", 324: "TileOffsets", 325: "TileByteCounts",
    330: "SubIFDs", 33421: "CFARepeatPatternDim", 33422: "CFAPattern",
    50706: "DNGVersion", 50708: "UniqueCameraModel", 50718: "DefaultScale",
    50719: "DefaultCropOrigin", 50720: "DefaultCropSize", 50721: "ColorMatrix1",
    50722: "ColorMatrix2", 50723: "CameraCalibration1", 50724: "CameraCalibration2",
    50725: "ReductionMatrix1", 50726: "ReductionMatrix2", 50727: "AnalogBalance",
    50728: "AsShotNeutral", 50729: "AsShotWhiteXY", 50734: "LinearizationTable",
    50778: "CalibrationIlluminant1", 50779: "CalibrationIlluminant2",
    50780: "BestQualityScale", 50829: "ActiveArea", 50964: "NoiseProfile",
    51008: "OpcodeList1", 51009: "OpcodeList2", 51022: "OpcodeList3",
}

COLOR_TAGS = {50708, 50718, 50719, 50720, 50721, 50722, 50723, 50724,
              50725, 50726, 50727, 50728, 50778, 50779}


class Inspector:
    def __init__(self, data: bytes):
        self.data = data
        self.endian = "<"
        self.warnings: list[str] = []
        self.ifds: list[dict[str, Any]] = []
        self.seen: set[int] = set()

    def warn(self, message: str) -> None:
        if message not in self.warnings:
            self.warnings.append(message)

    def need(self, offset: int, size: int, label: str) -> None:
        if offset < 0 or size < 0 or offset > len(self.data) or size > len(self.data) - offset:
            raise ValueError(f"{label} exceeds input bounds")

    def unpack(self, fmt: str, offset: int) -> tuple[Any, ...]:
        size = struct.calcsize(self.endian + fmt)
        self.need(offset, size, "value")
        return struct.unpack_from(self.endian + fmt, self.data, offset)

    def values(self, typ: int, count: int, raw: bytes) -> Any:
        if typ == 2:
            return raw.split(b"\0", 1)[0].decode("ascii", "replace")
        if typ in (1, 6, 7, 17):
            return list(raw)
        fmts = {3: "H", 4: "I", 5: "II", 8: "h", 9: "i", 10: "ii",
                11: "f", 12: "d", 13: "I", 16: "Q", 18: "Q"}
        if typ not in fmts:
            self.warn(f"unknown TIFF type {typ}; raw value retained")
            return {"hex": raw.hex()}
        unit = struct.calcsize(self.endian + fmts[typ])
        if count * unit > len(raw):
            self.warn("short TIFF value; raw value retained")
            return {"hex": raw.hex()}
        out = []
        for i in range(count):
            parts = self.unpack(fmts[typ], 0) if False else struct.unpack_from(self.endian + fmts[typ], raw, i * unit)
            out.append(list(parts) if typ in (5, 10) else parts[0])
        return out[0] if count == 1 else out

    def entry(self, offset: int) -> dict[str, Any]:
        tag, typ, count = self.unpack("HHI", offset)
        if typ not in TYPE_SIZES:
            self.warn(f"tag {tag} uses unknown TIFF type {typ}")
            unit = 1
        else:
            unit = TYPE_SIZES[typ]
        total = unit * count
        if total > MAX_PAYLOAD:
            raise ValueError(f"tag {tag} payload exceeds {MAX_PAYLOAD} bytes")
        if total <= 4:
            raw = self.data[offset + 8:offset + 8 + total]
            value_offset = None
        else:
            value_offset = self.unpack("I", offset + 8)[0] if self.endian == "<" else self.unpack("I", offset + 8)[0]
            self.need(value_offset, total, f"tag {tag} payload")
            raw = self.data[value_offset:value_offset + total]
        item: dict[str, Any] = {"tag": tag, "name": TAGS.get(tag, f"Unknown-{tag}"),
                                "type": TYPE_NAMES.get(typ, f"UNKNOWN-{typ}"), "count": count,
                                "sha256": hashlib.sha256(raw).hexdigest(),
                                "values": self.values(typ, count, raw)}
        if value_offset is not None:
            item["offset"] = value_offset
        if tag not in TAGS:
            self.warn(f"unknown tag {tag} present in IFD")
        if tag in (51008, 51009, 51022):
            item["opcode_list"] = self.opcodes(raw, tag)
        return item

    def opcodes(self, raw: bytes, tag: int) -> dict[str, Any]:
        if len(raw) < 4:
            self.warn(f"OpcodeList{tag} is truncated")
            return {"count": None, "operations": []}
        count = struct.unpack_from(">I", raw, 0)[0]
        if count > 32:
            self.warn(f"OpcodeList{tag} count {count} exceeds 32")
            return {"count": count, "operations": []}
        pos, operations = 4, []
        for index in range(count):
            if pos + 16 > len(raw):
                self.warn(f"OpcodeList{tag} header {index} exceeds payload")
                break
            opcode, version, flags, byte_count = struct.unpack_from(">4I", raw, pos)
            start, end = pos + 16, pos + 16 + byte_count
            if byte_count > MAX_PAYLOAD or end > len(raw):
                self.warn(f"OpcodeList{tag} opcode {index} payload is out of bounds")
                break
            payload = raw[start:end]
            op: dict[str, Any] = {"index": index, "id": opcode, "version": version,
                                  "flags": flags, "byte_count": byte_count,
                                  "payload_sha256": hashlib.sha256(payload).hexdigest()}
            try:
                if opcode == 9:
                    op.update(self.gain_map(payload))
                elif opcode == 1:
                    op.update(self.warp(payload))
                else:
                    op["layout"] = "unknown opcode payload; correction not interpreted"
                    self.warn(f"unsupported opcode id {opcode} in OpcodeList{tag}")
            except (struct.error, ValueError) as exc:
                self.warn(f"opcode {opcode} in OpcodeList{tag} layout unknown: {exc}")
            operations.append(op)
            pos = end
        return {"count": count, "operations": operations}

    def gain_map(self, payload: bytes) -> dict[str, Any]:
        if len(payload) < 76:
            raise ValueError("GainMap header is truncated")
        area = struct.unpack_from(">8I", payload, 0)
        points_v, points_h = struct.unpack_from(">2I", payload, 32)
        spacing_v, spacing_h, origin_v, origin_h = struct.unpack_from(">4d", payload, 40)
        planes = struct.unpack_from(">I", payload, 72)[0]
        cells = points_v * points_h * planes
        if cells > MAX_PAYLOAD // 4 or 76 + cells * 4 > len(payload):
            raise ValueError("GainMap grid exceeds bounded payload")
        vals = struct.unpack_from(">" + "f" * cells, payload, 76)
        return {"layout": "GainMap", "area": list(area), "points": [points_v, points_h],
                "spacing": [spacing_v, spacing_h], "origin": [origin_v, origin_h],
                "planes": planes, "gain_min": min(vals), "gain_max": max(vals)}

    def warp(self, payload: bytes) -> dict[str, Any]:
        if len(payload) < 4:
            raise ValueError("WarpRectilinear header is truncated")
        planes = struct.unpack_from(">I", payload, 0)[0]
        need = 4 + planes * 48 + 16
        if planes > 64 or need > len(payload):
            raise ValueError("WarpRectilinear coefficients exceed bounded payload")
        coeffs = [list(struct.unpack_from(">6d", payload, 4 + 48 * i)) for i in range(planes)]
        center = list(struct.unpack_from(">2d", payload, 4 + 48 * planes))
        return {"layout": "WarpRectilinear", "planes": planes,
                "coefficients_sha256": hashlib.sha256(payload[4:4 + planes * 48]).hexdigest(),
                "center": center, "coefficient_finite": all(math.isfinite(v) for c in coeffs for v in c)}

    def walk(self, offset: int, role: str) -> None:
        if len(self.ifds) >= MAX_IFDS:
            self.warn(f"IFD traversal capped at {MAX_IFDS}")
            return
        if offset in self.seen:
            return
        self.seen.add(offset)
        self.need(offset, 2, "IFD entry count")
        count = self.unpack("H", offset)[0]
        if count > 4096:
            raise ValueError("IFD entry count exceeds bound")
        self.need(offset + 2, count * 12 + 4, "IFD entries")
        entries = [self.entry(offset + 2 + 12 * i) for i in range(count)]
        next_offset = self.unpack("I", offset + 2 + count * 12)[0]
        info: dict[str, Any] = {"offset": offset, "role": role, "entries": entries}
        by_tag = {x["tag"]: x for x in entries}
        for tag in (256, 257, 258, 259, 262, 277, 284, 33421, 33422, 50706, 50708):
            if tag in by_tag: info.setdefault("geometry", {})[TAGS[tag]] = by_tag[tag]["values"]
        for tag in COLOR_TAGS:
            if tag in by_tag: info.setdefault("color_calibration", {})[TAGS[tag]] = by_tag[tag]["values"]
        self.ifds.append(info)
        sub = by_tag.get(330)
        if sub:
            children = sub["values"] if isinstance(sub["values"], list) else [sub["values"]]
            for child in children: self.walk(int(child), "SubIFD")
        if next_offset: self.walk(next_offset, "next")

    def inspect(self) -> dict[str, Any]:
        if len(self.data) < 8: raise ValueError("input is shorter than TIFF header")
        marker = self.data[:2]
        if marker == b"II": self.endian = "<"
        elif marker == b"MM": self.endian = ">"
        else: raise ValueError("not a TIFF/DNG byte-order marker")
        magic = self.unpack("H", 2)[0]
        if magic != 42: self.warn(f"unexpected TIFF magic {magic}")
        root = self.unpack("I", 4)[0]
        self.walk(root, "root")
        sensor_ifds = [x for x in self.ifds
                       if x.get("geometry", {}).get("PhotometricInterpretation") == 32803]
        required = {50829: "ActiveArea", 50719: "DefaultCropOrigin", 50720: "DefaultCropSize",
                    50721: "ColorMatrix1", 50722: "ColorMatrix2", 50728: "AsShotNeutral"}
        for ifd in sensor_ifds:
            present = {e["tag"] for e in ifd["entries"]}
            for tag, name in required.items():
                if tag not in present:
                    self.warn(f"sensor IFD at {ifd['offset']} lacks {name}; value remains unknown")
        return {"format": "TIFF/DNG", "byte_order": "little" if self.endian == "<" else "big",
                "input_bytes": len(self.data), "ifd_count": len(self.ifds), "ifds": self.ifds,
                "warnings": self.warnings}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("input", type=Path)
    ap.add_argument("--json", type=Path, help="write diagnostic JSON to this path")
    args = ap.parse_args(argv)
    try:
        size = args.input.stat().st_size
        if size > MAX_INPUT: raise ValueError("input exceeds 512 MiB bound")
        result = Inspector(args.input.read_bytes()).inspect()
    except (OSError, ValueError, struct.error) as exc:
        print(json.dumps({"error": str(exc), "input": str(args.input)}, indent=2), file=sys.stderr)
        return 2
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.json: args.json.write_text(text, encoding="utf-8")
    else: print(text, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
