//! presentation backends.

#[cfg(feature = "opengl")]
pub mod gl;

/// the screen's pixels, straight RGBA8.
pub struct ScreenImage<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [u8],
}

impl ScreenImage<'_> {
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.pixels.is_empty()
    }
}

/// what is drawn over the screen, a user interface, which there is none of
/// yet.
#[derive(Default)]
pub struct Overlay {}

/// where the screen goes in the window, in pixels from the top left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// the console's screen.
pub const SCREEN: (u32, u32) = (960, 544);

/// where the screen goes inside a window, keeping the console's aspect
/// ratio and centred, scaled by a whole number when integer asks and the
/// window has room.
pub fn layout(window_width: u32, window_height: u32, integer: bool) -> Viewport {
    let (width, height) = (SCREEN.0 as f32, SCREEN.1 as f32);
    let mut scale = (window_width as f32 / width).min(window_height as f32 / height);
    if integer && scale >= 1.0 {
        scale = scale.floor();
    }
    let mut x = (window_width as f32 - width * scale) / 2.0;
    let mut y = (window_height as f32 - height * scale) / 2.0;
    if integer {
        // whole pixels, or every texel would not get the same size
        x = x.floor();
        y = y.floor();
    }
    Viewport { x, y, width: width * scale, height: height * scale }
}

#[derive(Debug, thiserror::Error)]
pub enum PresentError {
    #[error("{0}")]
    Backend(String),
}

pub trait Presenter {
    fn name(&self) -> &'static str;

    /// draws the screen into the window, and the overlay over it.
    fn present(&mut self, screen: ScreenImage<'_>, overlay: &Overlay) -> Result<(), PresentError>;

    /// the window changed size.
    fn resize(&mut self, width: u32, height: u32);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_keeps_the_aspect_ratio_and_centers() {
        // the console's size needs no scaling or offset
        assert_eq!(layout(960, 544, false), Viewport { x: 0.0, y: 0.0, width: 960.0, height: 544.0 });
        // twice as big is twice the scale
        assert_eq!(layout(1920, 1088, false).width, 1920.0);
        // a window too wide letterboxes at the sides
        let wide = layout(1200, 544, false);
        assert_eq!((wide.x, wide.width), (120.0, 960.0));
        // 1.5 times fits, once is used, and the rest is border
        let integer = layout(1440, 816, true);
        assert_eq!((integer.width, integer.x, integer.y), (960.0, 240.0, 136.0));
    }
}
