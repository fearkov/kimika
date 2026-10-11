//! SceIofilemgr and the parts of SceLibKernel built on it: files and
//! folders, on the devices [crate::vfs] lays out, and the terminals, whose
//! output goes to the host's standard output, which is where a homebrew's
//! printf ends up.
//!
//! each function comes twice, as SceLibKernel exports it and as
//! SceIofilemgr's underscored system call, which takes some of its
//! arguments in a structure.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{Call, Function};
use crate::kernel::errors;
use crate::memory::Memory;
use crate::vfs::{io_error, Location};

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceIoOpen", open),
    ("_sceIoOpen", open),
    ("sceIoClose", close),
    ("sceIoRead", read),
    ("sceIoWrite", write),
    ("sceIoPread", pread),
    ("_sceIoPread", pread_with_option),
    ("sceIoPwrite", pwrite),
    ("_sceIoPwrite", pwrite_with_option),
    ("sceIoLseek", seek),
    ("_sceIoLseek", seek_with_option),
    ("sceIoLseek32", seek32),
    ("sceIoGetstat", get_stat),
    ("_sceIoGetstat", get_stat),
    ("sceIoGetstatByFd", get_stat_by_fd),
    ("_sceIoGetstatByFd", get_stat_by_fd),
    ("sceIoChstat", change_stat),
    ("_sceIoChstat", change_stat),
    ("sceIoChstatByFd", change_stat),
    ("_sceIoChstatByFd", change_stat),
    ("sceIoMkdir", make_directory),
    ("_sceIoMkdir", make_directory),
    ("sceIoRmdir", remove_directory),
    ("_sceIoRmdir", remove_directory),
    ("sceIoRemove", remove),
    ("_sceIoRemove", remove),
    ("sceIoRename", rename),
    ("_sceIoRename", rename),
    ("sceIoDopen", open_directory),
    ("_sceIoDopen", open_directory),
    ("sceIoDread", read_directory),
    ("_sceIoDread", read_directory),
    ("sceIoDclose", close),
    ("sceIoSync", sync),
    ("_sceIoSync", sync),
    ("sceIoSyncByFd", sync),
    ("sceIoDevctl", device_control),
];

/// SCE_O_WRONLY, open for writing; SCE_O_RDWR is it with SCE_O_RDONLY.
const WRITE: u32 = 0x2;
const APPEND: u32 = 0x100;
const CREATE: u32 = 0x200;
const TRUNCATE: u32 = 0x400;
const EXCLUSIVE: u32 = 0x800;

/// SCE_S_IFDIR and SCE_S_IFREG, and the permissions everything has, read
/// and write for the user and the system.
const DIRECTORY_MODE: u32 = 0o010000;
const FILE_MODE: u32 = 0o020000;
const PERMISSIONS: u32 = 0o000666;
/// the same in st_attr: SCE_SO_IFDIR and SCE_SO_IFREG, read and write.
const DIRECTORY_ATTRIBUTE: u32 = 0x10;
const FILE_ATTRIBUTE: u32 = 0x20;
const ATTRIBUTE_PERMISSIONS: u32 = 0x6;

/// how big a SceIoStat is, and where a SceIoDirent has its name.
const STAT_SIZE: u32 = 0x58;
const NAME_SIZE: usize = 256;

/// what a file descriptor names.
pub enum File {
    Terminal,
    Host {
        file: std::fs::File,
        path: String,
    },
    /// a file of a game in a .vpk, read whole.
    Memory {
        bytes: Arc<Vec<u8>>,
        position: u64,
        path: String,
    },
    Directory {
        entries: Vec<Entry>,
        next: usize,
    },
}

/// what a folder holds, as sceIoDread gives it.
pub struct Entry {
    pub name: String,
    pub stat: Stat,
}

/// what SceIoStat says of a file or a folder.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stat {
    pub directory: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
}

impl Stat {
    fn of(metadata: &std::fs::Metadata) -> Stat {
        Stat {
            directory: metadata.is_dir(),
            size: if metadata.is_dir() { 0 } else { metadata.len() },
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            accessed: metadata.accessed().ok(),
        }
    }

