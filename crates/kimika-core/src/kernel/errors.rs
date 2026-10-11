//! the error codes the system answers with, negative as an i32, with the
//! names and values vita-headers gives them.

/// 0x80020001, any other failure.
pub const ERROR: u32 = 0x8002_0001;
/// 0x80020006, a pointer or an argument out of range.
pub const ILLEGAL_ADDR: u32 = 0x8002_0006;
/// 0x80020009, a size or a count out of range.
pub const INVALID_ARGUMENT_SIZE: u32 = 0x8002_0009;

/// 0x80024302, no room left for the block.
pub const NO_FREE_PHYSICAL_PAGE: u32 = 0x8002_4302;
/// 0x80024501, no object by that UID.
pub const INVALID_UID: u32 = 0x8002_4501;
/// 0x80024701, freeing a block that is not there.
pub const ILLEGAL_BLOCK_ID: u32 = 0x8002_4701;
/// 0x80024B01, a memory block type the system does not have.
pub const ILLEGAL_MEMBLOCK_TYPE: u32 = 0x8002_4B01;
/// 0x80024B05, a size that is not a multiple of what the type allocates in.
pub const ILLEGAL_MEMBLOCK_SIZE: u32 = 0x8002_4B05;

/// 0x80028001, no object of the kind asked for by that UID.
pub const UNKNOWN_UID: u32 = 0x8002_8001;
/// 0x80028005, a wait that ran out of time.
pub const WAIT_TIMEOUT: u32 = 0x8002_8005;
/// 0x80028006, a wait cut short when its object was deleted.
pub const WAIT_DELETE: u32 = 0x8002_8006;
/// 0x80028021, no thread by that UID.
pub const UNKNOWN_THREAD_ID: u32 = 0x8002_8021;
/// 0x80028023, a thread priority out of range.
pub const ILLEGAL_PRIORITY: u32 = 0x8002_8023;
/// 0x80028024, a stack too small.
pub const ILLEGAL_STACK_SIZE: u32 = 0x8002_8024;
/// 0x80028028, starting a thread that is not dormant.
pub const NOT_DORMANT: u32 = 0x8002_8028;

/// 0x80028141, no mutex by that UID.
pub const UNKNOWN_MUTEX_ID: u32 = 0x8002_8141;
/// 0x80028142, locking again a mutex that is not recursive.
pub const MUTEX_RECURSIVE: u32 = 0x8002_8142;
/// 0x80028143, a lock count that would wrap around.
pub const MUTEX_LOCK_OVF: u32 = 0x8002_8143;
/// 0x80028144, unlocking more than was locked.
pub const MUTEX_UNLOCK_UDF: u32 = 0x8002_8144;
/// 0x80028145, a try lock on a mutex another thread holds.
pub const MUTEX_FAILED_TO_OWN: u32 = 0x8002_8145;
/// 0x80028146, unlocking a mutex another thread holds.
pub const MUTEX_NOT_OWNED: u32 = 0x8002_8146;

/// 0x80028181, no lightweight mutex by that work area.
pub const UNKNOWN_LW_MUTEX_ID: u32 = 0x8002_8181;
/// 0x80028182, locking again a lightweight mutex that is not recursive.
pub const LW_MUTEX_RECURSIVE: u32 = 0x8002_8182;
/// 0x80028184, unlocking a lightweight mutex more than was locked.
pub const LW_MUTEX_UNLOCK_UDF: u32 = 0x8002_8184;
/// 0x80028185, a try lock on a lightweight mutex another thread holds.
pub const LW_MUTEX_FAILED_TO_OWN: u32 = 0x8002_8185;
/// 0x80028186, unlocking a lightweight mutex another thread holds.
pub const LW_MUTEX_NOT_OWNED: u32 = 0x8002_8186;

/// 0x80010002, no such file or directory, errno's ENOENT.
pub const IO_NOENT: u32 = 0x8001_0002;
/// 0x80010009, a file descriptor that is not open, EBADF.
pub const IO_BADF: u32 = 0x8001_0009;
/// 0x8001000E, a buffer that is not all there, EFAULT.
pub const IO_FAULT: u32 = 0x8001_000E;
/// 0x80010015, a folder where a file was meant, EISDIR.
pub const IO_ISDIR: u32 = 0x8001_0015;
/// 0x80010016, an argument out of range, EINVAL.
pub const IO_INVAL: u32 = 0x8001_0016;
/// 0x8001001E, writing where nothing can be written, EROFS.
pub const IO_ROFS: u32 = 0x8001_001E;

/// 0x80025001, a file that is not a module.
pub const ILLEGAL_ELF_HEADER: u32 = 0x8002_5001;
/// 0x8002D000, a module_start that said it failed.
pub const MODULEMGR_START_FAILED: u32 = 0x8002_D000;
/// 0x8002D001, a module_stop that said it would not stop.
pub const MODULEMGR_STOP_FAIL: u32 = 0x8002_D001;
/// 0x8002D008, no room for a module.
pub const MODULEMGR_NOMEM: u32 = 0x8002_D008;
/// 0x8002D011, no module by that UID.
pub const MODULEMGR_NO_MOD: u32 = 0x8002_D011;
/// 0x8002D014, starting a module already started.
pub const MODULEMGR_STARTED: u32 = 0x8002_D014;
/// 0x8002D015, stopping a module not started.
pub const MODULEMGR_NOT_STARTED: u32 = 0x8002_D015;
/// 0x8002D016, unloading a module not stopped.
pub const MODULEMGR_NOT_STOPPED: u32 = 0x8002_D016;
/// 0x8002D01C, no module file at that path.
pub const MODULEMGR_NOENT: u32 = 0x8002_D01C;

/// 0x80290001, a SceDisplayFrameBuf of the wrong size.
pub const DISPLAY_INVALID_VALUE: u32 = 0x8029_0001;
/// 0x80290002, a framebuffer with no base.
pub const DISPLAY_INVALID_ADDR: u32 = 0x8029_0002;
/// 0x80290003, a pixel format the display does not have.
pub const DISPLAY_INVALID_PIXELFORMAT: u32 = 0x8029_0003;
/// 0x80290004, a pitch narrower than the width.
pub const DISPLAY_INVALID_PITCH: u32 = 0x8029_0004;
/// 0x80290005, a resolution the display does not have.
pub const DISPLAY_INVALID_RESOLUTION: u32 = 0x8029_0005;
/// 0x80290006, a sync mode that is neither now nor the next frame.
pub const DISPLAY_INVALID_UPDATETIMING: u32 = 0x8029_0006;
