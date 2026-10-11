//! 8-bit sRGB images: the probe's input and output domain.

use std::path::Path;

use image::{DynamicImage, ImageDecoder, ImageReader, RgbImage};

use crate::mask::RectPx;

/// An 8-bit sRGB image, interleaved RGB, row-major, with any EXIF orientation already applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgb8 {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgb8 {
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        assert_eq!(
            data.len(),
            width as usize * height as usize * 3,
            "image size and buffer disagree"
        );
        Self {
            width,
            height,
            data,
        }
    }

    /// Decodes a JPEG or PNG and applies its EXIF orientation.
    pub fn load(path: &Path) -> Result<Self, String> {
        let e = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
        let reader = ImageReader::open(path)
            .map_err(|x| e(&x))?
            .with_guessed_format()
            .map_err(|x| e(&x))?;
        let mut decoder = reader.into_decoder().map_err(|x| e(&x))?;
        let orientation = decoder.orientation().map_err(|x| e(&x))?;
        let mut img = DynamicImage::from_decoder(decoder).map_err(|x| e(&x))?;
        img.apply_orientation(orientation);
        let rgb = img.into_rgb8();
        Ok(Self::new(rgb.width(), rgb.height(), rgb.into_raw()))
    }

    pub fn save_png(&self, path: &Path) -> Result<(), String> {
        RgbImage::from_raw(self.width, self.height, self.data.clone())
            .expect("buffer matches its size")
            .save(path)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let i = (y as usize * self.width as usize + x as usize) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    pub fn crop(&self, r: RectPx) -> Rgb8 {
        assert!(
            r.x + r.width <= self.width && r.y + r.height <= self.height,
            "crop outside the image"
        );
        let mut data = Vec::with_capacity(r.width as usize * r.height as usize * 3);
        for y in r.y..r.y + r.height {
            let start = (y as usize * self.width as usize + r.x as usize) * 3;
            data.extend_from_slice(&self.data[start..start + r.width as usize * 3]);
        }
        Rgb8::new(r.width, r.height, data)
    }

    /// Resamples to an exact size with a Catmull-Rom filter.
    pub fn resized(&self, width: u32, height: u32) -> Rgb8 {
        let src = RgbImage::from_raw(self.width, self.height, self.data.clone())
            .expect("buffer matches its size");
        let out =
            image::imageops::resize(&src, width, height, image::imageops::FilterType::CatmullRom);
        Rgb8::new(width, height, out.into_raw())
    }

    /// The size whose long side is `long`, keeping the aspect ratio (SAM's `ResizeLongestSide`,
    /// rounding half up).
    pub fn long_side_size(&self, long: u32) -> (u32, u32) {
        let scale = long as f64 / self.width.max(self.height) as f64;
        let w = (self.width as f64 * scale + 0.5) as u32;
        let h = (self.height as f64 * scale + 0.5) as u32;
        (w.max(1), h.max(1))
    }
}
