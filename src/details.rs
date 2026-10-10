//! Explicit transient access to upstream results, separate from verified facts.
use crate::payments::Private;

/// How much provider information an authenticated caller requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Disclosure {
    /// Return normalized facts and any necessary member action only.
    #[default]
    Minimal,
    /// Also return complete available upstream payloads in private RAM buffers.
    Complete,
}

/// One available upstream response. Its fields are assertions, not verified proof.
/// This value deliberately has neither serialization nor revealing diagnostics.
pub struct ProviderReply {
    /// Adapter-defined phase, such as `verify`, `settle` or `response`.
    pub phase: &'static str,
    /// Status code when the upstream transport exposes it.
    pub http_status: Option<u16>,
    /// Complete available payload, including provider extensions and diagnostics.
    /// Bytes may be malformed or non-JSON; the caller must treat them as untrusted.
    pub payload: Private,
}

impl std::fmt::Debug for ProviderReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderReply").finish_non_exhaustive()
    }
}
