//! cargo run --release -p kimika-core --example boot -- <game> [frames] [dump frame] [dump.ppm]
//!
//! runs a game with no window for so many vertical blanks at the console's
//! rate, writes the screen to a PPM file at the frame asked for, and says
//! how it went: how it ended, the system functions it called that are not
//! done yet, and the memory it touched that was not there.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use kimika_core::{Config, Screen};

/// one vertical blank of the console's display, 59.94 a second.
const FRAME_TIME: Duration = Duration::from_nanos(1_001_000_000 / 60);

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(game) = args.first() else {
        eprintln!("usage, boot <game folder, .vpk or eboot.bin> [frames] [dump frame] [dump.ppm]");
        std::process::exit(2);
    };
    let frames: u64 = args.get(1).and_then(|frames| frames.parse().ok()).unwrap_or(600);
    let dump_at: Option<u64> = args.get(2).and_then(|frame| frame.parse().ok());
    let dump_to = args.get(3).cloned().unwrap_or_else(|| "screen.ppm".to_owned());

    let system = match kimika_core::load(Path::new(game), Config::default()) {
        Ok(system) => system,
        Err(error) => {
            eprintln!("could not load {game}, {error}");
            std::process::exit(1);
        }
    };
    let start = Instant::now();
    let mut next = start;
    for frame in 1..=frames {
        next += FRAME_TIME;
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        system.vblank();
        if dump_at == Some(frame) {
            match system.screen() {
                Some(screen) => {
                    write_ppm(&screen, Path::new(&dump_to));
                    println!("frame {frame}: the screen is in {dump_to}");
                }
                None => println!("frame {frame}: the screen is off"),
            }
        }
        if system.outcome().is_some() {
            break;
        }
    }
    println!("{:?} after {:.2?}, {}", system.outcome(), start.elapsed(), system.status_line());
    for (library, nid, count) in system.unimplemented() {
        println!(
            "  not done: {}::{} (0x{library:08X} 0x{nid:08X}) {count} times",
            vita_module::nid::library(library).unwrap_or("?"),
            vita_module::nid::name(library, nid).unwrap_or("?")
        );
    }
    for (page, count) in system.faults() {
        println!("  unmapped: 0x{page:08X} {count} times");
    }
}

fn write_ppm(screen: &Screen, path: &Path) {
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).expect("create the PPM file"));
    write!(file, "P6\n{} {}\n255\n", screen.width, screen.height).unwrap();
    for pixel in screen.pixels.as_chunks::<4>().0 {
        file.write_all(&pixel[..3]).unwrap();
    }
}
