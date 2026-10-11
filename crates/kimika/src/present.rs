//! wiring a presentation backend to a winit window.

use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextApi, ContextAttributesBuilder, PossiblyCurrentContext, Version};
use glutin::display::GetGlDisplay;
use glutin::prelude::*;
use glutin::surface::{Surface, SwapInterval, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow};
use kimika_gpu::backend::gl::GlPresenter;
use kimika_gpu::{Overlay, PresentError, Presenter, ScreenImage};
use raw_window_handle::HasWindowHandle;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes};

pub enum Backend {
    OpenGl { presenter: GlPresenter, context: PossiblyCurrentContext, surface: Surface<WindowSurface> },
}

impl Backend {
    pub fn create(event_loop: &ActiveEventLoop, attributes: WindowAttributes) -> Result<(Window, Backend), String> {
        create_opengl(event_loop, attributes)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Backend::OpenGl { presenter, .. } => presenter.name(),
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        match self {
            Backend::OpenGl { presenter, context, surface } => {
                presenter.resize(width, height);
                surface.resize(
                    context,
                    std::num::NonZeroU32::new(width).unwrap(),
                    std::num::NonZeroU32::new(height).unwrap(),
                );
            }
        }
    }

    pub fn present(&mut self, screen: ScreenImage<'_>, overlay: &Overlay) -> Result<(), PresentError> {
        match self {
            Backend::OpenGl { presenter, context, surface } => {
                presenter.present(screen, overlay)?;
                surface.swap_buffers(context).map_err(|error| PresentError::Backend(error.to_string()))
            }
        }
    }
}

fn create_opengl(event_loop: &ActiveEventLoop, attributes: WindowAttributes) -> Result<(Window, Backend), String> {
    let template = ConfigTemplateBuilder::new().with_alpha_size(8);
    let (window, config) = DisplayBuilder::new()
        .with_window_attributes(Some(attributes))
        .build(event_loop, template, |configs| {
            // any config will do, prefer more samples if offered.
            configs
                .reduce(|best, candidate| if candidate.num_samples() > best.num_samples() { candidate } else { best })
                .expect("at least one GL config")
        })
        .map_err(|error| format!("no OpenGL configuration: {error}"))?;

    let window = window.ok_or("glutin did not create a window")?;
    let raw_handle = window.window_handle().map_err(|error| error.to_string())?.as_raw();

    let display = config.display();
    // ask for 3.3 core, which is what the presentation shaders need.
    let context_attributes = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 3))))
        .build(Some(raw_handle));
    // SAFETY: the window the context is for outlives it, the Backend being
    // dropped before the window
    let not_current = unsafe { display.create_context(&config, &context_attributes) }
        .map_err(|error| format!("could not create an OpenGL 3.3 context: {error}"))?;

    let surface_attributes = window.build_surface_attributes(Default::default()).map_err(|error| error.to_string())?;
    // SAFETY: as above, for the surface
    let surface = unsafe { display.create_window_surface(&config, &surface_attributes) }
        .map_err(|error| format!("could not create a window surface: {error}"))?;

    let context =
        not_current.make_current(&surface).map_err(|error| format!("could not make the context current: {error}"))?;

    // present on vblank so the frontend's own pacing has something to work
    // against, a failure here is not fatal.
    let _ = surface.set_swap_interval(&context, SwapInterval::Wait(std::num::NonZeroU32::new(1).unwrap()));

    let size = window.inner_size();
    // SAFETY: the context made current just above
    let presenter = unsafe {
        GlPresenter::new(
            |name| {
                let name = std::ffi::CString::new(name).unwrap();
                display.get_proc_address(&name)
            },
            (size.width.max(1), size.height.max(1)),
        )
    }
    .map_err(|error| error.to_string())?;

    Ok((window, Backend::OpenGl { presenter, context, surface }))
}
