//! Isolated DNG opcode 3/4/5 helpers.
//!
//! The payload layouts and interpolation rules follow Adobe's public DNG SDK
//! (`dng_lens_correction.cpp`, `dng_bad_pixels.cpp`) and DNG 1.4+.  This file
//! intentionally has no orchestration or catalog policy; the caller must
//! decide the opcode list/stage and whether LibRaw has already applied a
//! correction.  Unknown or unsupported layouts return an error.

const MAX_PAYLOAD: usize = 1 << 20;
const MAX_BAD_POINTS: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpcodeError {
    Truncated,
    Invalid(&'static str),
    Unsupported(&'static str),
    Resource(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VignetteRadial {
    pub(crate) coefficients: [f64; 5],
    /// Normalized horizontal and vertical optical center.
    pub(crate) center: [f64; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BadPixels {
    pub(crate) bayer_phase: u32,
    pub(crate) points: Vec<(i32, i32)>,
    pub(crate) rectangles: Vec<(i32, i32, i32, i32)>,
}

impl VignetteRadial {
    /// Evaluate the DNG gain at an image-local pixel center.  The caller maps
    /// its `RawRect` to `(width, height)` and can apply this lazily per plane.
    pub(crate) fn gain(
        &self,
        x: f64,
        y: f64,
        width: usize,
        height: usize,
    ) -> Result<f64, OpcodeError> {
        if width == 0 || height == 0 || !x.is_finite() || !y.is_finite() {
            return Err(OpcodeError::Invalid("vignette coordinates"));
        }
        // DNG normalizes the optical center and radius against the outer
        // pixel coordinates, so an N-pixel axis spans 0..N-1 (not pixel
        // edges or pixel centers).
        let last_x = (width - 1) as f64;
        let last_y = (height - 1) as f64;
        let cx = self.center[0] * last_x;
        let cy = self.center[1] * last_y;
        let radius = (cx.max(last_x - cx).powi(2) + cy.max(last_y - cy).powi(2)).sqrt();
        if radius == 0.0 || !radius.is_finite() {
            return Err(OpcodeError::Invalid("vignette radius"));
        }
        let r2 = (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / radius).powi(2);
        let mut gain = 0.0;
        for coefficient in self.coefficients.iter().rev() {
            gain = r2 * (coefficient + gain);
        }
        let gain = gain + 1.0;
        if gain.is_finite() {
            Ok(gain)
        } else {
            Err(OpcodeError::Invalid("vignette gain"))
        }
    }
}

impl BadPixels {
    /// Compute one replacement from an immutable source mosaic.  This is the
    /// integration point for a decoder that must retain the exact original
    /// mosaic: callers can use the value only for the current development
    /// pass and leave source/history bytes untouched.
    #[cfg(test)]
    pub(crate) fn replacement(
        &self,
        source: &[u16],
        width: usize,
        height: usize,
        y: usize,
        x: usize,
    ) -> Result<Option<u16>, OpcodeError> {
        if !self.rectangles.is_empty() {
            return Err(OpcodeError::Unsupported(
                "clustered FixBadPixelsList rectangles",
            ));
        }
        if width == 0
            || height == 0
            || width.checked_mul(height) != Some(source.len())
            || y >= height
            || x >= width
        {
            return Err(OpcodeError::Invalid("bad-pixel source coordinates"));
        }
        let listed = self
            .points
            .iter()
            .any(|&(row, col)| row == y as i32 && col == x as i32);
        if !listed {
            return Ok(None);
        }
        let listed_bad = |yy: usize, xx: usize| {
            self.points
                .iter()
                .any(|&(row, col)| row == yy as i32 && col == xx as i32)
        };
        Ok(estimate_same_color(
            source,
            width,
            height,
            y,
            x,
            self.bayer_phase,
            listed_bad,
        ))
    }

    /// Compute all listed replacements from one immutable source pass.  The
    /// result is sorted and duplicate coordinates are emitted once; malformed
    /// coordinates and clustered rectangles fail. The returned unresolved
    /// count follows Adobe's behavior of leaving points with no usable
    /// same-colour neighbour unchanged.
    #[cfg(test)]
    pub(crate) fn replacements(
        &self,
        source: &[u16],
        width: usize,
        height: usize,
    ) -> Result<Vec<(usize, u16)>, OpcodeError> {
        self.replacements_with_unresolved(source, width, height)
            .map(|(patches, _)| patches)
    }

    pub(crate) fn replacements_with_unresolved(
        &self,
        source: &[u16],
        width: usize,
        height: usize,
    ) -> Result<(Vec<(usize, u16)>, usize), OpcodeError> {
        if !self.rectangles.is_empty() {
            return Err(OpcodeError::Unsupported(
                "clustered FixBadPixelsList rectangles",
            ));
        }
        if width == 0 || height == 0 || width.checked_mul(height) != Some(source.len()) {
            return Err(OpcodeError::Invalid("bad-pixel source dimensions"));
        }
        let mut points = self.points.clone();
        points.sort_unstable();
        points.dedup();
        if points.len() > MAX_BAD_POINTS {
            return Err(OpcodeError::Resource("bad pixel replacements"));
        }
        let mut out = Vec::with_capacity(points.len());
        let mut unresolved = 0;
        for &(y, x) in &points {
            if y < 0 || x < 0 || y as usize >= height || x as usize >= width {
                return Err(OpcodeError::Invalid("bad pixel outside image"));
            }
            let y = y as usize;
            let x = x as usize;
            // The list can contain 65,536 entries. Repeated linear membership
            // scans would make a valid dense list quadratic to decode.
            let listed_bad =
                |yy: usize, xx: usize| points.binary_search(&(yy as i32, xx as i32)).is_ok();
            if let Some(value) =
                estimate_same_color(source, width, height, y, x, self.bayer_phase, listed_bad)
            {
                out.push((y * width + x, value));
            } else {
                unresolved += 1;
            }
        }
        Ok((out, unresolved))
    }
}

/// Immutable counterpart for opcode 4.  Use this during each develop pass
/// when the retained mosaic must remain byte-for-byte identical.
pub(crate) fn bad_pixel_constant_replacement(
    source: &[u16],
    width: usize,
    height: usize,
    y: usize,
    x: usize,
    constant: u32,
    phase: u32,
) -> Result<Option<u16>, OpcodeError> {
    if constant > u16::MAX as u32
        || width == 0
        || height == 0
        || width.checked_mul(height) != Some(source.len())
        || y >= height
        || x >= width
    {
        return Err(OpcodeError::Invalid("bad-pixel source coordinates"));
    }
    let bad = constant as u16;
    if source[y * width + x] != bad {
        return Ok(None);
    }
    Ok(estimate_same_color(
        source,
        width,
        height,
        y,
        x,
        phase,
        |yy, xx| source[yy * width + xx] == bad,
    ))
}

/// Scan once and return sparse replacements, leaving `source` untouched.
#[cfg(test)]
pub(crate) fn bad_pixel_constant_replacements(
    source: &[u16],
    width: usize,
    height: usize,
    constant: u32,
    phase: u32,
) -> Result<Vec<(usize, u16)>, OpcodeError> {
    bad_pixel_constant_replacements_with_unresolved(source, width, height, constant, phase)
        .map(|(patches, _)| patches)
}

pub(crate) fn bad_pixel_constant_replacements_with_unresolved(
    source: &[u16],
    width: usize,
    height: usize,
    constant: u32,
    phase: u32,
) -> Result<(Vec<(usize, u16)>, usize), OpcodeError> {
    if constant > u16::MAX as u32
        || width == 0
        || height == 0
        || width.checked_mul(height) != Some(source.len())
    {
        return Err(OpcodeError::Invalid("bad-pixel source dimensions/value"));
    }
    let bad = constant as u16;
    let mut out = Vec::new();
    let mut unresolved = 0;
    for y in 0..height {
        for x in 0..width {
            if source[y * width + x] == bad {
                if let Some(value) =
                    bad_pixel_constant_replacement(source, width, height, y, x, constant, phase)?
                {
                    out.push((y * width + x, value));
                } else {
                    unresolved += 1;
                }
                if out.len() > MAX_BAD_POINTS {
                    return Err(OpcodeError::Resource("bad pixel replacements"));
                }
            }
        }
    }
    Ok((out, unresolved))
}

fn u32be(data: &[u8], at: usize) -> Result<u32, OpcodeError> {
    let b = data.get(at..at + 4).ok_or(OpcodeError::Truncated)?;
    Ok(u32::from_be_bytes(b.try_into().unwrap()))
}

fn i32be(data: &[u8], at: usize) -> Result<i32, OpcodeError> {
    Ok(u32be(data, at)? as i32)
}

fn f64be(data: &[u8], at: usize) -> Result<f64, OpcodeError> {
    let b = data.get(at..at + 8).ok_or(OpcodeError::Truncated)?;
    let value = f64::from_bits(u64::from_be_bytes(b.try_into().unwrap()));
    if !value.is_finite() {
        return Err(OpcodeError::Invalid("non-finite opcode coefficient"));
    }
    Ok(value)
}

fn checked_payload(data: &[u8]) -> Result<&[u8], OpcodeError> {
    if data.len() > MAX_PAYLOAD {
        return Err(OpcodeError::Resource("opcode payload"));
    }
    Ok(data)
}

/// Parse opcode 3.  Adobe defines five coefficients followed by H/V center.
pub(crate) fn parse_vignette_radial(data: &[u8]) -> Result<VignetteRadial, OpcodeError> {
    let p = checked_payload(data)?;
    if p.len() != 56 {
        return Err(OpcodeError::Invalid("FixVignetteRadial parameter length"));
    }
    let mut coefficients = [0.0; 5];
    for (index, value) in coefficients.iter_mut().enumerate() {
        *value = f64be(p, index * 8)?;
    }
    let center = [f64be(p, 40)?, f64be(p, 48)?];
    if center.iter().any(|v| !(0.0..=1.0).contains(v)) {
        return Err(OpcodeError::Invalid("FixVignetteRadial center"));
    }
    Ok(VignetteRadial {
        coefficients,
        center,
    })
}

/// Parse opcode 4's bare parameter payload: `constant, bayerPhase`.
pub(crate) fn parse_bad_pixels_constant(data: &[u8]) -> Result<(u32, u32), OpcodeError> {
    let p = checked_payload(data)?;
    if p.len() != 8 {
        return Err(OpcodeError::Invalid(
            "FixBadPixelsConstant parameter length",
        ));
    }
    let phase = u32be(p, 4)?;
    if phase > 3 {
        return Err(OpcodeError::Invalid("FixBadPixelsConstant Bayer phase"));
    }
    Ok((u32be(p, 0)?, phase))
}

/// Parse opcode 5's bare parameter payload: `bayerPhase, pointCount,
/// rectangleCount`, followed by image-local coordinates.
pub(crate) fn parse_bad_pixels_list(data: &[u8]) -> Result<BadPixels, OpcodeError> {
    let p = checked_payload(data)?;
    if p.len() < 12 {
        return Err(OpcodeError::Truncated);
    }
    let phase = u32be(p, 0)?;
    let point_count = u32be(p, 4)? as usize;
    let rect_count = u32be(p, 8)? as usize;
    if phase > 3 || point_count > MAX_BAD_POINTS || rect_count > MAX_BAD_POINTS {
        return Err(OpcodeError::Resource("FixBadPixelsList entries"));
    }
    let expected = 12usize
        .checked_add(
            point_count
                .checked_mul(8)
                .ok_or(OpcodeError::Resource("bad pixel points"))?,
        )
        .and_then(|v| v.checked_add(rect_count.checked_mul(16)?))
        .ok_or(OpcodeError::Resource("bad pixel list size"))?;
    if p.len() != expected {
        return Err(OpcodeError::Invalid("FixBadPixelsList parameter length"));
    }
    let mut at = 12;
    let mut points = Vec::with_capacity(point_count);
    for _ in 0..point_count {
        points.push((i32be(p, at)?, i32be(p, at + 4)?));
        at += 8;
    }
    let mut rectangles = Vec::with_capacity(rect_count);
    for _ in 0..rect_count {
        rectangles.push((
            i32be(p, at)?,
            i32be(p, at + 4)?,
            i32be(p, at + 8)?,
            i32be(p, at + 12)?,
        ));
        at += 16;
    }
    Ok(BadPixels {
        bayer_phase: phase,
        points,
        rectangles,
    })
}

fn estimate_same_color<F: Fn(usize, usize) -> bool>(
    pixels: &[u16],
    width: usize,
    height: usize,
    y: usize,
    x: usize,
    phase: u32,
    bad: F,
) -> Option<u16> {
    // DNG's phase IDs are 0=top-left red, 1=green on the red row,
    // 2=green on the blue row, 3=top-left blue.  In all four layouts the
    // green samples are the diagonal parity selected by this expression;
    // phase 0/3 therefore use axial neighbours and phase 1/2 diagonals.
    let green = ((y as u32 + x as u32 + phase + (phase >> 1)) & 1) == 1;
    let offsets: &[(isize, isize)] = if green {
        &[(-1, -1), (-1, 1), (1, -1), (1, 1)]
    } else {
        &[(-2, 0), (2, 0), (0, -2), (0, 2)]
    };
    let mut sum = 0u32;
    let mut n = 0u32;
    for &(dy, dx) in offsets {
        let yy = y as isize + dy;
        let xx = x as isize + dx;
        if yy < 0 || xx < 0 || yy >= height as isize || xx >= width as isize {
            continue;
        }
        let yy = yy as usize;
        let xx = xx as usize;
        if !bad(yy, xx) {
            sum += pixels[yy * width + xx] as u32;
            n += 1;
        }
    }
    (n > 0).then(|| ((sum + n / 2) / n) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_applies_radial_gain() {
        let mut p = Vec::new();
        for _ in 0..5 {
            p.extend_from_slice(&0.0f64.to_be_bytes());
        }
        p.extend_from_slice(&0.5f64.to_be_bytes());
        p.extend_from_slice(&0.5f64.to_be_bytes());
        let params = parse_vignette_radial(&p).unwrap();
        assert!((params.gain(0.0, 0.0, 3, 3).unwrap() - 1.0).abs() < 1e-12);
        let mut q = Vec::new();
        for value in [0.5f64, 0.0, 0.0, 0.0, 0.0] {
            q.extend_from_slice(&value.to_be_bytes());
        }
        q.extend_from_slice(&0.5f64.to_be_bytes());
        q.extend_from_slice(&0.5f64.to_be_bytes());
        let curved = parse_vignette_radial(&q).unwrap();
        assert!((curved.gain(0.0, 0.0, 3, 3).unwrap() - 1.5).abs() < 1e-12);
    }

    #[test]
    fn constant_bad_pixel_uses_same_colour_neighbours() {
        let mut px = vec![100u16; 25];
        px[12] = 0;
        let replacements = bad_pixel_constant_replacements(&px, 5, 5, 0, 0).unwrap();
        assert_eq!(replacements, vec![(12, 100)]);
    }

    #[test]
    fn clustered_list_is_explicitly_rejected() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        for n in [1i32, 1, 2, 2] {
            p.extend_from_slice(&n.to_be_bytes());
        }
        let list = parse_bad_pixels_list(&p).unwrap();
        assert!(matches!(
            list.replacement(&[1; 25], 5, 5, 2, 2),
            Err(OpcodeError::Unsupported(_))
        ));
    }

    #[test]
    fn listed_replacements_are_sorted_deduplicated_and_preserve_source() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&3u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        for (y, x) in [(3i32, 3i32), (2, 2), (3, 3)] {
            p.extend_from_slice(&y.to_be_bytes());
            p.extend_from_slice(&x.to_be_bytes());
        }
        let list = parse_bad_pixels_list(&p).unwrap();
        let source = vec![100u16; 49];
        let result = list.replacements(&source, 7, 7).unwrap();
        assert_eq!(result, vec![(16, 100), (24, 100)]);
        assert!(source.iter().all(|v| *v == 100));
    }

    #[test]
    fn listed_border_uses_available_neighbours_and_out_of_bounds_fails() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        for n in [1i32, 1] {
            p.extend_from_slice(&n.to_be_bytes());
        }
        let list = parse_bad_pixels_list(&p).unwrap();
        assert_eq!(list.replacements(&[1; 25], 5, 5).unwrap(), vec![(6, 1)]);
        let mut q = Vec::new();
        q.extend_from_slice(&0u32.to_be_bytes());
        q.extend_from_slice(&1u32.to_be_bytes());
        q.extend_from_slice(&0u32.to_be_bytes());
        for n in [9i32, 9] {
            q.extend_from_slice(&n.to_be_bytes());
        }
        let out = parse_bad_pixels_list(&q).unwrap();
        assert!(matches!(
            out.replacements(&[1; 25], 5, 5),
            Err(OpcodeError::Invalid(_))
        ));
    }

    #[test]
    fn maximum_dense_list_has_no_usable_neighbours() {
        let list = BadPixels {
            bayer_phase: 0,
            points: (0..256)
                .rev()
                .flat_map(|y| (0..256).map(move |x| (y, x)))
                .collect(),
            rectangles: vec![],
        };
        let source = vec![100; MAX_BAD_POINTS];
        let (patches, unresolved) = list
            .replacements_with_unresolved(&source, 256, 256)
            .unwrap();
        assert!(patches.is_empty());
        assert_eq!(unresolved, MAX_BAD_POINTS);
    }

    #[test]
    fn listed_point_with_no_available_same_colour_is_reported() {
        let mut p = Vec::new();
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&5u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        for (y, x) in [(2i32, 2i32), (1, 1), (1, 3), (3, 1), (3, 3)] {
            p.extend_from_slice(&y.to_be_bytes());
            p.extend_from_slice(&x.to_be_bytes());
        }
        let list = parse_bad_pixels_list(&p).unwrap();
        let (patches, unresolved) = list.replacements_with_unresolved(&[1; 25], 5, 5).unwrap();
        assert_eq!(patches.len(), 4);
        assert_eq!(unresolved, 1);
    }

    #[test]
    fn phase_selects_axial_or_diagonal_same_colour_neighbours() {
        let mut source = vec![0u16; 49];
        source[7 + 3] = 10;
        source[5 * 7 + 3] = 20;
        source[3 * 7 + 1] = 30;
        source[3 * 7 + 5] = 40;
        source[2 * 7 + 2] = 1;
        source[2 * 7 + 4] = 2;
        source[4 * 7 + 2] = 3;
        source[4 * 7 + 4] = 4;
        let axial = BadPixels {
            bayer_phase: 0,
            points: vec![(3, 3)],
            rectangles: vec![],
        };
        let diagonal = BadPixels {
            bayer_phase: 1,
            points: vec![(3, 3)],
            rectangles: vec![],
        };
        assert_eq!(axial.replacement(&source, 7, 7, 3, 3).unwrap(), Some(25));
        assert_eq!(diagonal.replacement(&source, 7, 7, 3, 3).unwrap(), Some(3));
    }
}
