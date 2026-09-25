#!/usr/bin/env python3
"""Independent DNG 1.4 GainMap/WarpRectilinear reference probe.

This deliberately has no Luxforge or Adobe runtime dependency.  It parses the
little-endian TIFF container and the big-endian opcode payloads used by the
supplied DJI Air 2S DNG, then evaluates the reference equations in float64.
"""

from __future__ import annotations

import argparse
import csv
import json
import math
import struct
from pathlib import Path


def _u32(buf: bytes, offset: int, endian: str = ">") -> int:
    return struct.unpack_from(endian + "I", buf, offset)[0]


def _f32(buf: bytes, offset: int) -> float:
    return struct.unpack_from(">f", buf, offset)[0]


def _f64(buf: bytes, offset: int) -> float:
    return struct.unpack_from(">d", buf, offset)[0]


def parse_opcode_list3(buf: bytes, offset: int, count: int) -> list[dict]:
    """Decode the DNG opcode list; numeric payload fields are big-endian."""
    opcodes = []
    pos = offset
    for index in range(count):
        opcode, version, flags, byte_count = struct.unpack_from(">IIII", buf, pos)
        payload = pos + 16
        end = payload + byte_count
        if end > len(buf):
            raise ValueError("opcode payload exceeds input")
        if opcode == 9:  # GainMap
            area = struct.unpack_from(">8I", buf, payload)
            p = payload + 32
            points_v, points_h = struct.unpack_from(">2I", buf, p)
            spacing_v, spacing_h, origin_v, origin_h = struct.unpack_from(">4d", buf, p + 8)
            planes = _u32(buf, p + 40)
            values = []
            p += 44
            for row in range(points_v):
                values.append([
                    [_f32(buf, p + 4 * (plane + planes * (col + points_h * row)))
                     for plane in range(planes)]
                    for col in range(points_h)
                ])
            opcodes.append({
                "index": index, "id": opcode, "version": version, "flags": flags,
                "byte_count": byte_count, "area": area,
                "points": [points_v, points_h], "spacing": [spacing_v, spacing_h],
                "origin": [origin_v, origin_h], "planes": planes, "values": values,
            })
        elif opcode == 1:  # WarpRectilinear
            planes = _u32(buf, payload)
            p = payload + 4
            coeffs = []
            for _ in range(planes):
                coeffs.append([_f64(buf, p + 8 * j) for j in range(6)])
                p += 48
            center = [_f64(buf, p), _f64(buf, p + 8)]
            opcodes.append({
                "index": index, "id": opcode, "version": version, "flags": flags,
                "byte_count": byte_count, "planes": planes, "coefficients": coeffs,
                "center": center,
            })
        else:
            opcodes.append({
                "index": index, "id": opcode, "version": version, "flags": flags,
                "byte_count": byte_count,
            })
        pos = end
    return opcodes


def _ifd_entry_data(buf: bytes, entry: int, sizes: dict[int, int]) -> tuple[int, int, int, bytes]:
    tag, typ, count = struct.unpack_from("<HHI", buf, entry)
    raw = struct.unpack_from("<I", buf, entry + 8)[0]
    total = sizes[typ] * count
    data = buf[entry + 8:entry + 8 + total] if total <= 4 else buf[raw:raw + total]
    return tag, typ, count, data


