//! The worker's one cached proxy source.

use crate::{Error, PreviewSource, ProxyIdentity, ProxyPlan};

/// One cached proxy source's identity: the pixels it came from and the plan it was built to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProxyKey {
    pub identity: ProxyIdentity,
    pub plan: ProxyPlan,
}

/// One cached proxy source. Bounded by construction: at most one entry, so the memory a cache can
/// hold is one proxy and a new plan replaces the old one rather than accumulating beside it.
#[derive(Default)]
pub(crate) struct ProxyCache {
    entry: Option<(ProxyKey, PreviewSource)>,
}

impl ProxyCache {
    /// The proxy source for `key`: on a hit, the held one's pixels under the evaluation settings
    /// of `job` ([`PreviewSource::with_settings_of`]) and `false`; on a miss, what `build` returns,
    /// which the cache then holds, and `true`. A miss releases the held proxy before `build` runs,
    /// so a replacement never sits beside the proxy it replaces at the build's peak. A build that
    /// fails or is cancelled leaves the cache empty, which the next job reads as a miss and
    /// rebuilds; nothing but the preview worker that owns the cache ever reads it.
    pub(crate) fn source_for(
        &mut self,
        key: &ProxyKey,
        job: &PreviewSource,
        build: impl FnOnce() -> Result<PreviewSource, Error>,
    ) -> Result<(PreviewSource, bool), Error> {
        if let Some((held, source)) = &self.entry
            && held == key
        {
            return Ok((source.with_settings_of(job), false));
        }
        self.entry = None;
        let built = build()?;
        self.entry = Some((key.clone(), built.clone()));
        Ok((built, true))
    }
}