    fn file(size: u64) -> Stat {
        Stat { size, ..Stat::default() }
    }

    fn directory() -> Stat {
        Stat { directory: true, ..Stat::default() }
    }

    fn write(&self, memory: &Memory, address: u32) {
        memory.fill(address, 0, STAT_SIZE);
        let (mode, attributes) =
            if self.directory { (DIRECTORY_MODE, DIRECTORY_ATTRIBUTE) } else { (FILE_MODE, FILE_ATTRIBUTE) };
        memory.write32(address, mode | PERMISSIONS);
        memory.write32(address + 4, attributes | ATTRIBUTE_PERMISSIONS);
        memory.write32(address + 8, self.size as u32);
        memory.write32(address + 12, (self.size >> 32) as u32);
        for (offset, time) in [(0x10, self.created), (0x20, self.accessed), (0x30, self.modified)] {
            if let Some(time) = time.or(self.modified) {
                write_date_time(memory, address + offset, time);
            }
        }
    }
}

/// a SceDateTime, in UTC.
fn write_date_time(memory: &Memory, address: u32, time: SystemTime) {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    super::time::write_date_time(memory, address, since.as_micros() as i64);
}

#[derive(Default)]
pub struct Files {
    pub open: HashMap<u32, Arc<Mutex<File>>>,
}

impl Files {
    fn get(&self, fd: u32) -> Option<Arc<Mutex<File>>> {
        self.open.get(&fd).cloned()
    }
}

/// the file a descriptor names, none if it names none.
fn file(call: &Call, fd: u32) -> Option<Arc<Mutex<File>>> {
    call.shared.hle.files.lock().unwrap().get(fd)
}

fn keep(call: &Call, file: File) -> u32 {
    let fd = call.shared.kernel.uid();
    call.shared.hle.files.lock().unwrap().open.insert(fd, Arc::new(Mutex::new(file)));
    fd
}

/// sceIoOpen(path, flags, mode) -> file descriptor
fn open(call: &mut Call) {
    let (path, flags) = (call.string(), call.u32());
    let Some(path) = path else {
        return call.ret(errors::IO_INVAL);
    };
    let result = open_path(call, &path, flags);
    match result {
        Ok(file) => {
            let fd = keep(call, file);
            log::debug!("{}: {path} open as 0x{fd:08X}, flags 0x{flags:X}", call.thread.name);
            call.ret(fd);
        }
        Err(error) => {
            log::debug!("{}: {path} does not open, flags 0x{flags:X}: 0x{error:08X}", call.thread.name);
            call.ret(error);
        }
    }
}

fn open_path(call: &Call, path: &str, flags: u32) -> Result<File, u32> {
    let writes = flags & (WRITE | APPEND | CREATE | TRUNCATE) != 0;
    match call.shared.vfs.resolve(path).ok_or(errors::IO_NOENT)? {
        Location::Terminal => Ok(File::Terminal),
        Location::Package(name) => {
            if writes {
                return Err(errors::IO_ROFS);
            }
            let bytes = call.shared.vfs.read(&format!("app0:{name}"))?;
            Ok(File::Memory { bytes: Arc::new(bytes), position: 0, path: path.to_owned() })
        }
        Location::Host { path: host, read_only } => {
            if writes && read_only {
                return Err(errors::IO_ROFS);
            }
            if host.is_dir() {
                return Err(errors::IO_ISDIR);
            }
            let mut options = std::fs::OpenOptions::new();
            options
                .read(flags & WRITE == 0 || flags & 1 != 0)
                .write(flags & WRITE != 0)
                .append(flags & APPEND != 0)
                .truncate(flags & TRUNCATE != 0);
            if flags & CREATE != 0 {
                if flags & EXCLUSIVE != 0 {
                    options.create_new(true);
                } else {
                    options.create(true);
                }
            }
            let file = options.open(&host).map_err(|error| io_error(&error))?;
            Ok(File::Host { file, path: path.to_owned() })
        }
    }
}

