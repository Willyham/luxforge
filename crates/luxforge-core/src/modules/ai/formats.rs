//! The format check an install runs on a published model file, after its length and SHA-256 have
//! passed: the file opens as the format its resource declares, judged from its first bytes only,
//! so a 2.5 GB weight file is never read twice. The hash already proves the bytes are the pinned
//! ones; this proves the pin names the kind of file the runtime will be handed.
use std::{fs::File, io::Read, path::Path};

/// The largest safetensors header accepted: its JSON table of tensors.
const MAX_SAFETENSORS_HEADER: u64 = 100 * 1024 * 1024;

/// Whether the file at `path` opens as `format`: `gguf` (the magic and a version of 2 or 3),
/// `safetensors` (a header length within bounds and a JSON header), `lfsdcond-1` (the
/// conditioning's magic and version 1), `onnx` (a `ModelProto` opening with its IR version) or
/// `json` (an object). Any other format is refused.
pub(crate) fn check(path: &Path, format: &str) -> Result<(), String> {
    let mut head = [0_u8; 16];
    let read = File::open(path)
        .and_then(|mut file| {
            let mut filled = 0;
            loop {
                let n = file.read(&mut head[filled..])?;
                if n == 0 || filled + n == head.len() {
                    return Ok(filled + n);
                }
                filled += n;
            }
        })
        .map_err(|error| format!("cannot be read: {error}"))?;
    let head = &head[..read];
    let ok = match format {
        "gguf" => {
            head.len() >= 8
                && &head[..4] == b"GGUF"
                && matches!(u32::from_le_bytes(head[4..8].try_into().unwrap()), 2 | 3)
        }
        "safetensors" => {
            head.len() >= 9 && {
                let header = u64::from_le_bytes(head[..8].try_into().unwrap());
                (2..=MAX_SAFETENSORS_HEADER).contains(&header) && head[8] == b'{'
            }
        }
        "lfsdcond-1" => {
            head.len() >= 12
                && &head[..8] == b"LFSDCOND"
                && u32::from_le_bytes(head[8..12].try_into().unwrap()) == 1
        }
        // An ONNX model is a protobuf `ModelProto` whose first field is `ir_version` (field 1,
        // varint): the tag byte 0x08.
        "onnx" => head.first() == Some(&0x08),
        "json" => head
            .iter()
            .find(|byte| !byte.is_ascii_whitespace())
            .is_some_and(|byte| *byte == b'{'),
        other => return Err(format!("format {other} has no check")),
    };
    if ok {
        Ok(())
    } else {
        Err(format!("is not a {format} file"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_format_is_told_by_its_first_bytes() {
        let root = crate::capabilities::testing::temp("ai-formats");
        std::fs::create_dir_all(&root).unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = root.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let mut gguf = b"GGUF".to_vec();
        gguf.extend(3_u32.to_le_bytes());
        gguf.extend([0; 8]);
        let mut safetensors = 2_u64.to_le_bytes().to_vec();
        safetensors.extend(b"{}");
        let mut condition = b"LFSDCOND".to_vec();
        condition.extend(1_u32.to_le_bytes());
        condition.extend(1_u32.to_le_bytes());
        let cases = [
            ("onnx", write("a.onnx", &[0x08, 0x08, 0x12, 0x07])),
            ("gguf", write("a.gguf", &gguf)),
            ("safetensors", write("a.safetensors", &safetensors)),
            ("lfsdcond-1", write("a.lfsdcond", &condition)),
            ("json", write("a.json", b"  {\"model\": {}}")),
        ];
        for (format, path) in &cases {
            check(path, format).unwrap();
            // Every other format refuses it.
            for (other, _) in &cases {
                if other != format {
                    assert!(check(path, other).is_err(), "{format} as {other}");
                }
            }
        }
        // A truncated file, a later GGUF version, a conditioning of another version.
        assert!(check(&write("short", b"GG"), "gguf").is_err());
        let mut v4 = b"GGUF".to_vec();
        v4.extend(4_u32.to_le_bytes());
        assert!(check(&write("v4", &v4), "gguf").is_err());
        let mut c2 = b"LFSDCOND".to_vec();
        c2.extend(2_u32.to_le_bytes());
        assert!(check(&write("c2", &c2), "lfsdcond-1").is_err());
        assert!(check(&cases[1].1, "onnx").is_err());
        assert!(check(&cases[0].1, "pickle").is_err());
        assert!(check(&root.join("missing"), "gguf").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
