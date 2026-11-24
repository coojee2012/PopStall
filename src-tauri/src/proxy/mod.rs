mod guest;
mod window;

pub use guest::GuestProxy;
pub use window::{attach_stage, close_stage_view, next_stage_label, place_stage};
