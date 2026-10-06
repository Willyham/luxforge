//! The stage holder (`docs/design/gpu-preview.md`, "The picture at rest"; TASK-012's staged
//! sweeps): one link's output over a whole content stage, held across the tiles of a sweep so the
//! next sweep cuts its boundaries from it instead of drawing every earlier link again over each
//! tile's window. A staged picture at rest draws through it ([`super::rest`]).
//!
//! - **Textures.** The stage in at most [`STAGE_TEXTURES`] × [`STAGE_TEXTURES`] textures of the
//!   device's largest side, at most 8,192 pixels, row by row, as the source is held: a stage past
//!   two textures either way is refused, `texture-limit`, before anything is created.
//! - **In and out.** A tile's rectangle of the stage is copied in from the texture a link drew it
//!   into ([`StageHolder::copy_in`]), and a window of the stage copied out into a boundary
//!   ([`StageHolder::copy_out`]), each split at the textures' seams; texture copies alone, no
//!   shader, so what comes out is what went in, bit for bit.
//! - **Bounds.** Charged before it is created ([`StageHolder::charge`], [`StageHolder::create`]),
//!   and its textures retire with its owner ([`StageHolder::into_textures`]).
use super::GpuFallback;

/// The most textures a holder takes across and down.
pub const STAGE_TEXTURES: u32 = 2;

/// The largest side of one of its textures, whatever the device allows past it.
const STAGE_SIDE: u32 = 8192;

/// What a holder's textures are created with: copied into and out of, and bound where a later
/// sweep reads it.
const STAGE_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::COPY_SRC
    .union(wgpu::TextureUsages::COPY_DST)
    .union(wgpu::TextureUsages::TEXTURE_BINDING);

/// A content stage of one format held in at most 2 × 2 textures ([module documentation](self)).
pub struct StageHolder {
    stage: (u32, u32),
    side: u32,
    /// Its textures across and down.
    grid: (u32, u32),
    /// Row by row.
    textures: Vec<wgpu::Texture>,
    bytes: u64,
}

/// The rectangle `[x0, y0, x1, y1)` of one texture of the grid: `(column, row)`.
fn cell(stage: (u32, u32), side: u32, (column, row): (u32, u32)) -> [u32; 4] {
    [
        column * side,
        row * side,
        ((column + 1) * side).min(stage.0),
        ((row + 1) * side).min(stage.1),
    ]
}

/// What `a` and `b`, half-open rectangles, share; `None` when nothing.
fn meet(a: [u32; 4], b: [u32; 4]) -> Option<[u32; 4]> {
    let met = [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ];
    (met[0] < met[2] && met[1] < met[3]).then_some(met)
}

fn extent(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}

impl StageHolder {
    /// The side of its textures on a device whose largest texture is `limit`, and how many it takes
    /// across and down for `stage`; `texture-limit` past [`STAGE_TEXTURES`] either way.
    fn layout(stage: (u32, u32), limit: u32) -> Result<(u32, (u32, u32)), GpuFallback> {
        let side = limit.clamp(1, STAGE_SIDE);
        let grid = (stage.0.div_ceil(side), stage.1.div_ceil(side));
        if stage.0 == 0 || stage.1 == 0 || grid.0 > STAGE_TEXTURES || grid.1 > STAGE_TEXTURES {
            return Err(GpuFallback::TextureLimit {
                width: stage.0,
                height: stage.1,
                limit: side * STAGE_TEXTURES,
            });
        }
        Ok((side, grid))
    }

    /// What a holder of `stage` in `format` takes on a device whose largest texture is `limit`: its
    /// texels, each texture exactly the part of the stage it holds. Creates nothing.
    pub fn charge(
        stage: (u32, u32),
        format: wgpu::TextureFormat,
        limit: u32,
    ) -> Result<u64, GpuFallback> {
        Self::layout(stage, limit)?;
        let texel = u64::from(format.block_copy_size(None).unwrap_or(16));
        Ok(u64::from(stage.0) * u64::from(stage.1) * texel)
    }

    /// A holder of `stage` in `format` on `device`, its bytes passed to `charge` before anything is
    /// created: refused, having created nothing, by the stage's size or by the charge.
    pub fn create<E: From<GpuFallback>>(
        device: &wgpu::Device,
        stage: (u32, u32),
        format: wgpu::TextureFormat,
        charge: impl FnOnce(u64) -> Result<(), E>,
    ) -> Result<Self, E> {
        let limit = device.limits().max_texture_dimension_2d;
        let (side, grid) = Self::layout(stage, limit)?;
        let bytes = Self::charge(stage, format, limit)?;
        charge(bytes)?;
        let mut textures = Vec::with_capacity((grid.0 * grid.1) as usize);
        for row in 0..grid.1 {
            for column in 0..grid.0 {
                let [x0, y0, x1, y1] = cell(stage, side, (column, row));
                textures.push(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("luxforge.gpu_preview.stage"),
                    size: extent(x1 - x0, y1 - y0),
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: STAGE_USAGE,
                    view_formats: &[],
                }));
            }
        }
        Ok(Self {
            stage,
            side,
            grid,
            textures,
            bytes,
        })
    }

    /// The stage it holds.
    pub fn stage(&self) -> (u32, u32) {
        self.stage
    }

    /// What it was charged.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Each texture with the rectangle of the stage it holds, row by row.
    fn cells(&self) -> impl Iterator<Item = (&wgpu::Texture, [u32; 4])> {
        let (stage, side, columns) = (self.stage, self.side, self.grid.0);
        self.textures.iter().enumerate().map(move |(at, texture)| {
            let at = at as u32;
            (texture, cell(stage, side, (at % columns, at / columns)))
        })
    }

    /// Copy `rect` (`[x0, y0, x1, y1)` of the stage) in on `encoder` from `from`, whose texel
    /// `origin` holds the rectangle's first pixel: a tile's rectangle of the link's output, from
    /// the texture it was drawn into past its halo. Split at the seams; the part past the stage
    /// is not copied.
    pub fn copy_in(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        from: &wgpu::Texture,
        origin: (u32, u32),
        rect: [u32; 4],
    ) {
        for (texture, held) in self.cells() {
            let Some([x0, y0, x1, y1]) = meet(rect, held) else {
                continue;
            };
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: from,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: origin.0 + (x0 - rect[0]),
                        y: origin.1 + (y0 - rect[1]),
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: x0 - held[0],
                        y: y0 - held[1],
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                extent(x1 - x0, y1 - y0),
            );
        }
    }

    /// Copy `window` (`[x, y, width, height]` of the stage) out on `encoder` into `to` at its
    /// first texel: the boundary a later sweep's tile reads. Split at the seams; the part past the
    /// stage is not copied.
    pub fn copy_out(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        to: &wgpu::Texture,
        window: [u32; 4],
    ) {
        let [x, y, width, height] = window;
        let rect = [x, y, x + width, y + height];
        for (texture, held) in self.cells() {
            let Some([x0, y0, x1, y1]) = meet(rect, held) else {
                continue;
            };
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: x0 - held[0],
                        y: y0 - held[1],
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: to,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: x0 - x,
                        y: y0 - y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                extent(x1 - x0, y1 - y0),
            );
        }
    }

    /// Its textures, to retire with its owner, still charged until the GPU is done with them.
    pub fn into_textures(self) -> Vec<wgpu::Texture> {
        self.textures
    }
}

#[cfg(test)]
#[path = "staged_tests.rs"]
mod tests;
