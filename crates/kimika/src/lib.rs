//! Kimika's frontend: a window showing what the game draws, the keyboard as
//! its controls, and the console's 59.94 vertical blanks a second, which the
//! game paces itself by, raised from the window's own loop.

mod cli;
mod input;
mod present;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kimika_core::{Config, System};
use kimika_gpu::backend::SCREEN;
use kimika_gpu::{Overlay, ScreenImage};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use input::Keyboard;
use present::Backend;

/// one vertical blank of the console's display, 59.94 a second.
const FRAME_TIME: Duration = Duration::from_nanos(1_001_000_000 / 60);
/// how far behind the schedule may fall before it starts over instead of
/// running fast to catch up, after the window was dragged say.
const CATCH_UP_LIMIT: Duration = Duration::from_millis(200);

/// runs the emulator as the command line says.
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let options = match cli::parse() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("kimika: {message}");
            std::process::exit(2);
        }
    };
    let Some(game) = options.game.clone() else {
        eprintln!("{}", cli::USAGE);
        std::process::exit(2);
    };
    let data_dir = options.data.as_ref().map(PathBuf::from).or_else(default_data_dir);
    if let Some(data_dir) = &data_dir {
        log::info!("keeping ux0: and the saves in {}", data_dir.display());
    }
    let system = match kimika_core::load(Path::new(&game), Config { data_dir, ..Config::default() }) {
        Ok(system) => system,
        Err(error) => {
            eprintln!("kimika: could not load {game}, {error}");
            std::process::exit(1);
        }
    };
    if let Some(frames) = options.headless {
        run_headless(&system, frames);
        return;
    }

    let name = Path::new(&game).file_stem().map_or_else(|| game.clone(), |stem| stem.to_string_lossy().into_owned());
    let mut app = App {
        system,
        name,
        scale: options.scale.unwrap_or(1),
        keyboard: Keyboard::default(),
        backend: None,
        window: None,
        next_frame: Instant::now(),
        last_title_update: Instant::now(),
    };
    let event_loop = EventLoop::new().expect("create an event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    if let Err(error) = event_loop.run_app(&mut app) {
        eprintln!("kimika: {error}");
    }
    app.system.stop();
    report(&app.system);
}

/// kimika in the user's data folder: XDG_DATA_HOME or ~/.local/share on
/// unix, APPDATA on Windows.
fn default_data_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    };
    base.map(|base| base.join("kimika"))
}

/// runs the game for so many frames with no window, then says how it went.
fn run_headless(system: &System, frames: u64) {
    let start = Instant::now();
    let mut next = start;
    for _ in 0..frames {
        next += FRAME_TIME;
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        system.vblank();
        if system.outcome().is_some() {
            break;
        }
    }
    system.stop();
    println!("{:?} after {:.2?}, {}", system.outcome(), start.elapsed(), system.status_line());
    report(system);
}

/// what the game called that is not done yet, and where it reached into
/// nothing.
fn report(system: &System) {
    for (library, nid, count) in system.unimplemented() {
        let (library_name, name) = kimika_core::import_name(library, nid);
        log::warn!("not done yet: {library_name}::{name} (0x{library:08X} 0x{nid:08X}), called {count} times");
    }
}

struct App {
    system: System,
    name: String,
    scale: u32,
    keyboard: Keyboard,
    // the backend goes before the window, so the OpenGL surface is dropped
    // while the window it draws on still exists
    backend: Option<Backend>,
    window: Option<Window>,
    next_frame: Instant,
    last_title_update: Instant,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let size = LogicalSize::new(SCREEN.0 * self.scale, SCREEN.1 * self.scale);
        let attributes =
            Window::default_attributes().with_title(format!("Kimika - {}", self.name)).with_inner_size(size);
        match Backend::create(event_loop, attributes) {
            Ok((window, backend)) => {
                log::info!("presenting with the {} backend", backend.name());
                self.window = Some(window);
                self.backend = Some(backend);
                self.next_frame = Instant::now();
            }
            Err(error) => {
                eprintln!("kimika: could not open a window: {error}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            // keys let go while another window has the keyboard never come
            WindowEvent::Focused(false) => self.keyboard.release(),
            WindowEvent::Resized(size) => {
                if let Some(backend) = &mut self.backend {
                    backend.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput { event, .. } if !event.repeat => {
                self.keyboard.key(event.physical_key, event.state == ElementState::Pressed);
            }
            WindowEvent::RedrawRequested => self.step(event_loop),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

impl App {
    /// a vertical blank when one is due, the screen shown, and a sleep
    /// until the next one.
    fn step(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(outcome) = self.system.outcome() {
            log::info!("{} ended: {outcome:?}", self.name);
            event_loop.exit();
            return;
        }
        let now = Instant::now();
        if now > self.next_frame + CATCH_UP_LIMIT {
            self.next_frame = now;
        }
        if now >= self.next_frame {
            self.next_frame += FRAME_TIME;
            self.system.set_input(self.keyboard.state());
            self.system.vblank();
        }
        let screen = self.system.screen();
        let image = match &screen {
            Some(screen) => ScreenImage { width: screen.width, height: screen.height, pixels: &screen.pixels },
            None => ScreenImage { width: 0, height: 0, pixels: &[] },
        };
        if let Some(backend) = &mut self.backend {
            if let Err(error) = backend.present(image, &Overlay::default()) {
                eprintln!("kimika: could not show the screen, {error}");
                event_loop.exit();
                return;
            }
        }
        if self.last_title_update.elapsed() >= Duration::from_millis(500) {
            self.last_title_update = Instant::now();
            if let Some(window) = &self.window {
                window.set_title(&format!("Kimika - {} - {}", self.name, self.system.status_line()));
            }
        }
        if let Some(wait) = self.next_frame.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
}
