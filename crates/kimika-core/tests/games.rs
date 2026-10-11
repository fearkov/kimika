//! whole games made up with vita-module's builders, loaded and run to
//! their end.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use kimika_core::{load, Config, Outcome, System};
use vita_module::sfo::Value;
use vita_module::testing::{self, ModuleBuilder};

/// a game whose module_start returns at once, packed as a zip.
fn packed_game(deflate: bool) -> Vec<u8> {
    let eboot = ModuleBuilder { name: "packed".to_owned(), ..ModuleBuilder::default() }.build();
    let sfo =
        testing::sfo(&[("TITLE_ID", Value::Text("KIMI09999".to_owned())), ("TITLE", Value::Text("Packed".to_owned()))]);
    testing::vpk(&[("eboot.bin", &eboot), ("sce_sys/param.sfo", &sfo)], deflate)
}

/// a file of its own for one test, under cargo's scratch folder for tests.
fn scratch(name: &str) -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("games");
    std::fs::create_dir_all(&folder).unwrap();
    folder.join(name)
}

/// the outcome once the game ends, or none if it does not within a while.
fn run_to_end(system: &System) -> Option<Outcome> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(outcome) = system.outcome() {
            return Some(outcome);
        }
        system.vblank();
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

#[test]
fn a_zip_opens_as_a_vpk_does() {
    for (name, deflate) in [("game.zip", true), ("game.ZIP", false), ("game.vpk", true)] {
        let path = scratch(name);
        std::fs::write(&path, packed_game(deflate)).unwrap();
        let system = load(&path, Config::default()).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(run_to_end(&system), Some(Outcome::Exited(0)), "{name}");
    }
}

#[test]
fn a_zip_without_an_eboot_is_refused() {
    let path = scratch("empty.zip");
    std::fs::write(&path, testing::vpk(&[("readme.txt", b"nothing to run")], false)).unwrap();
    assert!(load(&path, Config::default()).is_err());
}
