//! Iced presentation policy for the shared GPU executor on Iced's existing device.
use super::{PhotoPipeline, Picture, SurfaceSlots, Tile, TileLayout};
#[cfg(test)]
use luxforge_gpu::qualification::{BLOCK_CHUNK, MIN_BUFFER, OUTPUT_FORMAT, assemble, validate};
pub use luxforge_gpu::*;
#[cfg(test)]
use luxforge_gpu_types::{MAP_WORDS, STEP_WORDS};
use std::sync::Mutex;
#[cfg(test)]
use std::{
    borrow::Cow,
    sync::{Arc, atomic::Ordering},
};
#[cfg(test)]
mod blocks {
    pub use luxforge_gpu::qualification::block_len;
}
#[cfg(test)]
mod chain {
    pub use luxforge_gpu::qualification::{chain, pack_steps};
}
#[cfg(test)]
mod mask {
    pub use luxforge_gpu::qualification::changed_ranges;
}
#[cfg(test)]
mod rest {
    pub use luxforge_gpu::qualification::RestPasses;
}

mod dissolve;
pub use dissolve::{DISSOLVE_DURATION, Dissolve, DrawnDissolve};
pub(crate) use dissolve::{DissolveFrame, dissolving, photo_uniform};
#[cfg(any(test, feature = "qualification"))]
pub mod headless;
#[cfg(any(test, feature = "qualification"))]
pub mod qualification;
pub mod tiles;

impl PhotoPipeline {
    #[cfg(test)]
    fn without_gpu_stage(mut self) -> Self {
        self.executor.disable_gpu_stage();
        self
    }

    pub(super) fn prepare_gpu(
        &mut self,
        surface: &mut SurfaceSlots,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: Option<&GpuPlan>,
        dissolve: Option<DissolveFrame>,
        change: Option<GpuChange>,
    ) {
        // The GPU frame the last draw showed, which is all a dissolve may start from.
        let shown = surface.gpu_output().is_some() || surface.dissolving.is_some();
        let output_shown = surface.gpu_output().is_some();
        surface.gpu_outcome = None;
        surface.gpu_held_frame = false;
        surface.dissolving = None;
        let Some(plan) = plan else {
            match dissolve {
                Some(frame) if shown && surface.gpu.is_some() => surface.dissolving = Some(frame),
                _ => self.release_gpu(surface),
            }
            self.sync_outputs(surface, device);
            return;
        };
        let outcome = self
            .executor
            .evaluate_lit(&mut surface.state, device, queue, plan, change);
        // A sequence still compiling leaves the slot as it was, its boundary included, for the
        // frame that finds the pipeline ready, and a boundary still uploading leaves it for the
        // frame that writes the next chunks; any other fallback lets the slot go.
        // A light the picture at rest computes leaves it as it was too, and the GPU frame on
        // screen stays until the plan is drawn with that light.
        if outcome.is_err()
            && !matches!(
                outcome,
                Err(GpuFallback::Compiling
                    | GpuFallback::BoundaryUploading { .. }
                    | GpuFallback::LightPending)
            )
        {
            self.release_gpu(surface);
        }
        surface.gpu_held_frame = output_shown && matches!(outcome, Err(GpuFallback::LightPending));
        // A dissolve handed beside a plan runs behind it while it is held: the slot keeps the
        // output of the GPU frame the last draw showed, which the plan's unchanged words leave as
        // it was.
        if outcome.is_ok() && shown {
            surface.dissolving = dissolve;
        }
        surface.gpu_outcome = Some(outcome);
        self.sync_outputs(surface, device);
    }

    pub(super) fn release_gpu(&self, surface: &mut SurfaceSlots) {
        self.executor.release_gpu(&mut surface.state);
        surface.gpu_picture = None;
    }
    pub(super) fn release_rest(&self, surface: &mut SurfaceSlots) {
        self.executor.release_rest(&mut surface.state);
        surface.rest_picture = None;
    }
    pub(super) fn prepare_rest(
        &mut self,
        surface: &mut SurfaceSlots,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rest: Option<&GpuRest>,
    ) {
        self.executor
            .prepare_rest(&mut surface.state, device, queue, rest);
        self.sync_outputs(surface, device);
    }
    pub(super) fn count_tick(
        &mut self,
        surface: &mut SurfaceSlots,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: Option<&GpuPlan>,
    ) {
        let frame = match (plan, surface.gpu_outcome, surface.gpu_tag) {
            (Some(plan), Some(Ok(boundary)), Some(tag)) => Some(CountedFrame {
                plan,
                boundary,
                tag,
            }),
            _ => None,
        };
        self.executor
            .count_tick(&mut surface.state, device, queue, frame);
    }
    fn sync_outputs(&self, surface: &mut SurfaceSlots, device: &wgpu::Device) {
        self.sync_picture(
            device,
            &mut surface.gpu_picture,
            surface.state.gpu.as_ref().map(GpuSlot::output),
        );
        self.sync_picture(
            device,
            &mut surface.rest_picture,
            surface.state.rest_output(),
        );
    }
    fn sync_picture(
        &self,
        device: &wgpu::Device,
        held: &mut Option<Picture>,
        output: Option<&Output>,
    ) {
        let Some(output) = output else {
            *held = None;
            return;
        };
        if held.as_ref().is_none_or(|picture| {
            picture.tiles.len() != output.tiles.len()
                || picture
                    .tiles
                    .iter()
                    .zip(&output.tiles)
                    .any(|(tile, output)| tile.texture != output.texture)
        }) {
            let tiles = output
                .tiles
                .iter()
                .map(|output| {
                    let view = output.texture.create_view(&wgpu::TextureViewDescriptor {
                        format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                        usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
                        ..Default::default()
                    });
                    Tile {
                        layout: TileLayout::whole((
                            output.texture.width(),
                            output.texture.height(),
                        )),
                        capacity: (output.texture.width(), output.texture.height()),
                        texture: output.texture.clone(),
                        uniform: output.uniform.clone(),
                        written_uniform: Mutex::new(None),
                        bindings: super::photo_bindings(
                            device,
                            &self.layout,
                            &output.uniform,
                            &view,
                            &self.linear,
                        ),
                    }
                })
                .collect();
            *held = Some(Picture {
                tiles,
                width: output.width,
                height: output.height,
                capacity: output.capacity,
                grid: (1, 1),
                limit: device.limits().max_texture_dimension_2d,
                version: output.version,
                content_id: None,
                region_key: None,
                allocated_bytes: u64::from(output.capacity.0) * u64::from(output.capacity.1) * 4,
                mip_levels: 1,
                mip_bytes: 0,
                mips_current: false,
            });
        }
        let picture = held.as_mut().expect("bound above");
        picture.width = output.width;
        picture.height = output.height;
        picture.version = output.version;
        picture.capacity = output.capacity;
        picture.region_key = output.region_key.map(|region| super::RegionKey {
            rect: region.rect,
            stage: region.stage,
            full_stage: region.full_stage,
            content_id: 0,
            generation: 0,
        });
        for tile in &mut picture.tiles {
            tile.layout = TileLayout::whole((output.width, output.height));
        }
    }
}

#[cfg(test)]
mod compile_tests;
#[cfg(test)]
mod tail_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub mod clipping;
#[cfg(test)]
pub mod timing;