def _metadata_color_stage(buf: bytes) -> dict:
    """Read camera WB/profile tags from IFD0 and optional profile IFDs.

    These values describe the stage after the opcode list: AsShotNeutral is
    the camera-space white-balance reference and ColorMatrix1/2 are the two
    XYZ-to-camera matrices selected by CalibrationIlluminant1/2.  They are
    emitted as rational pairs to avoid baking an implementation's rounding
    into the reference fixture.
    """
    sizes = {1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 7: 1, 10: 8, 11: 4, 12: 8}
    root = 8
    root_count = struct.unpack_from("<H", buf, root)[0]
    sub_ifds: list[int] = []
    for i in range(root_count):
        tag, typ, count, data = _ifd_entry_data(buf, root + 2 + 12 * i, sizes)
        if tag == 330 and typ == 4:
            sub_ifds.extend(struct.unpack_from("<" + "I" * count, data))

    wanted = {50708, 50721, 50722, 50727, 50728, 50778, 50779}
    fields: dict[int, tuple[int, int, bytes]] = {}
    # Profile and white-balance tags in the supplied file are in IFD0; scan
    # SubIFDs as well because the DNG specification permits Camera Profile IFD
    # placement for the color matrices.
    for ifd in [root, *sub_ifds]:
        n = struct.unpack_from("<H", buf, ifd)[0]
        for i in range(n):
            entry = ifd + 2 + 12 * i
            tag, typ, count, data = _ifd_entry_data(buf, entry, sizes)
            if tag in wanted:
                fields[tag] = (typ, count, data)

    def rationals(tag: int, signed: bool) -> list[list[int]]:
        typ, count, data = fields[tag]
        if typ not in (5, 10):
            raise ValueError(f"tag {tag} is not a rational")
        fmt = "<ii" if signed else "<II"
        return [list(struct.unpack_from(fmt, data, 8 * i)) for i in range(count)]

    as_shot = rationals(50728, signed=False)
    as_shot_values = _rational_values(as_shot)
    result = {
        "unique_camera_model": fields[50708][2].split(b"\0", 1)[0].decode("ascii"),
        "calibration_illuminants": [
            struct.unpack_from("<H", fields[50778][2])[0],
            struct.unpack_from("<H", fields[50779][2])[0],
        ],
        "analog_balance": rationals(50727, signed=False),
        "as_shot_neutral": as_shot,
        "as_shot_neutral_values": as_shot_values,
        "as_shot_green_normalized_sensor_gains": [
            as_shot_values[1] / as_shot_values[0], 1.0,
            as_shot_values[1] / as_shot_values[2],
        ],
        "color_matrix1": rationals(50721, signed=True),
        "color_matrix2": rationals(50722, signed=True),
        "ordering": "opcode list 3 -> camera-space WB (AsShotNeutral) -> inverse XYZ-to-camera matrix to XYZ",
    }
    return result


def _rational_values(values: list[list[int]]) -> list[float]:
    return [numerator / denominator for numerator, denominator in values]


def _base_whitepoint_xy(temperature_kelvin: float) -> tuple[float, float]:
    """The documented Luxforge Planck/daylight locus used by the WB solver."""
    t = temperature_kelvin
    if t <= 4_000.0:
        x = -0.2661239e9 / t**3 - 0.2343580e6 / t**2 + 0.8776956e3 / t + 0.179910
    else:
        x = -3.0258469e9 / t**3 + 2.1070379e6 / t**2 + 0.2226347e3 / t + 0.240390
    if t <= 2_222.0:
        y = -1.1063814 * x**3 - 1.3481102 * x**2 + 2.18555832 * x - 0.20219683
    elif t <= 4_000.0:
        y = -0.9549476 * x**3 - 1.37418593 * x**2 + 2.09137015 * x - 0.16748867
    else:
        y = 3.0817580 * x**3 - 5.87338670 * x**2 + 3.75112997 * x - 0.37001483
    if t <= 7_000.0:
        daylight_x = 0.244063 + 0.09911e3 / t + 2.9678e6 / t**2 - 4.6070e9 / t**3
    else:
        daylight_x = 0.237040 + 0.24748e3 / t + 1.9018e6 / t**2 - 2.0064e9 / t**3
    daylight = (daylight_x, -3.0 * daylight_x**2 + 2.870 * daylight_x - 0.275)
    planck = (x, y)
    blend = max(0.0, min(1.0, (t - 3_800.0) / 700.0))
    blend = blend * blend * (3.0 - 2.0 * blend)
    return tuple(planck[i] + (daylight[i] - planck[i]) * blend for i in range(2))


def _xy_to_uv(xy: tuple[float, float]) -> tuple[float, float]:
    x, y = xy
    d = -2.0 * x + 12.0 * y + 3.0
    return 4.0 * x / d, 6.0 * y / d


def _uv_to_xy(uv: tuple[float, float]) -> tuple[float, float]:
    u, v = uv
    d = 2.0 * u - 8.0 * v + 4.0
    return 3.0 * u / d, 2.0 * v / d


def _tinted_whitepoint_xy(temperature_kelvin: float, tint: float) -> tuple[float, float]:
    uv = _xy_to_uv(_base_whitepoint_xy(temperature_kelvin))
    lo = _xy_to_uv(_base_whitepoint_xy(max(2_000.0, temperature_kelvin - 1.0)))
    hi = _xy_to_uv(_base_whitepoint_xy(min(12_000.0, temperature_kelvin + 1.0)))
    tangent = (hi[0] - lo[0], hi[1] - lo[1])
    length = math.hypot(*tangent)
    normal = (tangent[1] / length, -tangent[0] / length)
    return _uv_to_xy((uv[0] + normal[0] * tint * 1.0e-4,
                      uv[1] + normal[1] * tint * 1.0e-4))


