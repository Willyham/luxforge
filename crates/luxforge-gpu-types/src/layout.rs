//! Logical resource layout shared by planning and allocation. Policy budgets and device caps are
//! caller inputs. These bytes cover declared textures/parameters, never staging or total memory.
use crate::{BoundaryFormat, PARAMS_STRIDE, PlaneFormat, PlaneSize};
use std::collections::BTreeMap;

/// Physical texture format and anchored extent of a non-light plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlaneClass {
    pub format: PlaneFormat,
    pub size: PlaneSize,
}

impl PlaneClass {
    pub fn new(format: PlaneFormat, size: PlaneSize) -> Self {
        Self {
            format: format.kept_as(),
            size,
        }
    }

    pub fn extent(self, origin: (u32, u32), size: (u32, u32)) -> (u32, u32) {
        self.size.extent(origin, size)
    }

    pub fn bytes(self, origin: (u32, u32), size: (u32, u32)) -> u64 {
        texture_bytes(self.extent(origin, size), self.format.texel_bytes())
    }
}

/// Whether a plane is held across ticks, used only within a link, or an explicitly resolved light.
#[derive(Clone, Copy, Debug)]
pub enum PlaneRole {
    Kept(PlaneClass),
    Scratch(PlaneClass),
    Light(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlaneLocation {
    Kept(usize),
    Pool(PlaneClass, usize),
    Light(u32),
}

/// One link's kept textures and simultaneous scratch requirements, in plane order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkLayout {
    kept: Vec<PlaneClass>,
    scratch: BTreeMap<PlaneClass, usize>,
    lights: u32,
}

impl LinkLayout {
    pub fn push(&mut self, role: PlaneRole) -> PlaneLocation {
        match role {
            PlaneRole::Kept(class) => {
                let index = self.kept.len();
                self.kept.push(class);
                PlaneLocation::Kept(index)
            }
            PlaneRole::Scratch(class) => {
                let count = self.scratch.entry(class).or_default();
                let index = *count;
                *count += 1;
                PlaneLocation::Pool(class, index)
            }
            PlaneRole::Light(index) => {
                self.lights = self.lights.max(index + 1);
                PlaneLocation::Light(index)
            }
        }
    }

    pub fn kept_bytes(&self, origin: (u32, u32), size: (u32, u32), passes: usize) -> u64 {
        self.kept
            .iter()
            .map(|class| class.bytes(origin, size))
            .sum::<u64>()
            + parameter_bytes(passes)
    }

    pub fn scratch(&self) -> Vec<(PlaneClass, usize)> {
        self.scratch
            .iter()
            .map(|(&class, &count)| (class, count))
            .collect()
    }
}

/// Each class's maximum simultaneous count across links, and the explicitly resolved light count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PoolLayout {
    textures: BTreeMap<PlaneClass, usize>,
    pub lights: u32,
}

pub const LIGHT_BYTES: u64 = 16;
/// Six vec4 placement values shared by photo and GPU output presentation.
pub const PLACEMENT_BYTES: usize = 96;

impl PoolLayout {
    pub fn include(&mut self, link: &LinkLayout) {
        self.lights = self.lights.max(link.lights);
        self.include_scratch(link.scratch.iter().map(|(&class, &count)| (class, count)));
    }

    pub fn include_scratch(&mut self, scratch: impl IntoIterator<Item = (PlaneClass, usize)>) {
        for (class, count) in scratch {
            let held = self.textures.entry(class).or_default();
            *held = (*held).max(count);
        }
    }

    pub fn textures(&self) -> Vec<(PlaneClass, usize)> {
        self.textures
            .iter()
            .map(|(&class, &count)| (class, count))
            .collect()
    }

    pub fn bytes(&self, origin: (u32, u32), size: (u32, u32)) -> u64 {
        pool_bytes(
            self.textures.iter().map(|(&class, &count)| (class, count)),
            origin,
            size,
            self.lights,
        )
    }
}

pub fn pool_bytes(
    textures: impl IntoIterator<Item = (PlaneClass, usize)>,
    origin: (u32, u32),
    size: (u32, u32),
    lights: u32,
) -> u64 {
    textures
        .into_iter()
        .map(|(class, count)| count as u64 * class.bytes(origin, size))
        .sum::<u64>()
        + u64::from(lights) * LIGHT_BYTES
}

pub fn texture_bytes(size: (u32, u32), texel_bytes: u64) -> u64 {
    u64::from(size.0) * u64::from(size.1) * texel_bytes
}

pub fn boundary_bytes(size: (u32, u32), format: BoundaryFormat) -> u64 {
    texture_bytes(size, format.texel_bytes() as u64)
}

pub fn parameter_bytes(passes: usize) -> u64 {
    PARAMS_STRIDE * passes.max(1) as u64
}

