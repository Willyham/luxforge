//! The corpus manifest: the images the harness is measured on, and the pasted-object ground truth
//! generated from them.
//!
//! `corpus.json` is checked in. It names the owner's private images by file, size, source and use,
//! never by content (they stay in the ignored `private/` directory and are never committed), and
//! lists a public CC0 subset with source URLs, licences, sizes and SHA-256 digests. The harness
//! never downloads: a public file is fetched by hand from its URL and checked with
//! `removal-probe corpus --verify`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::commands::fill::{Pasted, paste_object};
use crate::image::Rgb8;
use crate::models;

const MANIFEST: &str = include_str!("../corpus.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub private: Private,
    pub public: Public,
    pub ground_truth: GroundTruth,
}

/// The owner's images: listed, never committed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Private {
    pub directory: String,
    pub environment: String,
    pub committed: bool,
    pub note: String,
    pub images: Vec<PrivateImage>,
}

/// A private image by name and use, with no digest: its content is not the manifest's to record.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateImage {
    pub file: String,
    pub size: String,
    pub source: String,
    #[serde(rename = "use")]
    pub usage: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Public {
    pub directory: String,
    pub environment: String,
    pub licence: String,
    pub note: String,
    pub images: Vec<PublicImage>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicImage {
    pub file: String,
    pub category: String,
    #[serde(rename = "use")]
    pub usage: String,
    pub title: String,
    pub author: String,
    pub licence: String,
    pub page: String,
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroundTruth {
    pub ring_width: u32,
    pub note: String,
    pub cases: Vec<Case>,
}

/// A pasted-object case: a clean image and where the synthetic object goes.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    /// `private:FILE` or `public:FILE`.
    pub image: String,
    /// The object's centre as fractions of the image's width and height.
    pub centre: [f64; 2],
    /// The object's width and height as fractions of the image's shorter side.
    pub size: [f64; 2],
    /// The region's dilation in pixels.
    pub dilate: u32,
}

impl Manifest {
    /// The manifest checked in beside the sources.
    pub fn embedded() -> Manifest {
        serde_json::from_str(MANIFEST).expect("corpus.json parses")
    }

    pub fn case(&self, id: &str) -> Result<&Case, String> {
        self.ground_truth
            .cases
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| {
                let ids: Vec<_> = self
                    .ground_truth
                    .cases
                    .iter()
                    .map(|c| c.id.as_str())
                    .collect();
                format!("unknown case {id:?}; expected one of {}", ids.join(", "))
            })
    }

    pub fn public_image(&self, file: &str) -> Option<&PublicImage> {
        self.public.images.iter().find(|i| i.file == file)
    }
}

/// Where the corpus lives on this machine.
pub struct Dirs {
    pub private: PathBuf,
    pub public: PathBuf,
}

impl Dirs {
    pub fn new(private: Option<PathBuf>, public: Option<PathBuf>) -> Dirs {
        Dirs {
            private: crate::paths::corpus_directory(private),
            public: crate::paths::public_directory(public),
        }
    }

    /// The file a reference (`private:FILE` or `public:FILE`) names.
    pub fn resolve(&self, reference: &str) -> Result<PathBuf, String> {
        match reference.split_once(':') {
            Some(("private", file)) => Ok(self.private.join(file)),
            Some(("public", file)) => Ok(self.public.join(file)),
            _ => Err(format!(
                "{reference:?} is not a corpus reference; expected private:FILE or public:FILE"
            )),
        }
    }
}

impl Case {
    /// The object's centre, its size and its dilation in pixels, for an image of `size`.
    pub fn placement(&self, size: (u32, u32)) -> ((u32, u32), (u32, u32), u32) {
        let shorter = size.0.min(size.1) as f64;
        let round = |v: f64| v.round().max(1.0) as u32;
        (
            (
                (self.centre[0] * size.0 as f64).round() as u32,
                (self.centre[1] * size.1 as f64).round() as u32,
            ),
            (round(self.size[0] * shorter), round(self.size[1] * shorter)),
            self.dilate,
        )
    }

    /// The pasted image and the region that removes it, generated from the clean image.
    pub fn paste(&self, clean: &Rgb8) -> Pasted {
        let (centre, size, dilate) = self.placement((clean.width, clean.height));
        paste_object(clean, centre, size, dilate)
    }
}

