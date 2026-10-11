# Usage statistics wire format

The desktop encoder (`crates/luxforge-core/src/usage/`) and the standalone ingress
(`services/usage-ingest/`) implement exactly this shape; both are implemented
([ingress README](../../services/usage-ingest/README.md)). The core also holds an independent strict
decoder (`usage::decode_batch`) and synthetic valid and invalid batches in
`crates/luxforge-core/tests/fixtures/usage/`, which both sides accept and refuse alike (the ingress's
`tests/desktop.rs` reads them from the checkout). The
[design](usage-statistics.md) is the behavioural contract; this file fixes the key names and ranges. Every object is closed: an unknown key rejects the whole batch.

## Envelope (all keys required unless marked optional)

| Key | Type and range |
| --- | --- |
| `schema` | integer, exactly `1` |
| `disclosure_version` | integer, exactly `1` |
| `release` | string matching `^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\+[0-9a-f]{12}$` |
| `installation_id` | canonical lowercase hyphenated UUID, version 4, RFC 4122 variant |
| `session_id` | same form as `installation_id` |
| `session_start` | string `YYYY-MM-DDTHH:MM:SSZ`, a valid UTC calendar time, years 2020 to 2200 |
| `batch_seq` | integer `0..=4294967295` |
| `dropped` | integer `0..=4294967295` |
| `device` | object, below; serialized size at most 2048 bytes |
| `events` | array of 1 to 512 event objects |

Body at most 65536 bytes, `Content-Type: application/json` (an optional `; charset=utf-8` is
accepted), nesting depth at most 4 below the envelope (envelope → device → os/cpu/gpus → gpu object →
`driver_version`, the fifth container including the envelope; event objects are flat), checked before parsing.

## Device

| Key | Type and range |
| --- | --- |
| `os` | object: `family` (`macos`, `windows`, `linux`, `other`), `version` (array of 1 to 4 integers `0..=4294967295`); optional `build` (macOS only, `^[0-9]{1,3}[A-Z][0-9]{1,5}[a-z]?$`); Linux only, optional `distro` (`arch`, `debian`, `ubuntu`, `fedora`, `rhel`, `opensuse`, `nixos`, `alpine`, `other`), optional `distro_version` (array of 1 to 4 integers), optional `kernel` (array of 1 to 4 integers) |
| `arch` | `arm64`, `x86_64`, `other` |
| `cpu` | object: `vendor` (`apple`, `intel`, `amd`, `qualcomm`, `other`), `physical_cores` and `logical_cores` (`1..=1024`); Apple only, optional `generation` (`1..=64`) and `tier` (`base`, `pro`, `max`, `ultra`); Intel/AMD only, optional `family`, `model`, `stepping` (`0..=4095`) |
| `memory_gib_band` | one of the integers `8, 16, 32, 64, 128, 256, 512, 1024` (installed memory at most that many GiB) or the string `"over1024"` |
| `gpus` | array of 0 to 2 objects, distinct by (`vendor`, `vendor_id`, `device_id`, `generation`, `tier`): `vendor` (`apple`, `nvidia`, `amd`, `intel`, `qualcomm`, `arm`, `imagination`, `microsoft`, `mesa`, `other`), `backend` (`metal`, `vulkan`, `dx12`, `gl`, `other`), `type` (`integrated`, `discrete`, `virtual`, `cpu`, `other`), `unified_memory` (boolean); optional `vendor_id` and `device_id` (`0..=65535`), optional `generation` (`1..=64`) and `tier` (`base`, `pro`, `max`, `ultra`) for Apple, optional `memory_gib_band` (as above), optional `driver_version` (array of 1 to 4 integers) |
| `renderer` | `gpu`, `reference` |
| `renderer_fallback` | optional, only with `renderer: reference`: `no-adapter`, `adapter-lost`, `unsupported`, `disabled`, `other` |

## Events

Every event has `seq` (integer `0..=4294967295`, strictly increasing within the batch), `t`
(integer milliseconds `0..=4294967295`, non-decreasing within the batch) and `name`, plus exactly the
fields its row allows (required unless marked optional).

| `name` | Fields |
| --- | --- |
| `session.start` | `trigger`: `launch`, `opt-in` |
| `app.presented` | `state`: `visible`, `hidden` |
| `app.idle` | `state`: `idle`, `active` |
| `workspace.enter` | `workspace`: `select`, `develop` |
| `image.open` | `image` |
| `interaction` | `feature` (registry enum); optional `image` |
| `tool.commit` | `tool` (registry enum), `image` |
| `images.added` | `count`: `1..=1000000` |
| `operation.end` | `operation`: `source-preparation`, `catalog-scan`, `export`, `auto-tone`; `outcome`: `completed`, `cancelled`, `refused`, `failed`; `duration_ms`: `0..=86400000`; optional `count` (`0..=1000000`, `export` only); optional `image` (`source-preparation`, `auto-tone`, `export` only) |
| `preview.settled` | `image`, `renderer` (`gpu`, `reference`), `duration_ms` (`0..=86400000`) |
| `process.sample` | `cpu_percent` (`0..=102400`), `memory_mib` (`0..=16777216`), `memory_kind` (`footprint`, `resident`, `private`); optional `gpu_percent` (`0..=102400`), optional `gpu_memory_mib` (`0..=16777216`) |

`image` is exactly 32 lowercase hexadecimal characters.

The `feature` and `tool` enums are the registry's, resolved against the shipped built-in modules
and the design's interaction list and checked in as `crates/luxforge-core/src/usage/registry.json`
(`{"tool": [...], "feature": [...]}`, kebab-case values, sorted). The ingress validates against its
own copy, which its `tests/desktop.rs` and the backend journey's check hold byte for byte equal to the desktop's.

## Responses

`202` with an empty body on acceptance; `400` for any shape, range or ordering violation (whole
batch), `413` over 65536 bytes, `415` wrong content type, `405` another method, `404` another path,
`429` when admission is refused (optional `Retry-After` seconds). No response body beyond a fixed
short status word.