/// Fixed parameter slices require a supported device alignment; no extra padding is introduced.
pub fn parameter_alignment_supported(alignment: u32) -> bool {
    alignment != 0 && PARAMS_STRIDE.is_multiple_of(u64::from(alignment))
}

/// Buffer allocation bucket, with the device binding cap and the existing minimum explicit.
pub fn buffer_capacity(bytes: u64, minimum: u64, binding_limit: u64) -> Result<u64, u64> {
    let limit = binding_limit & !3;
    if bytes > limit {
        return Err(limit);
    }
    Ok(bytes.max(minimum).next_power_of_two().min(limit))
}

/// Whole-output square bucket within the caller's texture side and byte ceiling.
pub fn full_capacity(size: (u32, u32), limit: u32, budget: u64) -> (u32, u32) {
    let longer = size.0.max(size.1);
    let edge = if longer <= 2048 {
        longer.next_power_of_two()
    } else {
        longer.next_multiple_of(512)
    };
    if edge <= limit && texture_bytes((edge, edge), 4) <= budget {
        (edge, edge)
    } else {
        size
    }
}

/// One-texture region output bucket within explicit device and byte limits.
pub fn region_capacity(size: (u32, u32), limit: u32, budget: u64) -> (u32, u32) {
    let reserved = (
        size.0.saturating_add(2).next_multiple_of(64),
        size.1.saturating_add(2).next_multiple_of(64),
    );
    if reserved.0 <= limit && reserved.1 <= limit && texture_bytes(reserved, 4) <= budget {
        reserved
    } else {
        size
    }
}

/// The geometry tail's intermediate precision, distinct from the source/boundary precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailFormat {
    Codes,
    Half,
    Float,
}

impl TailFormat {
    pub fn of(quantize: bool, preserve_f32: bool) -> Self {
        if quantize {
            Self::Codes
        } else if preserve_f32 {
            Self::Float
        } else {
            Self::Half
        }
    }
    pub fn texel_bytes(self) -> u64 {
        match self {
            Self::Codes => 4,
            Self::Half => 8,
            Self::Float => 16,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_keep_apply_planes_and_share_only_scratch_at_anchored_extents() {
        let scalar = PlaneClass::new(PlaneFormat::HalfScalar, PlaneSize::Reduced(4));
        let quad = PlaneClass::new(PlaneFormat::Quad, PlaneSize::Reduced(1));
        let mut first = LinkLayout::default();
        assert_eq!(
            first.push(PlaneRole::Scratch(scalar)),
            PlaneLocation::Pool(scalar, 0)
        );
        assert_eq!(
            first.push(PlaneRole::Scratch(scalar)),
            PlaneLocation::Pool(scalar, 1)
        );
        assert_eq!(first.push(PlaneRole::Kept(quad)), PlaneLocation::Kept(0));
        first.push(PlaneRole::Light(2));
        let mut second = LinkLayout::default();
        second.push(PlaneRole::Scratch(PlaneClass::new(
            PlaneFormat::Scalar,
            PlaneSize::Reduced(4),
        )));
        second.push(PlaneRole::Scratch(quad));
        let origin = (3, 5);
        let size = (8, 7);
        assert_eq!(scalar.extent(origin, size), (3, 2));
        assert_eq!(first.kept_bytes(origin, size, 3), 8 * 7 * 16 + 3 * 256);
        let mut pool = PoolLayout::default();
        pool.include(&first);
        pool.include(&second);
        assert_eq!(
            pool.bytes(origin, size),
            2 * 3 * 2 * 4 + 8 * 7 * 16 + 3 * 16
        );
        assert_eq!(pool.textures(), vec![(scalar, 2), (quad, 1)]);
        assert_eq!(parameter_bytes(0), 256);
    }

    #[test]
    fn buckets_obey_explicit_side_binding_and_byte_limits() {
        assert_eq!(full_capacity((1700, 1000), 8192, 512 << 20), (2048, 2048));
        assert_eq!(full_capacity((2100, 1000), 8192, 512 << 20), (2560, 2560));
        assert_eq!(full_capacity((1700, 1000), 1800, 512 << 20), (1700, 1000));
        assert_eq!(full_capacity((1700, 1000), 8192, 1024), (1700, 1000));
        assert_eq!(region_capacity((64, 127), 8192, 32 << 20), (128, 192));
        assert_eq!(region_capacity((64, 127), 128, 32 << 20), (64, 127));
        assert_eq!(region_capacity((64, 127), 8192, 1024), (64, 127));
        assert_eq!(buffer_capacity(1200, 1024, 10000), Ok(2048));
        assert_eq!(buffer_capacity(9000, 1024, 10001), Ok(10000));
        assert_eq!(buffer_capacity(10001, 1024, 10001), Err(10000));
        for alignment in [1, 16, 64, 128, 256] {
            assert!(parameter_alignment_supported(alignment));
        }
        for alignment in [0, 3, 512] {
            assert!(!parameter_alignment_supported(alignment));
        }
    }
}