def dng_wb_reference(color_stage: dict, temperature_kelvin: float,
                     tint: float) -> dict:
    """Resolve one independent 3-channel WB reference from DNG profile tags."""
    cct1, cct2 = 2_856.0, 6_504.0  # Standard Light A and D65, respectively.
    weight2 = max(0.0, min(1.0,
                           (1.0 / temperature_kelvin - 1.0 / cct1) /
                           (1.0 / cct2 - 1.0 / cct1)))
    cm1 = _rational_values(color_stage["color_matrix1"])
    cm2 = _rational_values(color_stage["color_matrix2"])
    matrix = [a + (b - a) * weight2 for a, b in zip(cm1, cm2)]
    analog = _rational_values(color_stage["analog_balance"])
    matrix = [matrix[i * 3 + j] * analog[i]
              for i in range(3) for j in range(3)]
    x, y = _tinted_whitepoint_xy(temperature_kelvin, tint)
    xyz = (x / y, 1.0, (1.0 - x - y) / y)
    response = [sum(matrix[i * 3 + j] * xyz[j] for j in range(3))
                for i in range(3)]
    gains = [response[1] / response[0], 1.0, response[1] / response[2]]
    return {
        "production_reference": False,
        "selection": "inverse-CCT dual-calibration interpolation comparator",
        "temperature_kelvin": temperature_kelvin,
        "tint_luxforge_units": tint,
        "calibration_illuminants_cct_kelvin": [cct1, cct2],
        "matrix2_inverse_cct_weight": weight2,
        "whitepoint_xy": [x, y],
        "whitepoint_xyz_y1": list(xyz),
        "xyz_to_camera_matrix": [matrix[0:3], matrix[3:6], matrix[6:9]],
        "camera_white_response": response,
        "green_normalized_sensor_gains": gains,
    }


def dng_wb_fixed_cm2_reference(color_stage: dict, temperature_kelvin: float,
                               tint: float) -> dict:
    """Resolve the production FC3411 policy: fixed D65 ColorMatrix2."""
    matrix = _rational_values(color_stage["color_matrix2"])
    analog = _rational_values(color_stage["analog_balance"])
    matrix = [matrix[i * 3 + j] * analog[i]
              for i in range(3) for j in range(3)]
    x, y = _tinted_whitepoint_xy(temperature_kelvin, tint)
    xyz = (x / y, 1.0, (1.0 - x - y) / y)
    response = [sum(matrix[i * 3 + j] * xyz[j] for j in range(3))
                for i in range(3)]
    gains = [response[1] / response[0], 1.0, response[1] / response[2]]
    valid = all(math.isfinite(value) and 0.0 < value <= 32.0 for value in gains)
    return {
        "production_reference": True,
        "selection": "fixed ColorMatrix2 D65 XYZ-to-camera",
        "temperature_kelvin": temperature_kelvin,
        "tint_luxforge_units": tint,
        "whitepoint_xy": [x, y],
        "whitepoint_xyz_y1": list(xyz),
        "xyz_to_camera_matrix": [matrix[0:3], matrix[3:6], matrix[6:9]],
        "camera_white_response": response,
        "green_normalized_sensor_gains": gains,
        "valid_for_luxforge_gain_contract": valid,
        "validation_reason": ("within positive finite 0..32 gain range" if valid
                              else "rejected: a sensor gain exceeds the 0..32 contract"),
    }


def gain_interpolate(gain: dict, row: int, col: int, plane: int,
                     bounds: tuple[int, int, int, int]) -> float:
    """DNG SDK interpolation: bounds-relative pixel centers, edge replication."""
    top, left, bottom, right = bounds
    height = bottom - top
    width = right - left
    points_v, points_h = gain["points"]
    spacing_v, spacing_h = gain["spacing"]
    origin_v, origin_h = gain["origin"]
    plane = min(plane, gain["planes"] - 1)

    def axis(pixel: int, image_origin: int, extent: int, points: int,
             spacing: float, origin: float):
        # The +0.5 is the pixel-centre convention.  Keep the integer image
        # origin explicit: height and width may happen to be equal.
        x = ((pixel + 0.5 - image_origin) / extent - origin) / spacing
        if x <= 0.0:
            return 0, 0, 0.0
        last = points - 1
        if x >= last:
            return last, last, 0.0
        lo = math.floor(x)
        return lo, lo + 1, x - lo

    r0, r1, rf = axis(row, top, height, points_v, spacing_v, origin_v)
    c0, c1, cf = axis(col, left, width, points_h, spacing_h, origin_h)
    v = gain["values"]
    a = v[r0][c0][plane] * (1.0 - rf) + v[r1][c0][plane] * rf
    b = v[r0][c1][plane] * (1.0 - rf) + v[r1][c1][plane] * rf
    return a * (1.0 - cf) + b * cf


