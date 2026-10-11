//! the PlayStation Vita's picture: for now only showing the framebuffer a
//! game draws itself, in a window, where GXM's drawing will join it.

pub mod backend;

pub use backend::{layout, Overlay, PresentError, Presenter, ScreenImage, Viewport};
