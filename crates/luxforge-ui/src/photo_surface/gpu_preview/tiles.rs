//! Desktop entry point during executor extraction; request policy stays with its host.
pub use luxforge_gpu::tiles::*;
pub struct TileRunner(luxforge_gpu::tiles::TileRunner);
impl TileRunner {
    pub fn open(backend: &str, name: &str) -> Result<Self, TileRefusal> {
        luxforge_gpu::tiles::TileRunner::open_with(
            luxforge_gpu::gpu_stage_refused(),
            backend,
            name,
            crate::adapters::renderer_backends(),
            &crate::adapters::renderer_limits(),
        )
        .map(Self)
    }
}
impl std::ops::Deref for TileRunner {
    type Target = luxforge_gpu::tiles::TileRunner;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for TileRunner {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