def warp_source_position(params: dict, row: float, col: float,
                         bounds: tuple[int, int, int, int],
                         pixel_scale_v: float = 1.0,
                         plane: int = 0) -> tuple[float, float]:
    """Adobe SDK GetSrcPixelPosition for WarpRectilinear (row, column)."""
    top, left, bottom, right = bounds
    center_v = top + (bottom - top) * params["center"][1]
    center_h = left + (right - left) * params["center"][0]
    square_bottom = top + round(pixel_scale_v * (bottom - top))
    square_center_v = top + (square_bottom - top) * params["center"][1]
    square_center_h = left + (right - left) * params["center"][0]
    max_dv = max(abs(square_center_v - top), abs(square_center_v - square_bottom))
    max_dh = max(abs(square_center_h - left), abs(square_center_h - right))
    norm_radius = math.hypot(max_dv, max_dh)
    dv, dh = row - center_v, col - center_h
    nv, nh = dv / norm_radius, dh / norm_radius
    nvs, nhs = nv * pixel_scale_v, nh
    rr = min(nvs * nvs + nhs * nhs, 1.0)
    kr0, kr1, kr2, kr3, kt0, kt1 = params["coefficients"][min(plane, params["planes"] - 1)]
    ratio = kr0 + rr * (kr1 + rr * (kr2 + rr * kr3))
    tan_h = kt0 * (2.0 * nvs * nhs) + kt1 * (rr + 2.0 * nhs * nhs)
    tan_v = kt0 * (rr + 2.0 * nvs * nvs) + kt1 * (2.0 * nvs * nhs)
    src_h = norm_radius * (nh * ratio + tan_h)
    src_v = norm_radius * (nv * ratio + tan_v / pixel_scale_v)
    return center_v + src_v, center_h + src_h


def warp_source_position_exact_identity(params: dict, row: float, col: float,
                                        bounds: tuple[int, int, int, int],
                                        pixel_scale_v: float = 1.0,
                                        plane: int = 0) -> tuple[float, float]:
    """Return the production exact identity for a coefficient-set NOP.

    Evaluating the general normalized polynomial can turn an identity plane's
    exact coordinate into 3.9999999999997726 at an edge.  The Adobe equation
    is mathematically identity for kr0=1 and all other terms zero; the adapter
    intentionally recognizes that plane and skips resampling.
    """
    coeff = params["coefficients"][min(plane, params["planes"] - 1)]
    if coeff == [1.0, 0.0, 0.0, 0.0, 0.0, 0.0]:
        return row, col
    return warp_source_position(params, row, col, bounds, pixel_scale_v, plane)


def warp_source_position_spec_endpoints(params: dict, row: float, col: float,
                                        bounds: tuple[int, int, int, int],
                                        pixel_scale_v: float = 1.0,
                                        plane: int = 0) -> tuple[float, float]:
    """Literal DNG-spec variant using x1/y1 as bottom-right pixel coordinates.

    The Adobe SDK implementation uses dng_rect.r/b (exclusive bounds), while
    the DNG 1.7.1 prose calls x1/y1 the bottom-right pixel.  Keeping this
    variant beside the SDK reference makes the small convention difference
    reviewable without silently choosing one interpretation.
    """
    top, left, bottom, right = bounds
    last_v, last_h = bottom - 1, right - 1
    center_v = top + (last_v - top) * params["center"][1]
    center_h = left + (last_h - left) * params["center"][0]
    square_last_v = top + round(pixel_scale_v * (last_v - top))
    square_center_v = top + (square_last_v - top) * params["center"][1]
    square_center_h = left + (last_h - left) * params["center"][0]
    max_dv = max(abs(square_center_v - top), abs(square_center_v - square_last_v))
    max_dh = max(abs(square_center_h - left), abs(square_center_h - last_h))
    norm_radius = math.hypot(max_dv, max_dh)
    dv, dh = row - center_v, col - center_h
    nv, nh = dv / norm_radius, dh / norm_radius
    nvs, nhs = nv * pixel_scale_v, nh
    rr = min(nvs * nvs + nhs * nhs, 1.0)
    kr0, kr1, kr2, kr3, kt0, kt1 = params["coefficients"][min(plane, params["planes"] - 1)]
    ratio = kr0 + rr * (kr1 + rr * (kr2 + rr * kr3))
    tan_h = kt0 * (2.0 * nvs * nhs) + kt1 * (rr + 2.0 * nhs * nhs)
    tan_v = kt0 * (rr + 2.0 * nvs * nvs) + kt1 * (2.0 * nvs * nhs)
    return (center_v + norm_radius * (nv * ratio + tan_v / pixel_scale_v),
            center_h + norm_radius * (nh * ratio + tan_h))


