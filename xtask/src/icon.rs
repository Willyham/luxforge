//! The application icon in `packaging/icon`. `luxforge.svg` is the editable source; the macOS ICNS
//! and the Linux PNGs beside it are generated from it by `cargo xtask icon`, which rasterizes with
//! macOS's `sips` and assembles the ICNS with `iconutil`. The generated files are committed, so
//! packaging needs neither tool.
use crate::*;

/// The icon's directory, relative to the repository root.
pub const DIRECTORY: &str = "packaging/icon";
/// The editable source.
pub const SOURCE: &str = "luxforge.svg";
/// The macOS icon, which the bundle names in `CFBundleIconFile`.
pub const ICNS: &str = "luxforge.icns";
/// The Linux icons: pixel size and file name.
pub const PNGS: &[(u32, &str)] = &[(256, "luxforge-256.png"), (512, "luxforge-512.png")];
/// The members of the ICNS's iconset, with their pixel sizes, as `iconutil` names them.
const ICONSET: &[(&str, u32)] = &[
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
];

/// Renders `svg` at `size` pixels square into the PNG `destination`.
fn rasterize(svg: &Path, size: u32, destination: &Path) -> Result {
    let size = size.to_string();
    let result = Command::new("sips")
        .args(["-s", "format", "png", "-z", &size, &size])
        .arg(svg)
        .arg("--out")
        .arg(destination)
        .output()?;
    ensure(
        result.status.success() && destination.is_file(),
        format!(
            "sips could not render the icon at {size} pixels: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ),
    )
}

/// Regenerates the ICNS and the PNGs in `packaging/icon` from its SVG.
pub fn generate(root: &Path) -> Result {
    ensure(
        cfg!(target_os = "macos"),
        "Icon generation requires macOS sips and iconutil",
    )?;
    let directory = root.join(DIRECTORY);
    let svg = directory.join(SOURCE);
    ensure(
        svg.is_file(),
        format!("Missing icon source: {}", svg.display()),
    )?;
    let temp = tempfile::tempdir()?;
    let iconset = temp.path().join("luxforge.iconset");
    fs::create_dir(&iconset)?;
    for (name, size) in ICONSET {
        rasterize(&svg, *size, &iconset.join(name))?;
    }
    let icns = temp.path().join(ICNS);
    let status = Command::new("iconutil")
        .args(["--convert", "icns", "--output"])
        .arg(&icns)
        .arg(&iconset)
        .status()?;
    ensure(status.success(), "iconutil could not assemble the ICNS")?;
    for (size, name) in PNGS {
        rasterize(&svg, *size, &temp.path().join(name))?;
    }
    for name in std::iter::once(ICNS).chain(PNGS.iter().map(|(_, name)| *name)) {
        fs::copy(temp.path().join(name), directory.join(name))?;
        println!("{}", directory.join(name).display());
    }
    Ok(())
}

/// Copies the committed ICNS to `destination`, refusing a missing, empty or malformed one.
pub fn copy_icns(root: &Path, destination: &Path) -> Result {
    let source = root.join(DIRECTORY).join(ICNS);
    ensure(
        source.is_file(),
        format!("Missing application icon: {}", source.display()),
    )?;
    let bytes = fs::read(&source)?;
    ensure(
        !icns_types(&bytes)?.is_empty(),
        format!("The application icon holds no images: {}", source.display()),
    )?;
    fs::write(destination, bytes)?;
    Ok(())
}

/// The four-character types of an ICNS's entries, refusing a file that is not one.
pub fn icns_types(bytes: &[u8]) -> Result<Vec<String>> {
    let length = |at: usize| -> Result<usize> {
        let field = bytes.get(at..at + 4).ok_or("Truncated ICNS")?;
        Ok(u32::from_be_bytes(field.try_into()?) as usize)
    };
    ensure(
        bytes.get(..4) == Some(b"icns") && length(4)? == bytes.len(),
        "Not an ICNS file",
    )?;
    let mut types = Vec::new();
    let mut at = 8;
    while at < bytes.len() {
        let size = length(at + 4)?;
        ensure(
            size >= 8 && at + size <= bytes.len(),
            "Malformed ICNS entry",
        )?;
        types.push(String::from_utf8_lossy(&bytes[at..at + 4]).into_owned());
        at += size;
    }
    Ok(types)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(DIRECTORY)
    }

    #[test]
    fn the_committed_icons_are_complete() {
        let directory = directory();
        let source = fs::read_to_string(directory.join(SOURCE)).unwrap();
        assert!(source.contains("viewBox=\"0 0 1024 1024\""));
        for (size, name) in PNGS {
            let image = image::open(directory.join(name)).unwrap();
            assert_eq!((image.width(), image.height()), (*size, *size), "{name}");
            assert!(image.color().has_alpha(), "{name}");
        }
        // The ICNS holds every iconset member: 16, 32, 128, 256 and 512 points at 1x and 2x.
        let types = icns_types(&fs::read(directory.join(ICNS)).unwrap()).unwrap();
        for kind in [
            "ic04", "ic11", "ic05", "ic12", "ic07", "ic13", "ic08", "ic14", "ic09", "ic10",
        ] {
            assert!(types.iter().any(|t| t == kind), "{kind} missing: {types:?}");
        }
    }

    #[test]
    fn icns_parsing_refuses_other_files() {
        assert!(icns_types(b"\x89PNG\r\n\x1a\n").is_err());
        assert!(icns_types(b"icns\0\0\0\x10ic07\0\0\0\x09").is_err());
        assert_eq!(
            icns_types(b"icns\0\0\0\x10ic07\0\0\0\x08").unwrap(),
            vec!["ic07".to_owned()]
        );
    }

    #[test]
    fn copying_refuses_a_missing_icon() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("checkout wïth spaces");
        fs::create_dir_all(root.join(DIRECTORY)).unwrap();
        let destination = root.join("Lüxforge.app Resources.icns");
        let error = copy_icns(&root, &destination).unwrap_err().to_string();
        assert!(error.contains("Missing application icon"), "{error}");
        fs::write(root.join(DIRECTORY).join(ICNS), b"icns\0\0\0\x08").unwrap();
        assert!(copy_icns(&root, &destination).is_err());
        let icns = b"icns\0\0\0\x10ic07\0\0\0\x08";
        fs::write(root.join(DIRECTORY).join(ICNS), icns).unwrap();
        copy_icns(&root, &destination).unwrap();
        assert_eq!(fs::read(destination).unwrap(), icns);
    }
}
