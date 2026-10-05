//! One 8-bit sRGB colour with alpha, written `#rrggbb` or `#rrggbbaa`, and the colour arithmetic
//! the theme model does on it through the core's colour equations: the encoded-sRGB mix, WCAG 2
//! contrast from Rec. 709 luminance, and moves in Oklab.
use crate::colour::{cielab, luma, oklab, srgb};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;

/// An 8-bit sRGB colour and its alpha, `255` for an opaque one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// The value of one hexadecimal digit in a `const` context.
const fn digit(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => panic!("not a hexadecimal digit"),
    }
}

impl Rgba {
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// A `#rrggbb` or `#rrggbbaa` literal, for the built-in tables: anything else fails to
    /// compile.
    pub(crate) const fn hex(text: &str) -> Self {
        let bytes = text.as_bytes();
        assert!(bytes.len() == 7 || bytes.len() == 9, "#rrggbb or #rrggbbaa");
        assert!(bytes[0] == b'#', "#rrggbb or #rrggbbaa");
        const fn pair(bytes: &[u8], at: usize) -> u8 {
            digit(bytes[at]) * 16 + digit(bytes[at + 1])
        }
        let a = if bytes.len() == 9 {
            pair(bytes, 7)
        } else {
            255
        };
        Self {
            r: pair(bytes, 1),
            g: pair(bytes, 3),
            b: pair(bytes, 5),
            a,
        }
    }

    /// `#rrggbb` or `#rrggbbaa`, either case. Anything else is refused with the text it was
    /// given.
    pub fn parse(text: &str) -> Result<Self, String> {
        let refuse = || format!("expected a colour #rrggbb or #rrggbbaa, found {text:?}");
        let digits = text.strip_prefix('#').ok_or_else(refuse)?;
        if !matches!(digits.len(), 6 | 8) || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(refuse());
        }
        let pair = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).map_err(|_| refuse());
        let a = if digits.len() == 8 { pair(6)? } else { 255 };
        Ok(Self {
            r: pair(0)?,
            g: pair(2)?,
            b: pair(4)?,
            a,
        })
    }

    pub const fn is_opaque(self) -> bool {
        self.a == 255
    }

    pub const fn channels(self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }

    const fn from_channels([r, g, b]: [u8; 3]) -> Self {
        Self::rgb(r, g, b)
    }

    /// The colour's channels in linear light, its alpha ignored.
    pub(crate) fn linear(self) -> [f64; 3] {
        self.channels().map(srgb::decode_u8)
    }

    /// WCAG 2's relative luminance: Rec. 709 luminance of the linear channels.
    pub(crate) fn luminance(self) -> f64 {
        luma::rec709_f64(self.linear())
    }

    /// WCAG 2's contrast ratio between two opaque colours, from 1 to 21.
    pub(crate) fn contrast(self, other: Self) -> f64 {
        let (first, second) = (self.luminance(), other.luminance());
        (first.max(second) + 0.05) / (first.min(second) + 0.05)
    }

    /// Oklab `[L, a, b]`.
    pub(crate) fn oklab(self) -> [f64; 3] {
        oklab::lab_f64(self.linear())
    }

    /// OKLCh chroma.
    pub(crate) fn chroma(self) -> f64 {
        let [_, a, b] = self.oklab();
        a.hypot(b)
    }

    /// CIELAB `[L*, a*, b*]` under D65.
    pub(crate) fn cielab(self) -> [f64; 3] {
        cielab::from_linear(self.linear())
    }

    /// This colour mixed toward `other` by `weight` in encoded sRGB, rounded to the nearest code:
    /// the way the visual language's composites of one token over another were sampled. Opaque.
    pub(crate) fn mix(self, other: Self, weight: f64) -> Self {
        let [from, to] = [self.channels(), other.channels()];
        Self::from_channels(std::array::from_fn(|channel| {
            let (from, to) = (f64::from(from[channel]), f64::from(to[channel]));
            (from + (to - from) * weight).round().clamp(0.0, 255.0) as u8
        }))
    }

    /// The opaque colour at Oklab `[l, a, b]`, `l` clamped to `[0, 1]`. Where that lies outside
    /// the sRGB gamut, its chroma is reduced, at the same lightness and hue, to the gamut's edge.
    pub(crate) fn from_oklab([l, a, b]: [f64; 3]) -> Self {
        let l = l.clamp(0.0, 1.0);
        let at = |scale: f64| oklab::from_lab_f64([l, a * scale, b * scale]);
        let inside = |rgb: [f64; 3]| {
            rgb.iter()
                .all(|channel| (-1e-9..=1.0 + 1e-9).contains(channel))
        };
        let rgb = if inside(at(1.0)) {
            at(1.0)
        } else {
            // A neutral at any lightness in [0, 1] is inside, so the edge lies between.
            let (mut inner, mut outer) = (0.0, 1.0);
            for _ in 0..40 {
                let middle = (inner + outer) / 2.0;
                if inside(at(middle)) {
                    inner = middle;
                } else {
                    outer = middle;
                }
            }
            at(inner)
        };
        let quantizer = srgb::quantizer();
        Self::from_channels(rgb.map(|channel| quantizer.rounded(channel)))
    }

    /// This colour at another Oklab lightness, keeping its hue and, where the gamut allows, its
    /// chroma. Opaque.
    pub(crate) fn at_lightness(self, lightness: f64) -> Self {
        let [_, a, b] = self.oklab();
        Self::from_oklab([lightness, a, b])
    }
}

impl fmt::Display for Rgba {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)?;
        if !self.is_opaque() {
            write!(f, "{:02x}", self.a)?;
        }
        Ok(())
    }
}

impl std::str::FromStr for Rgba {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        Self::parse(text)
    }
}

impl Serialize for Rgba {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(de::Error::custom)
    }
}