/// How a manifest file stands on this machine.
#[derive(Debug, PartialEq, Eq)]
pub enum Presence {
    Absent,
    /// Present; with `--verify`, whether its size and SHA-256 match (public files only).
    Present,
    Verified,
    Refused(String),
}

/// Checks one public file against its listed size and SHA-256. Absent is not a failure here.
pub fn check_public(image: &PublicImage, dir: &Path, verify: bool) -> Presence {
    let path = dir.join(&image.file);
    let Ok(meta) = std::fs::metadata(&path) else {
        return Presence::Absent;
    };
    if meta.len() != image.bytes {
        return Presence::Refused(format!(
            "refusing {}: {} bytes, expected {}",
            image.file,
            meta.len(),
            image.bytes
        ));
    }
    if !verify {
        return Presence::Present;
    }
    match models::sha256(&path) {
        Ok(digest) if digest == image.sha256 => Presence::Verified,
        Ok(digest) => Presence::Refused(format!(
            "refusing {}: SHA-256 {digest}, expected {}",
            image.file, image.sha256
        )),
        Err(e) => Presence::Refused(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn the_manifest_parses_and_names_every_image_once() {
        let m = Manifest::embedded();
        assert_eq!(m.format, 1);
        assert!(!m.private.committed);
        let files: Vec<&str> = m
            .private
            .images
            .iter()
            .map(|i| i.file.as_str())
            .chain(m.public.images.iter().map(|i| i.file.as_str()))
            .collect();
        let unique: HashSet<_> = files.iter().collect();
        assert_eq!(unique.len(), files.len(), "{files:?}");
        for f in &files {
            assert!(f.ends_with(".jpg") && !f.contains('/'), "{f}");
        }
    }

    #[test]
    fn the_private_corpus_is_listed_by_name_and_use_not_by_content() {
        let m = Manifest::embedded();
        assert!(m.private.images.iter().any(|i| i.file == "z6-bridge.jpg"));
        for i in &m.private.images {
            assert!(!i.usage.is_empty() && !i.source.is_empty(), "{}", i.file);
        }
        // No digest or size in bytes can be recorded for them: the fields do not exist, and an
        // unknown field is a parse error (`deny_unknown_fields`).
        let with_hash = MANIFEST.replacen("\"source\"", "\"sha256\": \"00\", \"source\"", 1);
        assert!(serde_json::from_str::<Manifest>(&with_hash).is_err());
    }

    #[test]
    fn the_public_subset_is_cc0_with_sources_and_digests_and_covers_the_categories() {
        let m = Manifest::embedded();
        for i in &m.public.images {
            assert_eq!(i.licence, "CC0 1.0", "{}", i.file);
            assert!(
                i.url.starts_with("https://upload.wikimedia.org/"),
                "{}: {}",
                i.file,
                i.url
            );
            assert!(
                i.page
                    .starts_with("https://commons.wikimedia.org/wiki/File:"),
                "{}",
                i.file
            );
            assert_eq!(i.sha256.len(), 64, "{}", i.file);
            assert!(
                i.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                "{}",
                i.file
            );
            assert!(
                i.bytes > 0 && i.width >= 2400 && i.height >= 2000,
                "{}",
                i.file
            );
            assert!(!i.author.is_empty() && !i.title.is_empty(), "{}", i.file);
        }
        for category in ["people", "skies", "water", "small objects"] {
            let n = m
                .public
                .images
                .iter()
                .filter(|i| i.category == category)
                .count();
            assert!(n >= 3, "{category}: {n}");
        }
        let categories: HashSet<_> = m
            .public
            .images
            .iter()
            .map(|i| i.category.as_str())
            .collect();
        assert_eq!(categories.len(), 4, "{categories:?}");
    }

    #[test]
    fn every_case_names_a_listed_image_and_fits_inside_it() {
        let m = Manifest::embedded();
        let mut ids = HashSet::new();
        for c in &m.ground_truth.cases {
            assert!(ids.insert(&c.id), "duplicate case {}", c.id);
            let (kind, file) = c.image.split_once(':').expect("a reference");
            let size = match kind {
                "public" => {
                    let i = m
                        .public_image(file)
                        .unwrap_or_else(|| panic!("{} unlisted", c.image));
                    (i.width, i.height)
                }
                "private" => {
                    assert!(
                        m.private.images.iter().any(|i| i.file == file),
                        "{}",
                        c.image
                    );
                    // The private sizes are descriptive text; the smallest is 3389 x 4236.
                    (3389, 3389)
                }
                other => panic!("{}: unknown kind {other}", c.image),
            };
            let (centre, object, dilate) = c.placement(size);
            // The object, its shadow and the dilation stay inside the frame, so the region is
            // never clipped by the image border (the reach `paste_object` computes).
            let (w, h) = (object.0 as f64, object.1 as f64);
            let soft = (0.12 * h).max(2.0);
            let margin =
                (0.6 * w + soft + 2.0).max(0.7 * h + soft + 2.0).ceil() as u64 + dilate as u64;
            for (at, extent) in [(centre.0, size.0), (centre.1, size.1)] {
                assert!(
                    at as u64 >= margin && at as u64 + margin <= extent as u64,
                    "{}",
                    c.id
                );
            }
        }
        assert!(
            m.case("no-such-case")
                .unwrap_err()
                .contains("expected one of")
        );
        assert_eq!(m.ground_truth.ring_width, crate::score::RING_WIDTH);
    }

    #[test]
    fn references_resolve_into_the_two_directories() {
        let d = Dirs {
            private: PathBuf::from("/p"),
            public: PathBuf::from("/q"),
        };
        assert_eq!(
            d.resolve("private:a.jpg").unwrap(),
            PathBuf::from("/p/a.jpg")
        );
        assert_eq!(
            d.resolve("public:b.jpg").unwrap(),
            PathBuf::from("/q/b.jpg")
        );
        assert!(d.resolve("c.jpg").is_err());
        assert!(d.resolve("other:c.jpg").is_err());
    }

    /// A noisy clean image, deterministic.
    fn noise(w: u32, h: u32) -> Rgb8 {
        let mut s = 12345u32;
        let data = (0..w * h * 3)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s >> 24) as u8
            })
            .collect();
        Rgb8::new(w, h, data)
    }

    #[test]
    fn a_case_is_deterministic_and_changes_only_inside_its_region() {
        let m = Manifest::embedded();
        let case = m.case("sky-clouds-blue").unwrap();
        let clean = noise(500, 330);
        let (a, b) = (case.paste(&clean), case.paste(&clean));
        assert_eq!(a.image, b.image);
        assert_eq!(a.region, b.region);
        assert_ne!(a.image, clean, "the object is pasted");
        let mut inside = 0;
        for y in 0..clean.height {
            for x in 0..clean.width {
                if a.region.get(x, y) == 0 {
                    assert_eq!(a.image.pixel(x, y), clean.pixel(x, y), "({x}, {y})");
                } else {
                    inside += 1;
                }
            }
        }
        assert!(inside > 0);
        // Removing the object perfectly scores perfectly: the clean image is its own truth.
        let s = crate::score::score(&a.image, &clean, &a.region, Some(&clean), 8).unwrap();
        assert!(s.outside_unchanged());
        assert_eq!(s.region.unwrap().psnr_db, f64::INFINITY);
    }

    #[test]
    fn a_public_file_is_refused_by_name_when_its_size_or_digest_differs() {
        let dir = std::env::temp_dir().join(format!("luxforge-corpus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut image = Manifest::embedded().public.images.remove(0);
        image.file = "tiny.jpg".into();
        assert_eq!(check_public(&image, &dir, true), Presence::Absent);
        std::fs::write(dir.join("tiny.jpg"), b"abc").unwrap();
        let Presence::Refused(size) = check_public(&image, &dir, false) else {
            panic!("a wrong size is refused");
        };
        assert!(
            size.contains("refusing tiny.jpg") && size.contains("3 bytes"),
            "{size}"
        );
        image.bytes = 3;
        assert_eq!(check_public(&image, &dir, false), Presence::Present);
        let Presence::Refused(digest) = check_public(&image, &dir, true) else {
            panic!("a wrong digest is refused");
        };
        assert!(
            digest.contains("refusing tiny.jpg") && digest.contains("SHA-256"),
            "{digest}"
        );
        image.sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into();
        assert_eq!(check_public(&image, &dir, true), Presence::Verified);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
