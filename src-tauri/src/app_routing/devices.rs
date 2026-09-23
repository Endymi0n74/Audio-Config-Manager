//! Socle d'énumération WASAPI (IMMDeviceEnumerator → collection → Item/GetId)
//! et lecture des identifiants « emballés » / noms conviviaux des périphériques.

use super::ffi::{
    CLSCTX_ALL, DEVICE_STATE_ACTIVE, PKEY_DEVICE_FRIENDLY_NAME, STGM_READ, VT_LPWSTR,
    CLSID_MMDEVICE_ENUMERATOR, IID_IMMDEVICE_ENUMERATOR, PropertyKey, PropVariant,
    CoCreateInstance, CoTaskMemFree, ComRef, PropVariantClear, vtable_slot, with_apartment,
};
use super::Flow;

// ---------------------------------------------------------------------------
// Identifiants « emballés » par le registre audio Windows.
// ---------------------------------------------------------------------------

const MMDEVAPI_TOKEN: &str = r"\\?\SWD#MMDEVAPI#";
pub(super) const RENDER_INTERFACE: &str = "#{e6327cad-dcec-4949-ae8a-991e976a79d2}";
const CAPTURE_INTERFACE: &str = "#{2eef81be-33fa-4800-9670-1cd474972c3f}";

pub(super) fn pack_device_id(flow: Flow, device_id: &str) -> String {
    let suffix = match flow {
        Flow::Output => RENDER_INTERFACE,
        Flow::Input => CAPTURE_INTERFACE,
    };
    format!("{MMDEVAPI_TOKEN}{device_id}{suffix}")
}

pub(super) fn unpack_device_id(packed: &str) -> Option<String> {
    let rest = packed.strip_prefix(MMDEVAPI_TOKEN)?;
    let id = rest
        .strip_suffix(RENDER_INTERFACE)
        .or_else(|| rest.strip_suffix(CAPTURE_INTERFACE))?;
    Some(id.to_string())
}

// ---------------------------------------------------------------------------
// Énumération WASAPI brute (COM, sans appartement : à appeler depuis le
// thread d'appartement ou un contexte déjà initialisé).
// ---------------------------------------------------------------------------

// IMMDeviceEnumerator (vtable) : 0-2 IUnknown, 3 EnumAudioEndpoints.
type EnumAudioEndpointsFn = unsafe extern "system" fn(
    *mut std::ffi::c_void,
    i32,
    u32,
    *mut *mut std::ffi::c_void,
) -> i32;
// IMMDeviceCollection : 3 GetCount, 4 Item.
type GetCountU32Fn = unsafe extern "system" fn(*mut std::ffi::c_void, *mut u32) -> i32;
type ItemFn = unsafe extern "system" fn(
    *mut std::ffi::c_void,
    u32,
    *mut *mut std::ffi::c_void,
) -> i32;
// IMMDevice : 4 OpenPropertyStore, 5 GetId.
type GetIdFn = unsafe extern "system" fn(
    *mut std::ffi::c_void,
    *mut *mut u16,
) -> i32;

/// Énumérateur COM (MMDeviceEnumerator) — libération automatique.
fn device_enumerator() -> Result<ComRef, String> {
    let mut enumerator_ptr = std::ptr::null_mut();
    // SAFETY : création du CLSID standard (MMDeviceEnumerator).
    let hr = unsafe {
        CoCreateInstance(
            &CLSID_MMDEVICE_ENUMERATOR,
            std::ptr::null_mut(),
            CLSCTX_ALL,
            &IID_IMMDEVICE_ENUMERATOR,
            &mut enumerator_ptr,
        )
    };
    if hr < 0 || enumerator_ptr.is_null() {
        return Err(format!("Énumérateur audio indisponible (0x{:08X})", hr as u32));
    }
    Ok(ComRef::new(enumerator_ptr))
}

/// Collection des périphériques d'un flux selon le masque d'état
/// (`EnumAudioEndpoints`, emplacement 3). `Ok(None)` = énumération vide
/// (périphérique débranché entre-temps, par ex.).
pub(super) fn enum_devices(flow: Flow, state_mask: u32) -> Result<Option<ComRef>, String> {
    let enumerator = device_enumerator()?;
    let mut devices_ptr = std::ptr::null_mut();
    // SAFETY : énumération des périphériques du flux selon le masque d'état.
    let enum_fn: EnumAudioEndpointsFn =
        unsafe { std::mem::transmute(vtable_slot(enumerator.ptr(), 3)) };
    let hr = unsafe { enum_fn(enumerator.ptr(), flow.value(), state_mask, &mut devices_ptr) };
    if hr < 0 || devices_ptr.is_null() {
        return Ok(None);
    }
    Ok(Some(ComRef::new(devices_ptr)))
}

/// Nombre de périphériques d'une collection (`GetCount`, emplacement 3 ;
/// 0 en cas d'échec).
pub(super) fn collection_count(devices: &ComRef) -> u32 {
    let mut count: u32 = 0;
    // SAFETY : nombre de périphériques.
    let get_count: GetCountU32Fn = unsafe { std::mem::transmute(vtable_slot(devices.ptr(), 3)) };
    if unsafe { get_count(devices.ptr(), &mut count) } < 0 {
        return 0;
    }
    count
}

