//! OpenGL presentation backend.

use glow::HasContext;

use super::{layout, Overlay, PresentError, Presenter, ScreenImage, Viewport};

const VERTEX_SHADER: &str = r#"#version 330 core
// a single oversized triangle covers the viewport with no vertex buffer.
out vec2 uv;
void main() {
    vec2 position = vec2((gl_VertexID << 1) & 2, gl_VertexID & 2);
    uv = vec2(position.x, 1.0 - position.y);
    gl_Position = vec4(position * 2.0 - 1.0, 0.0, 1.0);
}
"#;

// a screen bigger than the window shows it is averaged over every texel a
// pixel covers.
const FRAGMENT_SHADER: &str = r#"#version 330 core
in vec2 uv;
out vec4 color;
uniform sampler2D screen;
void main() {
    vec2 size = vec2(textureSize(screen, 0));
    vec2 covered = fwidth(uv) * size;
    if (max(covered.x, covered.y) <= 1.001) {
        color = vec4(texture(screen, uv).rgb, 1.0);
        return;
    }
    ivec2 taps = ivec2(clamp(ceil(covered), 1.0, 4.0));
    vec3 sum = vec3(0.0);
    for (int y = 0; y < taps.y; y++) {
        for (int x = 0; x < taps.x; x++) {
            vec2 offset = ((vec2(x, y) + 0.5) / vec2(taps) - 0.5) * covered / size;
            sum += texture(screen, uv + offset).rgb;
        }
    }
    color = vec4(sum / float(taps.x * taps.y), 1.0);
}
"#;

pub struct GlPresenter {
    gl: glow::Context,
    program: glow::Program,
    vertex_array: glow::VertexArray,
    texture: glow::Texture,
    /// the size the texture was last allocated at, so uploads can use
    /// tex_sub_image_2d when nothing changed.
    size: (u32, u32),
    window: (u32, u32),
}

impl GlPresenter {
    /// loader resolves OpenGL function names, as the windowing library
    /// provides.
    ///
    /// # Safety
    ///
    /// A current OpenGL 3.3 context must be bound on this thread.
    pub unsafe fn new(
        loader: impl FnMut(&str) -> *const std::ffi::c_void,
        window: (u32, u32),
    ) -> Result<GlPresenter, PresentError> {
        // SAFETY: the caller made a context current
        let gl = unsafe { glow::Context::from_loader_function(loader) };
        // SAFETY: as above, for every call on gl below
        unsafe {
            let version = gl.get_parameter_string(glow::VERSION);
            let renderer = gl.get_parameter_string(glow::RENDERER);
            log::info!("OpenGL {version} on {renderer}");

            let program = link_program(&gl, VERTEX_SHADER, FRAGMENT_SHADER)?;
            let vertex_array = gl.create_vertex_array().map_err(PresentError::Backend)?;
            let texture = gl.create_texture().map_err(PresentError::Backend)?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            // linear, the screen is shown at whatever size the window is
            for (parameter, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, parameter, value as i32);
            }
            Ok(GlPresenter { gl, program, vertex_array, texture, size: (0, 0), window })
        }
    }

    fn upload(&mut self, image: &ScreenImage<'_>) {
        let gl = &self.gl;
        let pixels = glow::PixelUnpackData::Slice(Some(image.pixels));
        // SAFETY: the context this presenter was made with is current
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            if self.size != (image.width, image.height) {
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    image.width as i32,
                    image.height as i32,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    pixels,
                );
                self.size = (image.width, image.height);
            } else {
                let (width, height) = (image.width as i32, image.height as i32);
                gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, width, height, glow::RGBA, glow::UNSIGNED_BYTE, pixels);
            }
        }
    }

    fn draw(&self, viewport: Viewport) {
        let gl = &self.gl;
        // SAFETY: the context this presenter was made with is current
        unsafe {
            // OpenGL's origin is the bottom left, so the y coordinate flips.
            let y = self.window.1 as f32 - viewport.y - viewport.height;
            gl.viewport(viewport.x as i32, y as i32, viewport.width as i32, viewport.height as i32);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vertex_array));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
    }
}

impl Presenter for GlPresenter {
    fn name(&self) -> &'static str {
        "opengl"
    }

    fn present(&mut self, screen: ScreenImage<'_>, _overlay: &Overlay) -> Result<(), PresentError> {
        // SAFETY: the context this presenter was made with is current
        unsafe {
            self.gl.viewport(0, 0, self.window.0 as i32, self.window.1 as i32);
            self.gl.clear_color(0.0, 0.0, 0.0, 1.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
        if !screen.is_empty() {
            self.upload(&screen);
            self.draw(layout(self.window.0, self.window.1, false));
        }
        Ok(())
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.window = (width.max(1), height.max(1));
    }
}

impl Drop for GlPresenter {
    fn drop(&mut self) {
        // SAFETY: the context is still current while the presenter goes
        unsafe {
            self.gl.delete_program(self.program);
            self.gl.delete_vertex_array(self.vertex_array);
            self.gl.delete_texture(self.texture);
        }
    }
}

/// compiles and links a vertex and a fragment shader into a program.
///
/// # Safety
///
/// gl's context must be current.
unsafe fn link_program(gl: &glow::Context, vertex: &str, fragment: &str) -> Result<glow::Program, PresentError> {
    // SAFETY: as the caller promises
    unsafe {
        let program = gl.create_program().map_err(PresentError::Backend)?;
        let mut shaders = Vec::new();
        for (kind, source) in [(glow::VERTEX_SHADER, vertex), (glow::FRAGMENT_SHADER, fragment)] {
            let shader = gl.create_shader(kind).map_err(PresentError::Backend)?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                return Err(PresentError::Backend(gl.get_shader_info_log(shader)));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(PresentError::Backend(gl.get_program_info_log(program)));
        }
        for shader in shaders {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }
        Ok(program)
    }
}
