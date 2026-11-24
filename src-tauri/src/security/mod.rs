//! Path, SSRF, and bandwidth boundaries for the Phase 1 loop.

mod limit;
mod path;
mod ssrf;

pub use limit::{
    BANDWIDTH_BYTES_PER_SEC, BandwidthLimit, MAX_MEDIA_BYTES, MAX_REQUEST_BODY, MAX_RESPONSE_BYTES,
};
pub use path::{ResolvedPath, SafePathError, resolve_within_root, validate_target};
pub use ssrf::{is_restricted_host, navigation_allowed};

pub const MAX_VISITORS: usize = 20;
pub const MAX_STREAMS_PER_VISITOR: usize = 32;
pub const MAX_TARGET_LEN: usize = 4096;