/// Périphérique n° `index` d'une collection (`Item`, emplacement 4).
pub(super) fn collection_item(devices: &ComRef, index: u32) -> Option<ComRef> {
    let mut device_ptr = std::ptr::null_mut();
    // SAFETY : accès au périphérique de la collection.
    let item_fn: ItemFn = unsafe { std::mem::transmute(vtable_slot(devices.ptr(), 4)) };
    if unsafe { item_fn(devices.ptr(), index, &mut device_ptr) } < 0 || device_ptr.is_null() {
        return None;
    }
    Some(ComRef::new(device_ptr))
}

/// Identifiant (non emballé) d'un périphérique (`GetId`, emplacement 5).
pub(super) fn device_id(device: &ComRef) -> Option<String> {
    let mut id_ptr: *mut u16 = std::ptr::null_mut();
    // SAFETY : lecture de l'identifiant du périphérique.
    let get_id_fn: GetIdFn = unsafe { std::mem::transmute(vtable_slot(device.ptr(), 5)) };
    if unsafe { get_id_fn(device.ptr(), &mut id_ptr) } < 0 || id_ptr.is_null() {
        return None;
    }
    let len = (0..1024).find(|&i| unsafe { *id_ptr.add(i) } == 0).unwrap_or(0);
    let id = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(id_ptr, len) });
    // SAFETY : libération de la mémoire allouée par GetId (CoTaskMemAlloc).
    unsafe { CoTaskMemFree(id_ptr as *mut std::ffi::c_void) };
    Some(id)
}

// ---------------------------------------------------------------------------
// Noms conviviaux + listes publiques
// ---------------------------------------------------------------------------

/// Identifiants (non emballés) des périphériques d'un flux selon le masque
/// d'état (DEVICE_STATE_ACTIVE, ou DEVICE_STATE_ALL = 0xF pour inclure les
/// périphériques désactivés/débranchés).
#[cfg(test)] // utilisé seulement par les tests de bout en bout
pub(super) fn list_device_ids_by_state(flow: Flow, state_mask: u32) -> Vec<String> {
    let Some(devices) = enum_devices(flow, state_mask).unwrap_or(None) else {
        return Vec::new();
    };

    let mut ids = Vec::new();
    for index in 0..collection_count(&devices) {
        let Some(device) = collection_item(&devices, index) else {
            continue;
        };
        let Some(id) = device_id(&device) else {
            continue;
        };
        ids.push(id);
    }
    ids
}

/// Identifiants (non emballés) des périphériques actifs d'un flux — utilisé
/// par les tests de bout en bout pour choisir une cible de routage.
#[cfg(test)]
pub(super) fn list_device_ids_noinit(flow: Flow) -> Vec<String> {
    list_device_ids_by_state(flow, DEVICE_STATE_ACTIVE)
}

/// Périphériques actifs d'un flux, version thread-safe : exécute
/// l'énumération sur le thread d'appartement COM (à utiliser hors de
/// l'application, par ex. la CLI `route`).
pub fn active_devices(flow: Flow) -> Vec<(String, String)> {
    with_apartment(move || list_active_devices(flow)).unwrap_or_default()
}

/// Périphériques actifs d'un flux : `(identifiant non emballé, nom
/// convivial)`. Le nom vient de `PKEY_Device_FriendlyName` (IPropertyStore),
/// comme dans le panneau Son de Windows — sans passer par PowerShell.
/// Énumération brute (sans appartement COM) — réservée aux tests et au
/// wrapper `active_devices` ; en production, passer par `active_devices`.
pub(super) fn list_active_devices(flow: Flow) -> Vec<(String, String)> {
    type OpenPropertyStoreFn = unsafe extern "system" fn(
        *mut std::ffi::c_void,
        u32,
        *mut *mut std::ffi::c_void,
    ) -> i32;
    type GetValueFn = unsafe extern "system" fn(
        *mut std::ffi::c_void,
        *const PropertyKey,
        *mut PropVariant,
    ) -> i32;

    let Some(devices) = enum_devices(flow, DEVICE_STATE_ACTIVE).unwrap_or(None) else {
        return Vec::new();
    };

    let mut result = Vec::new();
    for index in 0..collection_count(&devices) {
        let Some(device) = collection_item(&devices, index) else {
            continue;
        };

        let Some(id) = device_id(&device) else {
            continue;
        };

        // Nom convivial via IPropertyStore (PKEY_Device_FriendlyName).
        let mut name = String::new();
        let mut store_ptr = std::ptr::null_mut();
        // SAFETY : ouverture du magasin de propriétés du périphérique.
        let open_store: OpenPropertyStoreFn =
            unsafe { std::mem::transmute(vtable_slot(device.ptr(), 4)) };
        let hr =
            unsafe { open_store(device.ptr(), STGM_READ, &mut store_ptr) };
        if hr >= 0 && !store_ptr.is_null() {
            let store = ComRef::new(store_ptr);
            let mut value: PropVariant = unsafe { std::mem::zeroed() };
            // SAFETY : lecture de la valeur du nom convivial.
            let get_value: GetValueFn =
                unsafe { std::mem::transmute(vtable_slot(store.ptr(), 5)) };
            let hr = unsafe { get_value(store.ptr(), &PKEY_DEVICE_FRIENDLY_NAME, &mut value) };
            if hr >= 0 && value.vt == VT_LPWSTR && !value.psz_val.is_null() {
                let len = (0..1024).find(|&i| unsafe { *value.psz_val.add(i) } == 0).unwrap_or(0);
                name = String::from_utf16_lossy(unsafe {
                    std::slice::from_raw_parts(value.psz_val, len)
                });
            }
            // SAFETY : libération du PROPVARIANT (même si GetValue a échoué).
            unsafe { PropVariantClear(&mut value) };
        }

        result.push((id, name));
    }
    result
}