def _float32(value: float) -> float:
    return struct.unpack("<f", struct.pack("<f", value))[0]


def _bicubic_kernel(value: float) -> float:
    a = -0.75
    x = abs(value)
    if x >= 2.0:
        return 0.0
    if x >= 1.0:
        return ((a * x - 5.0 * a) * x + 8.0 * a) * x - 4.0 * a
    return ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0


def _sdk_bicubic_weights(phase_v: int, phase_h: int) -> list[float]:
    """Float32 4x4 weights from dng_resample_weights_2d, phase 0..127."""
    fv, fh = phase_v / 128.0, phase_h / 128.0
    values: list[float] = []
    total = 0.0
    for j in range(-1, 3):
        for i in range(-1, 3):
            value = _float32(_bicubic_kernel(i - fh) * _bicubic_kernel(j - fv))
            values.append(value)
            total += value
    scale = _float32(1.0 / total)
    return [_float32(value * scale) for value in values]


def sparse_reference(path: Path, gain: dict, warp: dict,
                     active_area: list[int]) -> dict:
    """Compare independent SDK-equation output with an ignored 8x8 sparse dump."""
    rows = list(csv.DictReader(path.open(newline="")))
    groups: dict[tuple[int, int, int], list[dict]] = {}
    expected: dict[tuple[int, int, int], float] = {}
    mapped: dict[tuple[int, int, int], dict] = {}
    for item in rows:
        key = (int(item["out_x"]), int(item["out_y"]), int(item["channel"]))
        if item["kind"] == "input":
            groups.setdefault(key, []).append(item)
        elif item["kind"] == "result":
            expected[key] = float(item["value"])
        elif item["kind"] == "mapped":
            mapped[key] = item

    top, left, bottom, right = active_area
    bounds = (0, 0, bottom - top, right - left)
    output = []
    for key in sorted(expected):
        out_x, out_y, plane = key
        taps = {(int(x["sensor_x"]), int(x["sensor_y"])): _float32(float(x["value"]))
                for x in groups[key]}
        src_v, src_h = warp_source_position_exact_identity(
            warp, out_y - top, out_x - left, bounds, plane=plane)
        base_v, base_h = math.floor(src_v), math.floor(src_h)
        phase_v = math.floor((src_v - base_v) * 128.0)
        phase_h = math.floor((src_h - base_h) * 128.0)
        weights = _sdk_bicubic_weights(phase_v, phase_h)
        totals = {"float_headroom": _float32(0.0), "strict_dng_clipped": _float32(0.0)}
        for n, (j, i) in enumerate(( (j, i) for j in range(-1, 3) for i in range(-1, 3) )):
            # Stage 3 is the active-area image, so source edge replication
            # clamps to the active raw rectangle rather than the sensor's
            # hidden left/top margins.
            raw_x = min(right - 1, max(left, left + int(base_h + i)))
            raw_y = min(bottom - 1, max(top, top + int(base_v + j)))
            value = taps[(raw_x, raw_y)]
            map_gain = gain_interpolate(gain, raw_y - top, raw_x - left, plane, bounds)
            corrected = _float32(value * map_gain)
            for name, sample in (("float_headroom", corrected),
                                 ("strict_dng_clipped", max(0.0, min(1.0, corrected)))):
                totals[name] = _float32(totals[name] + _float32(weights[n] * sample))
        production = expected[key]
        mapped_row = mapped[key]
        mapped_raw_xy = [float(mapped_row["sensor_x"]), float(mapped_row["sensor_y"])]
        reference_raw_xy = [src_h + left, src_v + top]
        output.append({
            "out_raw_xy": [out_x, out_y],
            "plane": plane,
            "source_raw_yx": [src_v + top, src_h + left],
            "source_raw_xy": reference_raw_xy,
            "mapped_csv_source_raw_xy": mapped_raw_xy,
            "max_abs_error_to_mapped_csv_pixels": max(
                abs(reference_raw_xy[i] - mapped_raw_xy[i]) for i in (0, 1)),
            "source_active_local_yx": [src_v, src_h],
            "phase_128_yx": [phase_v, phase_h],
            "reference_float_headroom": totals["float_headroom"],
            "reference_strict_dng_clipped": min(1.0, max(0.0, totals["strict_dng_clipped"])),
            "production_sparse_csv_value": production,
            "abs_error_to_csv_value": abs(totals["float_headroom"] - production),
        })
    return {
        "provenance": "independent f32 GainMap + Adobe SDK bicubic phase-128 evaluation of an ignored 8x8 sparse adapter dump",
        "count": len(output),
        "max_abs_error_to_csv_value": max(x["abs_error_to_csv_value"] for x in output),
        "max_abs_error_to_mapped_csv_pixels": max(
            x["max_abs_error_to_mapped_csv_pixels"] for x in output),
        "samples": output,
    }


