//! Énumération WASAPI des sessions audio actives (par flux), comme pycaw.

use super::devices::{collection_count, collection_item, enum_devices};
use super::ffi::{
    query_interface, vtable_slot, CLSCTX_ALL, DEVICE_STATE_ACTIVE, Guid,
    IID_IAUDIO_SESSION_CONTROL_2, IID_IAUDIO_SESSION_MANAGER_2, ComRef,
};
use super::Flow;

/// État de session WASAPI : `AudioSessionStateActive` = le processus joue
/// (ou enregistre) du son en ce moment.
const SESSION_STATE_ACTIVE: i32 = 1;

/// Sessions audio d'un flux : `(PID, session active)` — une entrée par
/// session (le même PID peut apparaître plusieurs fois, sur des
/// périphériques ou flux différents).
pub(super) fn list_sessions_noinit(flow: Flow) -> Result<Vec<(u32, bool)>, String> {
    // IMMDevice : 3 Activate.
    type ActivateFn = unsafe extern "system" fn(
        *mut std::ffi::c_void,
        *const Guid,
        u32,
        *mut std::ffi::c_void,
        *mut *mut std::ffi::c_void,
    ) -> i32;
    // IAudioSessionManager2 : 3..4 (IAudioSessionManager), 5 GetSessionEnumerator.
    type GetSessionEnumeratorFn =
        unsafe extern "system" fn(*mut std::ffi::c_void, *mut *mut std::ffi::c_void) -> i32;
    // IAudioSessionEnumerator : 3 GetCount(i32), 4 GetSession(i32).
    type GetCountI32Fn = unsafe extern "system" fn(*mut std::ffi::c_void, *mut i32) -> i32;
    type GetSessionFn = unsafe extern "system" fn(
        *mut std::ffi::c_void,
        i32,
        *mut *mut std::ffi::c_void,
    ) -> i32;
    // IAudioSessionControl (hérité par IAudioSessionControl2 — même vtable
    // au début) : 3 GetState, 14 GetProcessId.
    type GetStateFn = unsafe extern "system" fn(*mut std::ffi::c_void, *mut i32) -> i32;
    type GetProcessIdFn = unsafe extern "system" fn(*mut std::ffi::c_void, *mut u32) -> i32;

    let Some(devices) = enum_devices(flow, DEVICE_STATE_ACTIVE)? else {
        return Ok(Vec::new());
    };

    let mut entries = Vec::new();
    for index in 0..collection_count(&devices) {
        let Some(device) = collection_item(&devices, index) else {
            continue;
        };

        let mut manager_ptr = std::ptr::null_mut();
        // SAFETY : activation du gestionnaire de sessions du périphérique.
        let activate_fn: ActivateFn = unsafe { std::mem::transmute(vtable_slot(device.ptr(), 3)) };
        let hr = unsafe {
            activate_fn(
                device.ptr(),
                &IID_IAUDIO_SESSION_MANAGER_2,
                CLSCTX_ALL,
                std::ptr::null_mut(),
                &mut manager_ptr,
            )
        };
        if hr < 0 || manager_ptr.is_null() {
            continue;
        }
        let manager = ComRef::new(manager_ptr);

        let mut session_enum_ptr = std::ptr::null_mut();
        // SAFETY : énumérateur de sessions du périphérique.
        let session_enum_fn: GetSessionEnumeratorFn =
            unsafe { std::mem::transmute(vtable_slot(manager.ptr(), 5)) };
        let hr = unsafe { session_enum_fn(manager.ptr(), &mut session_enum_ptr) };
        if hr < 0 || session_enum_ptr.is_null() {
            continue;
        }
        let session_enum = ComRef::new(session_enum_ptr);

        let mut session_count: i32 = 0;
        // SAFETY : nombre de sessions.
        let get_count_i32: GetCountI32Fn =
            unsafe { std::mem::transmute(vtable_slot(session_enum.ptr(), 3)) };
        if unsafe { get_count_i32(session_enum.ptr(), &mut session_count) } < 0 {
            continue;
        }
        for session_index in 0..session_count {
            let mut control_ptr = std::ptr::null_mut();
            // SAFETY : accès à la session.
            let get_session_fn: GetSessionFn =
                unsafe { std::mem::transmute(vtable_slot(session_enum.ptr(), 4)) };
            let hr = unsafe { get_session_fn(session_enum.ptr(), session_index, &mut control_ptr) };
            if hr < 0 || control_ptr.is_null() {
                continue;
            }
            let control = ComRef::new(control_ptr);

            // QI vers IAudioSessionControl2 pour lire le PID du processus.
            let control2_ptr = match unsafe { query_interface(control.ptr(), &IID_IAUDIO_SESSION_CONTROL_2) } {
                Ok(ptr) => ptr,
                Err(_) => continue,
            };
            let control2 = ComRef::new(control2_ptr);

            let mut pid: u32 = 0;
            // SAFETY : lecture du PID de la session (emplacement 14).
            let get_pid_fn: GetProcessIdFn =
                unsafe { std::mem::transmute(vtable_slot(control2.ptr(), 14)) };
            if unsafe { get_pid_fn(control2.ptr(), &mut pid) } < 0 || pid == 0 {
                continue;
            }

            // SAFETY : état de la session (emplacement 3, hérité de
            // IAudioSessionControl) — « active » = joue du son.
            let mut state: i32 = 0;
            let get_state_fn: GetStateFn =
                unsafe { std::mem::transmute(vtable_slot(control2.ptr(), 3)) };
            let playing =
                unsafe { get_state_fn(control2.ptr(), &mut state) } >= 0 && state == SESSION_STATE_ACTIVE;
            entries.push((pid, playing));
        }
    }
    Ok(entries)
}

/// PIDs (uniques) des processus ayant une session audio active pour un flux.
pub(super) fn list_session_pids_noinit(flow: Flow) -> Result<Vec<u32>, String> {
    let mut pids = Vec::new();
    for (pid, _) in list_sessions_noinit(flow)? {
        if !pids.contains(&pid) {
            pids.push(pid);
        }
    }
    Ok(pids)
}