/// sceIoClose(fd), and sceIoDclose(fd).
fn close(call: &mut Call) {
    let fd = call.u32();
    let closed = call.shared.hle.files.lock().unwrap().open.remove(&fd);
    call.ret(if closed.is_some() { 0 } else { errors::IO_BADF });
}

/// reads into a buffer of the game's from a file, at offset if one is
/// given and where the file is otherwise, giving how many bytes it read.
fn read_into(call: &Call, fd: u32, buffer: u32, size: u32, offset: Option<u64>) -> Result<u32, u32> {
    let file = file(call, fd).ok_or(errors::IO_BADF)?;
    let mut file = file.lock().unwrap();
    let mut data = vec![0; size as usize];
    let count = match &mut *file {
        File::Terminal => 0,
        File::Directory { .. } => return Err(errors::IO_BADF),
        File::Host { file, .. } => {
            let restore = match offset {
                Some(offset) => {
                    let at = file.stream_position().map_err(|error| io_error(&error))?;
                    file.seek(SeekFrom::Start(offset)).map_err(|error| io_error(&error))?;
                    Some(at)
                }
                None => None,
            };
            let count = read_fully(file, &mut data).map_err(|error| io_error(&error))?;
            if let Some(at) = restore {
                file.seek(SeekFrom::Start(at)).map_err(|error| io_error(&error))?;
            }
            count
        }
        File::Memory { bytes, position, .. } => {
            let start = offset.unwrap_or(*position).min(bytes.len() as u64) as usize;
            let count = data.len().min(bytes.len() - start);
            data[..count].copy_from_slice(&bytes[start..start + count]);
            if offset.is_none() {
                *position = (start + count) as u64;
            }
            count
        }
    };
    if !call.memory().write_bytes(buffer, &data[..count]) {
        return Err(errors::IO_FAULT);
    }
    Ok(count as u32)
}