def parse_dng(path: Path) -> dict:
    buf = path.read_bytes()
    # The supplied file's raw IFD is the first SubIFD (offset 728).
    raw_ifd = 728
    entries = _u16 = struct.unpack_from("<H", buf, raw_ifd)[0]
    opcode_offset = None
    opcode_count = None
    fields = {}
    sizes = {1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 7: 1, 11: 4, 12: 8}
    for i in range(entries):
        p = raw_ifd + 2 + 12 * i
        tag, typ, count = struct.unpack_from("<HHI", buf, p)
        raw = struct.unpack_from("<I", buf, p + 8)[0]
        total = sizes[typ] * count
        data = (buf[p + 8:p + 8 + total] if total <= 4 else buf[raw:raw + total])
        if tag == 51022:
            opcode_count = count
            opcode_offset = raw if total > 4 else raw
        if tag in (256, 257, 50718, 50719, 50720, 50780, 50829):
            if typ == 4:
                fields[tag] = struct.unpack_from("<" + "I" * count, data)
            elif typ == 5:
                fields[tag] = [struct.unpack_from("<II", data, 8 * j) for j in range(count)]
    if opcode_offset is None or opcode_count is None:
        raise ValueError("Raw IFD lacks OpcodeList3 (51022)")
    return {
        "source_file": path.name, "file_bytes": len(buf), "raw_ifd": raw_ifd,
        "opcode_list3_offset": opcode_offset, "opcode_list3_bytes": opcode_count,
        "raw_fields": {str(k): v for k, v in fields.items()},
        "color_stage": _metadata_color_stage(buf),
        "opcode_count": _u32(buf, opcode_offset),
        "opcodes": parse_opcode_list3(buf, opcode_offset + 4, _u32(buf, opcode_offset)),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("path", type=Path)
    parser.add_argument("--json", type=Path)
    parser.add_argument("--sparse", type=Path,
                        help="compare an ignored 8x8 sparse corrected-pixel dump")
    parser.add_argument("--full", action="store_true",
                        help="include all 32x32x3 gain-map values in JSON")
    args = parser.parse_args()
    result = parse_dng(args.path)
    gain = next(x for x in result["opcodes"] if x["id"] == 9)
    warp = next(x for x in result["opcodes"] if x["id"] == 1)
    # Opcode area bounds are in raw-image pixel coordinates.  They are not the
    # DefaultCropOrigin/DefaultCropSize (which are intentionally also emitted
    # above for callers that need final-image framing).
    bounds = tuple(int(x) for x in gain["area"][:4])
    synthetic_headroom_source = [
        -0.25, 0.0, 0.0, 0.0,
        0.0, 0.2, 0.8, 0.0,
        0.0, 0.8, 1.2, 0.0,
        0.0, 0.0, 0.0, 1.5,
    ]
    synthetic_headroom_weights = _sdk_bicubic_weights(64, 64)
    synthetic_unclipped = sum(
        weight * value for weight, value in zip(
            synthetic_headroom_weights, synthetic_headroom_source))
    synthetic_strict = min(1.0, max(0.0, sum(
        weight * max(0.0, min(1.0, value)) for weight, value in zip(
            synthetic_headroom_weights, synthetic_headroom_source))))
    # Three per-plane samples demonstrate plane clamping and nontrivial warp.
    reference_samples = {
        "gain_grid": [{"map_row": r, "map_col": c,
                       "value": gain["values"][r][c]}
                      for r, c in [(0, 0), (0, 31), (16, 16), (31, 0), (31, 31)]],
        "gain": [{"row": r, "col": c, "plane": p,
                  "value": gain_interpolate(gain, r, c, p, bounds)}
                 for r, c, p in [(0, 0, 0), (1824, 2736, 1), (3647, 5471, 2), (100, 5000, 3)]],
        # Negative coordinates exercise map-edge replication.  They are
        # outside this opcode's affected area and are therefore probe-only.
        "gain_edge_replication": [{"row": r, "col": c, "plane": p,
                                    "value": gain_interpolate(gain, r, c, p, bounds)}
                                   for r, c, p in [(-1, -1, 0), (3648, 5472, 2)]],
        "warp_source": [{"row": r, "col": c, "plane": p,
                         "value": warp_source_position(warp, r, c, bounds,
                                                        pixel_scale_v=1.0, plane=p)}
                        for r, c, p in [(0, 0, 0), (1000, 1000, 0),
                                        (1000, 1000, 1), (1000, 1000, 2),
                                        (1824, 2736, 1), (3647, 5471, 2)]],
        # The supplied file has square pixels.  Keep one non-unit vector in
        # the fixture so implementations cannot accidentally ignore the SDK's
        # PixelAspectRatio adjustment for other DNGs.
        "warp_nonunit_pixel_scale": {
            "pixel_scale_v": 1.25,
            "row": 1000,
            "col": 1000,
            "plane": 0,
            "value": warp_source_position(warp, 1000, 1000, bounds,
                                            pixel_scale_v=1.25, plane=0),
        },
        # A synthetic non-central centre catches accidental x/y swaps in the
        # centre convention.  The authentic file uses [0.5, 0.5], where that
        # mistake is hidden by symmetry.
        "warp_noncentral_center": {
            "center_xy": [0.25, 0.75],
            "pixel_scale_v": 1.0,
            "row": 1000,
            "col": 1000,
            "plane": 0,
            "value": warp_source_position(
                {**warp, "center": [0.25, 0.75]}, 1000, 1000, bounds,
                pixel_scale_v=1.0, plane=0),
        },
        "warp_identity_exactness": {
            "policy": "identity coefficient planes return destination coordinates exactly and skip resampling",
            "samples": [{"row": r, "col": c, "plane": 1,
                         "value": warp_source_position_exact_identity(
                             warp, r, c, bounds, pixel_scale_v=1.0, plane=1)}
                        for r, c in [(4, 4), (1000, 1000), (3643, 5467)]],
        },
        "warp_spec_literal_endpoints": {
            "convention": "x1/y1 are bottom-right pixel coordinates (DNG 1.7.1 prose)",
            "samples": [{"row": r, "col": c, "plane": p,
                         "value": warp_source_position_spec_endpoints(
                             warp, r, c, bounds, pixel_scale_v=1.0, plane=p)}
                        for r, c, p in [(4, 4, 0), (850, 1304, 0),
                                        (2900, 4574, 2), (3643, 5467, 2)]],
        },
        "synthetic_gain_square_nonzero_origin": {
            "bounds_tlbr": [10, 20, 14, 24],
            "points_vh": [2, 2],
            "spacing_vh": [0.5, 0.5],
            "origin_vh": [0.0, 0.0],
            "values": [[[1.0], [2.0]], [[3.0], [4.0]]],
            "samples": [{"row": r, "col": c,
                         "value": gain_interpolate({
                             "points": [2, 2], "spacing": [0.5, 0.5],
                             "origin": [0.0, 0.0], "planes": 1,
                             "values": [[[1.0], [2.0]], [[3.0], [4.0]]]},
                             r, c, 0, (10, 20, 14, 24))}
                        for r, c in [(10, 20), (11, 21), (13, 23)]],
        },
        "synthetic_bicubic_headroom": {
            "phase_128_yx": [64, 64],
            "source_4x4": [[-0.25, 0.0, 0.0, 0.0],
                            [0.0, 0.2, 0.8, 0.0],
                            [0.0, 0.8, 1.2, 0.0],
                            [0.0, 0.0, 0.0, 1.5]],
            "unclipped_bicubic_value": synthetic_unclipped,
            "strict_dng_clipped_value": synthetic_strict,
            "negative_scalar_unclipped": -0.25,
            "negative_scalar_strict_dng_clipped": 0.0,
        },
        "clipping_contract": {
            "opcode_list_2_3_logical_range": [0.0, 1.0],
            "gain_input": 0.3,
            "gain": gain["values"][0][0][0],
            "float_headroom_product": 0.3 * gain["values"][0][0][0],
            "strict_dng_clipped_product": min(1.0, max(0.0, 0.3 * gain["values"][0][0][0])),
            "proposed_float_headroom_is_equal_only_when_product_is_in_range": True,
        },
    }
    result["reference_samples"] = reference_samples
    width, height = (result["raw_fields"]["256"][0], result["raw_fields"]["257"][0])
    # DNG's ActiveArea defaults to the entire raw image when omitted.
    result["active_area"] = result["raw_fields"].get("50829", [0, 0, height, width])
    result["active_area_size"] = [
        result["active_area"][2] - result["active_area"][0],
        result["active_area"][3] - result["active_area"][1],
    ]
    result["coordinate_space"] = {
        "active_area_raw_tlbr": result["active_area"],
        "active_local_bounds_tlbr": [0, 0, *result["active_area_size"]],
        "opcode_area_is_active_local": True,
        "raw_to_active_local": "(row - active_top, col - active_left)",
    }
    crop = result["raw_fields"]["50720"]
    scale = result["raw_fields"]["50718"]
    result["default_final_size"] = [
        crop[0][0] / crop[0][1] * scale[0][0] / scale[0][1],
        crop[1][0] / crop[1][1] * scale[1][0] / scale[1][1],
    ]
    best = result["raw_fields"]["50780"][0]
    result["best_quality_final_size"] = [
        result["default_final_size"][0] * best[0] / best[1],
        result["default_final_size"][1] * best[0] / best[1],
    ]
    result["wb_reference_5500k_tint10_dual_calibration_comparator"] = dng_wb_reference(
        result["color_stage"], 5_500.0, 10.0)
    result["wb_reference_5500k_tint10_fixed_cm2"] = dng_wb_fixed_cm2_reference(
        result["color_stage"], 5_500.0, 10.0)
    result["wb_reference_2000k_tint100_fixed_cm2"] = dng_wb_fixed_cm2_reference(
        result["color_stage"], 2_000.0, 100.0)
    # Keep the short key aligned with the production policy used by the RAW
    # adapter; the dual-calibration result above is explicitly comparator-only.
    result["wb_reference_5500k_tint10"] = result["wb_reference_5500k_tint10_fixed_cm2"]
    if args.sparse:
        result["sparse_reference"] = sparse_reference(
            args.sparse, gain, warp, result["active_area"])
        endpoint_deltas = []
        for sample in result["sparse_reference"]["samples"]:
            out_x, out_y = sample["out_raw_xy"]
            plane = sample["plane"]
            sdk = warp_source_position(
                warp, out_y - result["active_area"][0],
                out_x - result["active_area"][1],
                                        bounds, pixel_scale_v=1.0, plane=plane)
            literal = warp_source_position_spec_endpoints(
                warp, out_y - result["active_area"][0],
                out_x - result["active_area"][1], bounds,
                pixel_scale_v=1.0, plane=plane)
            endpoint_deltas.append(max(abs(sdk[i] - literal[i]) for i in (0, 1)))
        result["warp_endpoint_comparison"] = {
            "sdk_uses_exclusive_dng_rect_right_bottom": True,
            "spec_literal_uses_bottom_right_pixel_coordinates": True,
            "sparse_max_abs_coordinate_delta_pixels": max(endpoint_deltas),
        }
    if not args.full:
        # The values are read above to calculate references but are not copied
        # into the checked-in JSON.  This keeps the fixture small and avoids
        # treating a generated copy of the private source as a test asset.
        for opcode in result["opcodes"]:
            opcode.pop("values", None)
    text = json.dumps(result, indent=2, sort_keys=True)
    if args.json:
        args.json.write_text(text + "\n")
    else:
        print(text)


if __name__ == "__main__":
    main()
