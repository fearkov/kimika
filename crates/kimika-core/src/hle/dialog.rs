//! the system's dialogs and PSN: a message dialog closes at once as if
//! its first button were chosen, its text in the log; the others close at
//! once too. trophies unlock without a word, and the console is not signed
//! in to PSN.

use super::{Call, Function};

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceCommonDialogSetConfigParam", accepted),
    ("sceCommonDialogUpdate", accepted),
    ("sceMsgDialogInit", message_init),
    ("sceMsgDialogGetStatus", finished),
    ("sceMsgDialogGetResult", message_result),
    ("sceMsgDialogClose", accepted),
    ("sceMsgDialogTerm", accepted),
    ("sceMsgDialogProgressBarSetValue", accepted),
    ("sceMsgDialogProgressBarInc", accepted),
    ("sceMsgDialogProgressBarSetMsg", accepted),
    ("sceSaveDataDialogInit", accepted),
    ("sceSaveDataDialogContinue", accepted),
    ("sceSaveDataDialogGetStatus", finished),
    ("sceSaveDataDialogGetSubStatus", finished),
    ("sceSaveDataDialogGetResult", plain_result),
    ("sceSaveDataDialogClose", accepted),
    ("sceSaveDataDialogTerm", accepted),
    ("sceSaveDataDialogFinish", accepted),
    ("sceImeDialogInit", accepted),
    ("sceImeDialogGetStatus", finished),
    ("sceImeDialogGetResult", plain_result),
    ("sceImeDialogTerm", accepted),
    ("sceNetCheckDialogInit", accepted),
    ("sceNetCheckDialogGetStatus", finished),
    ("sceNetCheckDialogGetResult", canceled_result),
    ("sceNetCheckDialogTerm", accepted),
    ("sceNpTrophyInit", accepted),
    ("sceNpTrophyTerm", accepted),
    ("sceNpTrophyCreateContext", trophy_create),
    ("sceNpTrophyDestroyContext", accepted),
    ("sceNpTrophyCreateHandle", trophy_create),
    ("sceNpTrophyDestroyHandle", accepted),
    ("sceNpTrophyAbortHandle", accepted),
    ("sceNpTrophyRegisterContext", accepted),
    ("sceNpTrophyUnlockTrophy", trophy_unlock),
    ("sceNpTrophyGetTrophyUnlockState", trophy_unlock_state),
    ("sceNpTrophyGetGameInfo", accepted),
    ("sceNpTrophyGetTrophyInfo", accepted),
    ("sceNpTrophyGetGroupInfo", accepted),
    ("sceNpInit", accepted),
    ("sceNpTerm", accepted),
    ("sceNpCheckCallback", accepted),
    ("sceNpRegisterServiceStateCallback", accepted),
    ("sceNpUnregisterServiceStateCallback", accepted),
    ("sceNpGetServiceState", np_service_state),
    ("sceNpManagerGetNpId", np_get_id),
];

fn accepted(call: &mut Call) {
    call.ret(0u32);
}

/// SCE_COMMON_DIALOG_STATUS_FINISHED
fn finished(call: &mut Call) {
    call.ret(2u32);
}

/// sceMsgDialogInit(params): the message goes to the log.
fn message_init(call: &mut Call) {
    let params = call.u32();
    let memory = call.memory();
    let mode = memory.read32(params + 0x50);
    let user = memory.read32(params + 0x54);
    let error = memory.read32(params + 0x5C);
    let text = match mode {
        1 if user != 0 => memory.read_cstring(memory.read32(user + 4), 1024),
        3 if error != 0 => Some(format!("error 0x{:08X}", memory.read32(error))),
        _ => None,
    };
    log::info!("{}: a message dialog says {}", call.thread.name, text.as_deref().unwrap_or("something"));
    call.shared.hle.dialog_mode.store(mode, std::sync::atomic::Ordering::Relaxed);
    call.ret(0u32);
}

/// sceMsgDialogGetResult(&result): the first button, yes or OK.
fn message_result(call: &mut Call) {
    let result = call.u32();
    let mode = call.shared.hle.dialog_mode.load(std::sync::atomic::Ordering::Relaxed);
    let memory = call.memory();
    memory.fill(result, 0, 0x2C);
    memory.write32(result, mode);
    memory.write32(result + 8, 1);
    call.ret(0u32);
}

/// a result structure with result 0, the OK, and nothing else.
fn plain_result(call: &mut Call) {
    let result = call.u32();
    let memory = call.memory();
    memory.write32(result, 0);
    memory.write32(result + 4, 0);
    call.ret(0u32);
}

/// a result structure saying the user canceled.
fn canceled_result(call: &mut Call) {
    let result = call.u32();
    let memory = call.memory();
    memory.write32(result, 1);
    call.ret(0u32);
}

/// sceNpTrophyCreateContext(&context, ...) and sceNpTrophyCreateHandle.
fn trophy_create(call: &mut Call) {
    let out = call.u32();
    let id = call.shared.kernel.uid();
    if out != 0 {
        call.memory().write32(out, id);
    }
    call.ret(0u32);
}

/// sceNpTrophyUnlockTrophy(context, handle, trophy, &platinum)
fn trophy_unlock(call: &mut Call) {
    let (_context, _handle, trophy, platinum) = (call.u32(), call.u32(), call.i32(), call.u32());
    log::info!("{}: trophy {trophy} unlocked", call.thread.name);
    if platinum != 0 {
        call.memory().write32(platinum, u32::MAX);
    }
    call.ret(0u32);
}

/// sceNpTrophyGetTrophyUnlockState(context, handle, &flags, &count):
/// nothing unlocked.
fn trophy_unlock_state(call: &mut Call) {
    let (_context, _handle, flags, count) = (call.u32(), call.u32(), call.u32(), call.u32());
    let memory = call.memory();
    if flags != 0 {
        memory.fill(flags, 0, 16);
    }
    if count != 0 {
        memory.write32(count, 0);
    }
    call.ret(0u32);
}

/// sceNpGetServiceState(&state): signed out.
fn np_service_state(call: &mut Call) {
    let state = call.u32();
    if state != 0 {
        call.memory().write32(state, 1);
    }
    call.ret(0u32);
}

/// sceNpManagerGetNpId(&id): the user's name as the online ID.
fn np_get_id(call: &mut Call) {
    let id = call.u32();
    if id == 0 {
        return call.ret(0x8055_0003u32);
    }
    let memory = call.memory();
    memory.fill(id, 0, 0x24);
    memory.write_bytes(id, b"Kimika");
    call.ret(0u32);
}
