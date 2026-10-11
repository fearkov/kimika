//! the console's storage as a game sees it, by device: app0:, the game's
//! own files, read only, from its folder or its .vpk; savedata0: and
//! addcont0:, the game's own parts of ux0:; ux0: and every other device in
//! a folder of its own under the data directory; and the terminals.
//!
//! the console's file systems ignore case, so a path finds a file on the
//! host whatever the case of its name there.

use std::path::{Path, PathBuf};

use vita_module::game::Game;

use crate::kernel::errors;

/// where a path the game gives leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// tty0: and the rest, the host's standard output.
    Terminal,
    /// a file or folder under app0: of a game in a .vpk, by its path there.
    Package(String),
    /// one on the host.
    Host { path: PathBuf, read_only: bool },
}

/// the game's files, as app0: shows them.
pub struct App {
    pub game: Game,
    /// the folder the game is in, none for a .vpk.
    pub folder: Option<PathBuf>,
}

pub struct Vfs {
    app: Option<App>,
    /// where ux0: and the other writable devices live, none to keep
    /// nothing.
    data: Option<PathBuf>,
    /// the game's title ID, which names its save data and its add-ons.
    title_id: String,
}

impl Vfs {
    pub fn new(app: Option<App>, data: Option<PathBuf>) -> Vfs {
        let title_id =
            app.as_ref().and_then(|app| app.game.title_id()).map_or_else(|| "KIMI00000".to_owned(), str::to_owned);
        Vfs { app, data, title_id }
    }

    /// makes the folders a console has before a game runs: ux0:data, and
    /// the game's save data, which savedata0: is.
    pub fn prepare(&self) {
        let Some(data) = &self.data else { return };
        for folder in [data.join("ux0/data"), data.join("ux0/user/00/savedata").join(&self.title_id)] {
            if let Err(error) = std::fs::create_dir_all(&folder) {
                log::warn!("could not make {}, {error}", folder.display());
            }
        }
    }

    pub fn app(&self) -> Option<&App> {
        self.app.as_ref()
    }

    pub fn title_id(&self) -> &str {
        &self.title_id
    }

    /// where a path leads, none for a device there is nothing behind.
    pub fn resolve(&self, path: &str) -> Option<Location> {
        let (device, parts) = split(path)?;
        if device.strip_prefix("tty").is_some_and(|number| number.parse::<u32>().is_ok()) {
            return Some(Location::Terminal);
        }
        if device == "app0" {
            let app = self.app.as_ref()?;
            return Some(match &app.folder {
                Some(folder) => Location::Host { path: on_host(folder, &parts), read_only: true },
                None => Location::Package(parts.join("/")),
            });
        }
        let data = self.data.as_ref()?;
        let (root, parts) = match device.as_str() {
            "savedata0" => (vec!["ux0", "user", "00", "savedata", &self.title_id], parts),
            "addcont0" => (vec!["ux0", "addcont", &self.title_id], parts),
            _ => (vec![device.as_str()], parts),
        };
        let root = root.iter().fold(data.clone(), |path, part| path.join(part));
        Some(Location::Host { path: on_host(&root, &parts), read_only: false })
    }

    /// the whole of a file, for the loader.
    pub fn read(&self, path: &str) -> Result<Vec<u8>, u32> {
        match self.resolve(path).ok_or(errors::IO_NOENT)? {
            Location::Terminal => Err(errors::IO_INVAL),
            Location::Package(name) => {
                let game = &self.app.as_ref().ok_or(errors::IO_NOENT)?.game;
                game.read(&name).map_err(|_| errors::IO_NOENT)
            }
            Location::Host { path, .. } => std::fs::read(path).map_err(|error| io_error(&error)),
        }
    }
}

/// a path's device, in lower case, and the names along it, with `.` and
/// `..` worked out. a path with no device is under app0:, where a game
/// starts out.
pub fn split(path: &str) -> Option<(String, Vec<String>)> {
    let (device, rest) = match path.split_once(':') {
        Some((device, rest)) => (device.to_ascii_lowercase(), rest),
        None => ("app0".to_owned(), path),
    };
    if device.is_empty() || device.contains(['/', '\\']) {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for part in rest.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part.to_owned()),
        }
    }
    Some((device, parts))
}

/// a path under root on the host, each name matched whatever its case to
/// one there, and kept as it is where nothing matches, for a file about to
/// be made.
fn on_host(root: &Path, parts: &[String]) -> PathBuf {
    let mut path = root.to_owned();
    for part in parts {
        let exact = path.join(part);
        if exact.exists() {
            path = exact;
            continue;
        }
        let found = std::fs::read_dir(&path).ok().and_then(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name())
                .find(|name| name.to_string_lossy().eq_ignore_ascii_case(part))
        });
        path = match found {
            Some(name) => path.join(name),
            None => exact,
        };
    }
    path
}

/// 0x80010000 with the errno a host error means.
pub fn io_error(error: &std::io::Error) -> u32 {
    use std::io::ErrorKind;
    let errno = match error.kind() {
        ErrorKind::NotFound => 2,
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => 13,
        ErrorKind::AlreadyExists => 17,
        ErrorKind::NotADirectory => 20,
        ErrorKind::IsADirectory => 21,
        ErrorKind::InvalidInput => 22,
        ErrorKind::StorageFull => 28,
        ErrorKind::DirectoryNotEmpty => 90,
        ErrorKind::InvalidFilename => 91,
        _ => 5,
    };
    0x8001_0000 | errno
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_split_into_a_device_and_names() {
        let parts = |names: &[&str]| names.iter().map(|name| name.to_string()).collect::<Vec<_>>();
        assert_eq!(split("app0:/sce_module/libc.suprx"), Some(("app0".into(), parts(&["sce_module", "libc.suprx"]))));
        assert_eq!(split("UX0:data//a/./b/../c"), Some(("ux0".into(), parts(&["data", "a", "c"]))));
        assert_eq!(split("ux0:/../../etc"), Some(("ux0".into(), parts(&["etc"]))));
        assert_eq!(split("eboot.bin"), Some(("app0".into(), parts(&["eboot.bin"]))));
        assert_eq!(split(":x"), None);
    }

    #[test]
    fn devices_lead_to_folders_under_the_data_directory() {
        let vfs = Vfs::new(None, Some(PathBuf::from("/nonexistent/data")));
        let host = |path: &str| Location::Host { path: PathBuf::from(path), read_only: false };
        assert_eq!(vfs.resolve("ux0:data/save.bin"), Some(host("/nonexistent/data/ux0/data/save.bin")));
        assert_eq!(
            vfs.resolve("savedata0:/slot1"),
            Some(host("/nonexistent/data/ux0/user/00/savedata/KIMI00000/slot1"))
        );
        assert_eq!(vfs.resolve("tty0:"), Some(Location::Terminal));
        // no game, no app0:
        assert_eq!(vfs.resolve("app0:eboot.bin"), None);
        assert_eq!(Vfs::new(None, None).resolve("ux0:x"), None);
    }
}