/// reads until the buffer is full or the file ends.
fn read_fully(file: &mut std::fs::File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buffer.len() {
        match file.read(&mut buffer[total..]) {
            Ok(0) => break,
            Ok(count) => total += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(total)
}

/// writes a buffer of the game's to a file, at offset if one is given.
fn write_from(call: &Call, fd: u32, buffer: u32, size: u32, offset: Option<u64>) -> Result<u32, u32> {
    let file = file(call, fd).ok_or(errors::IO_BADF)?;
    let mut file = file.lock().unwrap();
    let data = call.memory().bytes(buffer, size as usize).ok_or(errors::IO_FAULT)?;
    match &mut *file {
        File::Terminal => {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&data);
            let _ = out.flush();
        }
        File::Host { file, .. } => {
            let restore = match offset {
                Some(offset) => {
                    let at = file.stream_position().map_err(|error| io_error(&error))?;
                    file.seek(SeekFrom::Start(offset)).map_err(|error| io_error(&error))?;
                    Some(at)
                }
                None => None,
            };
            file.write_all(&data).map_err(|error| io_error(&error))?;
            if let Some(at) = restore {
                file.seek(SeekFrom::Start(at)).map_err(|error| io_error(&error))?;
            }
        }
        File::Memory { .. } | File::Directory { .. } => return Err(errors::IO_BADF),
    }
    Ok(size)
}

/// sceIoRead(fd, buffer, size) -> bytes read
fn read(call: &mut Call) {
    let (fd, buffer, size) = (call.u32(), call.u32(), call.u32());
    let result = read_into(call, fd, buffer, size, None);
    call.result(result);
}

/// sceIoWrite(fd, buffer, size) -> bytes written
fn write(call: &mut Call) {
    let (fd, buffer, size) = (call.u32(), call.u32(), call.u32());
    let result = write_from(call, fd, buffer, size, None);
    call.result(result);
}

/// sceIoPread(fd, buffer, size, offset) -> bytes read, the file's own
/// position left as it was.
fn pread(call: &mut Call) {
    let (fd, buffer, size, offset) = (call.u32(), call.u32(), call.u32(), call.u64());
    let result = read_into(call, fd, buffer, size, Some(offset));
    call.result(result);
}

/// _sceIoPread(fd, buffer, size, option), the offset first in option.
fn pread_with_option(call: &mut Call) {
    let (fd, buffer, size, option) = (call.u32(), call.u32(), call.u32(), call.u32());
    let offset = read_u64(call.memory(), option);
    let result = read_into(call, fd, buffer, size, Some(offset));
    call.result(result);
}

/// sceIoPwrite(fd, buffer, size, offset) -> bytes written
fn pwrite(call: &mut Call) {
    let (fd, buffer, size, offset) = (call.u32(), call.u32(), call.u32(), call.u64());
    let result = write_from(call, fd, buffer, size, Some(offset));
    call.result(result);
}

/// _sceIoPwrite(fd, buffer, size, option), the offset first in option.
fn pwrite_with_option(call: &mut Call) {
    let (fd, buffer, size, option) = (call.u32(), call.u32(), call.u32(), call.u32());
    let offset = read_u64(call.memory(), option);
    let result = write_from(call, fd, buffer, size, Some(offset));
    call.result(result);
}

fn read_u64(memory: &Memory, address: u32) -> u64 {
    memory.read32(address) as u64 | (memory.read32(address + 4) as u64) << 32
}

/// moves a file's position, giving where it ends up.
fn seek_to(call: &Call, fd: u32, offset: i64, whence: u32) -> Result<u64, u32> {
    let file = file(call, fd).ok_or(errors::IO_BADF)?;
    let mut file = file.lock().unwrap();
    match &mut *file {
        File::Host { file, .. } => {
            let to = match whence {
                0 => SeekFrom::Start(u64::try_from(offset).map_err(|_| errors::IO_INVAL)?),
                1 => SeekFrom::Current(offset),
                2 => SeekFrom::End(offset),
                _ => return Err(errors::IO_INVAL),
            };
            file.seek(to).map_err(|error| io_error(&error))
        }
        File::Memory { bytes, position, .. } => {
            let from = match whence {
                0 => 0,
                1 => *position as i64,
                2 => bytes.len() as i64,
                _ => return Err(errors::IO_INVAL),
            };
            let to = from.checked_add(offset).filter(|&to| to >= 0).ok_or(errors::IO_INVAL)?;
            *position = to as u64;
            Ok(*position)
        }
        File::Terminal | File::Directory { .. } => Err(errors::IO_INVAL),
    }
}

/// sceIoLseek(fd, offset, whence) -> the new position, 64 bits.
fn seek(call: &mut Call) {
    let (fd, offset, whence) = (call.u32(), call.u64() as i64, call.u32());
    match seek_to(call, fd, offset, whence) {
        Ok(position) => call.ret(position),
        Err(error) => call.ret(error as i32 as i64 as u64),
    }
}

/// _sceIoLseek(fd, option), the offset and whence in option.
fn seek_with_option(call: &mut Call) {
    let (fd, option) = (call.u32(), call.u32());
    let (offset, whence) = (read_u64(call.memory(), option) as i64, call.memory().read32(option + 8));
    match seek_to(call, fd, offset, whence) {
        Ok(position) => call.ret(position),
        Err(error) => call.ret(error as i32 as i64 as u64),
    }
}

/// sceIoLseek32(fd, offset, whence) -> the new position.
fn seek32(call: &mut Call) {
    let (fd, offset, whence) = (call.u32(), call.i32(), call.u32());
    let result = seek_to(call, fd, offset as i64, whence).map(|position| position as u32);
    call.result(result);
}

/// what a path leads to: a file or a folder, and how big.
fn stat_path(call: &Call, path: &str) -> Result<Stat, u32> {
    match call.shared.vfs.resolve(path).ok_or(errors::IO_NOENT)? {
        Location::Terminal => Ok(Stat::file(0)),
        Location::Package(name) => {
            let game = &call.shared.vfs.app().ok_or(errors::IO_NOENT)?.game;
            if let Some(file) = game.files.iter().find(|file| file.eq_ignore_ascii_case(&name)) {
                let size = game.read(file).map_or(0, |bytes| bytes.len() as u64);
                return Ok(Stat::file(size));
            }
            if package_entries(game, &name).is_some() {
                return Ok(Stat::directory());
            }
            Err(errors::IO_NOENT)
        }
        Location::Host { path, .. } => {
            std::fs::metadata(path).map(|metadata| Stat::of(&metadata)).map_err(|error| io_error(&error))
        }
    }
}

/// sceIoGetstat(path, stat)
fn get_stat(call: &mut Call) {
    let (path, stat) = (call.string(), call.u32());
    let Some(path) = path else {
        return call.ret(errors::IO_INVAL);
    };
    match stat_path(call, &path) {
        Ok(found) => {
            found.write(call.memory(), stat);
            call.ret(0u32);
        }
        Err(error) => call.ret(error),
    }
}

/// sceIoGetstatByFd(fd, stat)
fn get_stat_by_fd(call: &mut Call) {
    let (fd, stat) = (call.u32(), call.u32());
    let Some(file) = file(call, fd) else {
        return call.ret(errors::IO_BADF);
    };
    let found = match &*file.lock().unwrap() {
        File::Terminal => Ok(Stat::file(0)),
        File::Host { file, .. } => {
            file.metadata().map(|metadata| Stat::of(&metadata)).map_err(|error| io_error(&error))
        }
        File::Memory { bytes, .. } => Ok(Stat::file(bytes.len() as u64)),
        File::Directory { .. } => Ok(Stat::directory()),
    };
    match found {
        Ok(found) => {
            found.write(call.memory(), stat);
            call.ret(0u32);
        }
        Err(error) => call.ret(error),
    }
}

/// sceIoChstat(path, stat, bits) and its kin, which change nothing here:
/// the host keeps its own modes and times.
fn change_stat(call: &mut Call) {
    call.ret(0u32);
}

/// the host path a path leads to, for changing something there, refused
/// for app0: and the terminals.
fn writable(call: &Call, path: Option<String>) -> Result<std::path::PathBuf, u32> {
    match call.shared.vfs.resolve(&path.ok_or(errors::IO_INVAL)?).ok_or(errors::IO_NOENT)? {
        Location::Host { path, read_only: false } => Ok(path),
        _ => Err(errors::IO_ROFS),
    }
}

fn done(result: Result<(), std::io::Error>) -> u32 {
    match result {
        Ok(()) => 0,
        Err(error) => io_error(&error),
    }
}

/// sceIoMkdir(path, mode)
fn make_directory(call: &mut Call) {
    let path = call.string();
    let result = writable(call, path).map(|path| done(std::fs::create_dir(path)));
    call.ret(result.unwrap_or_else(|error| error));
}

/// sceIoRmdir(path)
fn remove_directory(call: &mut Call) {
    let path = call.string();
    let result = writable(call, path).map(|path| done(std::fs::remove_dir(path)));
    call.ret(result.unwrap_or_else(|error| error));
}

/// sceIoRemove(path)
fn remove(call: &mut Call) {
    let path = call.string();
    let result = writable(call, path).map(|path| done(std::fs::remove_file(path)));
    call.ret(result.unwrap_or_else(|error| error));
}

/// sceIoRename(from, to)
fn rename(call: &mut Call) {
    let (from, to) = (call.string(), call.string());
    let result = writable(call, from)
        .and_then(|from| Ok((from, writable(call, to)?)))
        .map(|(from, to)| done(std::fs::rename(from, to)));
    call.ret(result.unwrap_or_else(|error| error));
}

/// the files and folders right under a folder of a game in a .vpk, none if
/// it is not one.
fn package_entries(game: &vita_module::game::Game, folder: &str) -> Option<Vec<Entry>> {
    let prefix = if folder.is_empty() { String::new() } else { format!("{folder}/") };
    let mut entries: Vec<Entry> = Vec::new();
    let mut found = folder.is_empty();
    for file in &game.files {
        let Some(rest) =
            file.get(..prefix.len()).filter(|start| start.eq_ignore_ascii_case(&prefix)).map(|_| &file[prefix.len()..])
        else {
            continue;
        };
        found = true;
        let (name, stat) = match rest.split_once('/') {
            Some((name, _)) => (name, Stat::directory()),
            None => (rest, Stat::file(game.read(file).map_or(0, |bytes| bytes.len() as u64))),
        };
        if !entries.iter().any(|entry| entry.name == name) {
            entries.push(Entry { name: name.to_owned(), stat });
        }
    }
    found.then_some(entries)
}

/// sceIoDopen(path) -> a descriptor for reading the folder.
fn open_directory(call: &mut Call) {
    let Some(path) = call.string() else {
        return call.ret(errors::IO_INVAL);
    };
    let entries = match call.shared.vfs.resolve(&path) {
        None | Some(Location::Terminal) => Err(errors::IO_NOENT),
        Some(Location::Package(name)) => {
            call.shared.vfs.app().and_then(|app| package_entries(&app.game, &name)).ok_or(errors::IO_NOENT)
        }
        Some(Location::Host { path, .. }) => host_entries(&path).map_err(|error| io_error(&error)),
    };
    match entries {
        Ok(entries) => {
            let fd = keep(call, File::Directory { entries, next: 0 });
            call.ret(fd);
        }
        Err(error) => call.ret(error),
    }
}

fn host_entries(path: &Path) -> std::io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let Ok(metadata) = entry.metadata() else { continue };
        entries.push(Entry { name: entry.file_name().to_string_lossy().into_owned(), stat: Stat::of(&metadata) });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// sceIoDread(fd, entry) -> 1 with the next entry, 0 past the last.
fn read_directory(call: &mut Call) {
    let (fd, entry) = (call.u32(), call.u32());
    let Some(file) = file(call, fd) else {
        return call.ret(errors::IO_BADF);
    };
    let mut file = file.lock().unwrap();
    let File::Directory { entries, next } = &mut *file else {
        return call.ret(errors::IO_BADF);
    };
    let Some(found) = entries.get(*next) else {
        return call.ret(0u32);
    };
    *next += 1;
    let memory = call.memory();
    found.stat.write(memory, entry);
    let mut name = found.name.as_bytes().to_vec();
    name.truncate(NAME_SIZE - 1);
    name.resize(NAME_SIZE, 0);
    memory.write_bytes(entry + STAT_SIZE, &name);
    call.ret(1u32);
}

/// sceIoSync(device, flags) and sceIoSyncByFd(fd), the host writing when
/// it will.
fn sync(call: &mut Call) {
    call.ret(0u32);
}

/// sceIoDevctl(device, command, input, input size, output, output size).
/// 0x3001 asks how big a device is and how much of it is free.
fn device_control(call: &mut Call) {
    let (device, command, _input, _input_size) = (call.string(), call.u32(), call.u32(), call.u32());
    let (output, output_size) = (call.u32(), call.u32());
    match command {
        0x3001 if output != 0 && output_size >= 0x10 => {
            let memory = call.memory();
            // 16 GiB with 8 free, in clusters of 32 KiB
            let (total, free): (u64, u64) = (16 << 30, 8 << 30);
            memory.write32(output, total as u32);
            memory.write32(output + 4, (total >> 32) as u32);
            memory.write32(output + 8, free as u32);
            memory.write32(output + 12, (free >> 32) as u32);
            if output_size >= 0x14 {
                memory.write32(output + 16, 0x8000);
            }
            call.ret(0u32);
        }
        _ => {
            log::warn!(
                "{}: sceIoDevctl 0x{command:X} on {} is not done yet",
                call.thread.name,
                device.as_deref().unwrap_or("?")
            );
            call.ret(0u32);
        }
    }
}
