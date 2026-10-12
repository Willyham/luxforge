//! The harness's generative runtime: `luxforge-diffusion`'s binding over stable-diffusion.cpp and
//! ggml (`crates/luxforge-diffusion/src/native.rs`), compiled here from the editor's own source
//! file so the harness and the editor run one build of the pinned runtime. The harness does not
//! depend on `luxforge-diffusion` itself, which would bring the editor's core into its lockfile.
#[path = "../../../../crates/luxforge-diffusion/src/native.rs"]
mod native;

pub use native::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_links_and_reports_its_backends() {
        let info = system_info();
        assert!(!info.is_empty());
    }

    #[test]
    fn a_text_never_encoded_has_no_conditioning_to_save() {
        let dir = std::env::temp_dir().join("luxforge-diffusion-test-save");
        let error = save_condition("never encoded", &dir).unwrap_err();
        assert!(error.contains("no conditioning"), "{error}");
    }
}
