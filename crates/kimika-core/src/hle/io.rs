//! SceIofilemgr: files. so far the terminals, tty0: and the rest, whose
//! output goes to the host's standard output, which is where a homebrew's
//! printf ends up.

use std::collections::HashMap;
use std::io::Write;

use super::{Call, Function};
use crate::kernel::errors;

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceIoOpen", open),
    ("sceIoClose", close),
    ("sceIoRead", read),
    ("sceIoWrite", write),
    ("sceIoLseek", seek),
    ("sceIoLseek32", seek),
];

/// SCE_O_WRONLY, open for writing.
const WRITE: u32 = 0x2;

/// what a file descriptor names.
pub enum File {
    Terminal,
}

#[derive(Default)]
pub struct Files {
    pub open: HashMap<u32, File>,
}

/// sceIoOpen(path, flags, mode) -> file descriptor
fn open(call: &mut Call) {
    let (path, flags) = (call.string(), call.u32());
    let Some(path) = path else {
        return call.ret(errors::IO_INVAL);
    };
    let terminal = path
        .strip_prefix("tty")
        .and_then(|rest| rest.strip_suffix(':'))
        .is_some_and(|number| number.parse::<u32>().is_ok());
    if !terminal {
        log::warn!("{}: no file {path} yet, flags 0x{flags:X}", call.thread.name);
        return call.ret(errors::IO_NOENT);
    }
    let fd = call.shared.kernel.uid();
    call.shared.hle.files.lock().unwrap().open.insert(fd, File::Terminal);
    log::debug!("{}: {path} open as 0x{fd:08X}{}", call.thread.name, if flags & WRITE != 0 { " to write" } else { "" });
    call.ret(fd);
}

/// sceIoClose(fd)
fn close(call: &mut Call) {
    let fd = call.u32();
    let closed = call.shared.hle.files.lock().unwrap().open.remove(&fd);
    call.ret(if closed.is_some() { 0 } else { errors::IO_BADF });
}

/// sceIoRead(fd, buffer, size) -> bytes read, none from a terminal.
fn read(call: &mut Call) {
    let fd = call.u32();
    let known = call.shared.hle.files.lock().unwrap().open.contains_key(&fd);
    call.ret(if known { 0 } else { errors::IO_BADF });
}

/// sceIoWrite(fd, buffer, size) -> bytes written
fn write(call: &mut Call) {
    let (fd, buffer, size) = (call.u32(), call.u32(), call.u32());
    if !call.shared.hle.files.lock().unwrap().open.contains_key(&fd) {
        return call.ret(errors::IO_BADF);
    }
    let bytes = call.memory().bytes(buffer, size as usize).unwrap_or_default();
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&bytes);
    let _ = out.flush();
    call.ret(size);
}

/// sceIoLseek(fd, offset, whence), which a terminal cannot do.
fn seek(call: &mut Call) {
    let fd = call.u32();
    let known = call.shared.hle.files.lock().unwrap().open.contains_key(&fd);
    call.ret(if known { errors::IO_INVAL } else { errors::IO_BADF });
}
