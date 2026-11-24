mod guest;
mod media;
mod window;

pub use guest::GuestProxy;

pub(crate) const PROXY_CSP: &str = "default-src 'self' data: blob:; base-uri 'self'; form-action 'self'; frame-ancestors 'none'; object-src 'none'; img-src 'self' data: blob:; media-src 'self' blob: data:; font-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline' 'unsafe-eval' blob:; connect-src 'self'; frame-src 'self'; worker-src 'self' blob:";
pub use window::{attach_stage, close_stage_view, next_stage_label, place_stage};
