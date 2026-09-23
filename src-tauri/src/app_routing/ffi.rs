//! FFI de base du moteur de routage : GUID, chaînes WinRT (HSTRING), COM
//! brut (vtable / QI / Release), thread d'appartement STA et fabrique
//! `AudioPolicyConfig`.

use super::devices::{pack_device_id, unpack_device_id};
use super::Flow;

// ---------------------------------------------------------------------------
// GUID et constantes COM
// ---------------------------------------------------------------------------

/// Identifiant d'interface au format Windows (layout ABI natif).
#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

const fn guid(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Guid {
    Guid { data1, data2, data3, data4 }
}

const IID_POLICY_CONFIG_21H2: Guid =
    guid(0xab3d4648, 0xe242, 0x459f, [0xb0, 0x2f, 0x54, 0x1c, 0x70, 0x30, 0x63, 0x24]);
const IID_POLICY_CONFIG_DOWNLEVEL: Guid =
    guid(0x2a59116d, 0x6c4f, 0x45e0, [0xa7, 0x4f, 0x70, 0x7e, 0x3f, 0xef, 0x92, 0x58]);

/// CLSID/IID publics WASAPI (MMDevice API).
pub(super) const CLSID_MMDEVICE_ENUMERATOR: Guid =
    guid(0xbcde0395, 0xe52f, 0x467c, [0x8e, 0x3d, 0xc4, 0x57, 0x92, 0x91, 0x69, 0x2e]);
pub(super) const IID_IMMDEVICE_ENUMERATOR: Guid =
    guid(0xa95664d2, 0x9614, 0x4f35, [0xa7, 0x46, 0xde, 0x8d, 0xb6, 0x36, 0x17, 0xe6]);
pub(super) const IID_IAUDIO_SESSION_MANAGER_2: Guid =
    guid(0x77aa99a0, 0x1bd6, 0x484f, [0x8b, 0xc7, 0x2c, 0x65, 0x4c, 0x9a, 0x9b, 0x6f]);
pub(super) const IID_IAUDIO_SESSION_CONTROL_2: Guid =
    guid(0xbfb7ff88, 0x7239, 0x4fc9, [0x8f, 0xa2, 0x07, 0xc9, 0x50, 0xbe, 0x9c, 0x6d]);
/// PKEY_Device_FriendlyName (nom affiché dans le panneau Son de Windows).
pub(super) const PKEY_DEVICE_FRIENDLY_NAME: PropertyKey = PropertyKey {
    fmtid: guid(0xa45c254e, 0xdf1c, 0x4efd, [0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0]),
    pid: 14,
};

/// PROPERTYKEY (fmtid + pid).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PropertyKey {
    fmtid: Guid,
    pid: u32,
}

/// PROPVARIANT réduit : l'en-tête 8 octets + l'union (le premier membre,
/// `pszVal`, sert pour VT_LPWSTR = 31). Taille totale 24 octets sur x64.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PropVariant {
    pub(super) vt: u16,
    w_reserved1: u16,
    w_reserved2: u16,
    w_reserved3: u16,
    pub(super) psz_val: *mut u16,
    _rest: [u64; 1],
}

/// VT_LPWSTR.
pub(super) const VT_LPWSTR: u16 = 31;
/// STGM_READ.
pub(super) const STGM_READ: u32 = 0;

const POLICY_CONFIG_CLASS: &str = "Windows.Media.Internal.AudioPolicyConfig";

/// Emplacements de la vtable de l'interface de fabrique de la politique.
const INDEX_RELEASE: usize = 2;
const INDEX_SET_PERSISTED_DEFAULT_ENDPOINT: usize = 25;
const INDEX_GET_PERSISTED_DEFAULT_ENDPOINT: usize = 26;

/// `ERROR_NOT_FOUND` : l'application n'a pas de route persistée.
const HR_ERROR_NOT_FOUND: i32 = 0x8007_0490u32 as i32;

/// CLSCTX_ALL (création COM in/out-of-process).
pub const CLSCTX_ALL: u32 = 23;
/// DEVICE_STATE_ACTIVE.
pub const DEVICE_STATE_ACTIVE: u32 = 0x1;
/// PROCESS_QUERY_LIMITED_INFORMATION.
pub(super) const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
/// TH32CS_SNAPPROCESS.
pub(super) const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;

// ---------------------------------------------------------------------------
// Liaisons externes Windows
// ---------------------------------------------------------------------------

// `raw-dylib` : pas de bibliothèque d'importation requise pour combase.
#[cfg(windows)]
#[link(name = "combase", kind = "raw-dylib")]
unsafe extern "system" {
    pub fn RoInitialize(init_type: u32) -> i32;
    pub fn RoUninitialize();
    pub fn WindowsCreateString(source: *const u16, length: u32, string: *mut *mut c_void) -> i32;
    pub fn WindowsDeleteString(string: *mut c_void) -> i32;
    pub fn WindowsGetStringRawBuffer(string: *mut c_void, length: *mut u32) -> *const u16;
    pub fn RoGetActivationFactory(
        activatable_class_id: *mut c_void,
        iid: *const Guid,
        factory: *mut *mut c_void,
    ) -> i32;
}

#[cfg(windows)]
#[link(name = "ole32")]
extern "system" {
    pub fn CoCreateInstance(
        rclsid: *const Guid,
        punk_outer: *mut c_void,
        dw_cls_context: u32,
        riid: *const Guid,
        ppv: *mut *mut c_void,
    ) -> i32;
    pub fn CoTaskMemFree(pv: *mut c_void);
    pub fn PropVariantClear(pvar: *mut PropVariant) -> i32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    pub fn OpenProcess(dw_desired_access: u32, b_inherit_handle: i32, dw_process_id: u32)
    -> *mut c_void;
    pub fn QueryFullProcessImageNameW(
        h_process: *mut c_void,
        dw_flags: u32,
        lp_exe_name: *mut u16,
        lpdw_size: *mut u32,
    ) -> i32;
    pub fn CloseHandle(h_object: *mut c_void) -> i32;
    pub fn CreateToolhelp32Snapshot(dw_flags: u32, th32_process_id: u32) -> *mut c_void;
    pub fn Process32FirstW(h_snapshot: *mut c_void, lppe: *mut ProcessEntry32W) -> i32;
    pub fn Process32NextW(h_snapshot: *mut c_void, lppe: *mut ProcessEntry32W) -> i32;
}

use std::os::raw::c_void;

/// Entrée d'un instantané de processus (Toolhelp32).
#[repr(C)]
pub struct ProcessEntry32W {
    pub dw_size: u32,
    pub cnt_usage: u32,
    pub th32_process_id: u32,
    pub th32_default_heap_id: usize,
    pub th32_module_id: u32,
    pub cnt_threads: u32,
    pub th32_parent_process_id: u32,
    pub pc_pri_class_base: i32,
    pub dw_flags: u32,
    pub sz_exe_file: [u16; 260],
}

// ---------------------------------------------------------------------------
// Chaînes WinRT (HSTRING)
// ---------------------------------------------------------------------------

struct HString(*mut std::ffi::c_void);

impl HString {
    fn new(text: &str) -> Result<Self, String> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut handle = std::ptr::null_mut();
        let hr = unsafe { WindowsCreateString(wide.as_ptr(), wide.len() as u32, &mut handle) };
        if hr < 0 {
            return Err(format!(
                "Création de chaîne Windows impossible (0x{:08X})",
                hr as u32
            ));
        }
        Ok(HString(handle))
    }

    fn from_raw(handle: *mut std::ffi::c_void) -> Self {
        HString(handle)
    }

    fn as_ptr(&self) -> *mut std::ffi::c_void {
        self.0
    }

    /// Lit la chaîne Windows native en `String` Rust (UTF-16 → UTF-8).
    ///
    /// Nommé `decode` (et non `to_string`) pour ne pas masquer `ToString`,
    /// ce que clippy interdit.
    fn decode(&self) -> String {
        if self.0.is_null() {
            return String::new();
        }
        let mut length: u32 = 0;
        let raw = unsafe { WindowsGetStringRawBuffer(self.0, &mut length) };
        if raw.is_null() {
            return String::new();
        }
        let slice = unsafe { std::slice::from_raw_parts(raw, length as usize) };
        String::from_utf16_lossy(slice)
    }
}

impl Drop for HString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                WindowsDeleteString(self.0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fil de travail COM/WinRT : la classe est déclarée STA dans le registre ;
// toutes les opérations s'exécutent donc sur un thread d'appartement unique.
// ---------------------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send>;

fn apartment_sender() -> Result<&'static mpsc::Sender<Job>, String> {
    static APARTMENT: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    if let Some(sender) = APARTMENT.get() {
        return Ok(sender);
    }
    let (sender, receiver) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("audio-policy-config".into())
        .spawn(move || {
            // RO_INIT_SINGLETHREADED — appartement STA pour la classe.
            unsafe { RoInitialize(0) };
            for job in receiver {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
            }
            unsafe { RoUninitialize() };
        })
        .map_err(|e| format!("impossible de lancer le thread audio-policy : {e}"))?;
    // En cas de course, le doublon est abandonné : son thread se termine
    // dès que son `sender` est libéré (canal fermé).
    Ok(APARTMENT.get_or_init(|| sender))
}

/// Exécute `f` sur le thread d'appartement COM et renvoie son résultat.
pub(super) fn with_apartment<R: Send + 'static>(
    f: impl FnOnce() -> R + Send + 'static,
) -> Result<R, String> {
    let (sender, receiver) = mpsc::channel::<R>();
    apartment_sender()?
        .send(Box::new(move || {
            let _ = sender.send(f());
        }))
        .map_err(|_| "Moteur de routage par application indisponible.".to_string())?;
    receiver
        .recv()
        .map_err(|_| "Moteur de routage par application indisponible.".to_string())
}

/// L'API de politique audio est-elle activable sur ce système ?
pub fn routing_available() -> bool {
    with_apartment(|| PolicyConfig::activate().is_ok()).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Fabrique de politique audio (AudioPolicyConfig)
// ---------------------------------------------------------------------------

type SetPersistedFn = unsafe extern "system" fn(
    this: *mut std::ffi::c_void,
    process_id: u32,
    flow: i32,
    role: i32,
    device_id: *mut std::ffi::c_void,
) -> i32;
type GetPersistedFn = unsafe extern "system" fn(
    this: *mut std::ffi::c_void,
    process_id: u32,
    flow: i32,
    role: i32,
    device_id: *mut *mut std::ffi::c_void,
) -> i32;

/// Pointeur d'interface sur la fabrique AudioPolicyConfig. Ne se manipule
/// que sur le thread d'appartement.
pub(super) struct PolicyConfig(*mut std::ffi::c_void);

impl PolicyConfig {
    pub(super) fn activate() -> Result<Self, String> {
        let class = HString::new(POLICY_CONFIG_CLASS)?;
        // Windows 11 (≥ 21H2) d'abord, variante antérieure ensuite.
        for iid in [IID_POLICY_CONFIG_21H2, IID_POLICY_CONFIG_DOWNLEVEL] {
            let mut ptr = std::ptr::null_mut();
            let hr = unsafe { RoGetActivationFactory(class.as_ptr(), &iid, &mut ptr) };
            if hr >= 0 && !ptr.is_null() {
                return Ok(PolicyConfig(ptr));
            }
        }
        Err("Routage par application indisponible sur ce système.".to_string())
    }

    fn slot(&self, index: usize) -> usize {
        // La vtable est un tableau de pointeurs de fonctions.
        let table = unsafe { *(self.0 as *const *const usize) };
        unsafe { *table.add(index) }
    }

    /// Enregistre la route persistée d'un processus pour un flux.
    /// `device_id == None` efface la route (retour au périphérique système).
    pub(super) fn set(
        &self,
        process_id: u32,
        flow: Flow,
        device_id: Option<&str>,
    ) -> Result<(), String> {
        let packed = device_id.map(|id| pack_device_id(flow, id));
        let device = packed.as_deref().map(HString::new).transpose()?;
        let device_ptr = device
            .as_ref()
            .map(HString::as_ptr)
            .unwrap_or(std::ptr::null_mut());
        let f: SetPersistedFn =
            unsafe { std::mem::transmute(self.slot(INDEX_SET_PERSISTED_DEFAULT_ENDPOINT)) };
        // Écrit pour eConsole puis eMultimedia, comme le réglage de Windows.
        for (role, role_name) in [(0i32, "console"), (1i32, "multimédia")] {
            let hr = unsafe { f(self.0, process_id, flow.value(), role, device_ptr) };
            if hr < 0 {
                return Err(format!(
                    "Routage {} ({}) impossible (0x{:08X})",
                    flow.label(),
                    role_name,
                    hr as u32
                ));
            }
        }
        Ok(())
    }

    /// Lit la route persistée d'un processus (rôle multimédia, comme
    /// winappaudiorouter). `None` = l'application suit le périphérique par
    /// défaut du système.
    pub(super) fn get(&self, process_id: u32, flow: Flow) -> Result<Option<String>, String> {
        let f: GetPersistedFn =
            unsafe { std::mem::transmute(self.slot(INDEX_GET_PERSISTED_DEFAULT_ENDPOINT)) };
        let mut out = std::ptr::null_mut();
        let hr = unsafe { f(self.0, process_id, flow.value(), 1 /* eMultimedia */, &mut out) };
        if hr == HR_ERROR_NOT_FOUND {
            return Ok(None);
        }
        if hr < 0 {
            return Err(format!(
                "Lecture de la route {} impossible (0x{:08X})",
                flow.label(),
                hr as u32
            ));
        }
        if out.is_null() {
            return Ok(None);
        }
        let packed = HString::from_raw(out).decode();
        Ok(unpack_device_id(&packed))
    }
}

impl Drop for PolicyConfig {
    fn drop(&mut self) {
        if !self.0.is_null() {
            type ReleaseFn = unsafe extern "system" fn(*mut std::ffi::c_void) -> u32;
            let f: ReleaseFn = unsafe { std::mem::transmute(self.slot(INDEX_RELEASE)) };
            unsafe {
                f(self.0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Aides COM brutes (vtable, QI, libération)
// ---------------------------------------------------------------------------

/// Emplacement `index` de la vtable d'un objet COM.
pub(super) unsafe fn vtable_slot(object: *mut std::ffi::c_void, index: usize) -> usize {
    let table = *(object as *const *const usize);
    *table.add(index)
}

/// QI (emplacement 0 de la vtable IUnknown). Retourne un pointeur
/// supplémentaire référencé — le libérer via `release_com`.
pub(super) unsafe fn query_interface(
    object: *mut std::ffi::c_void,
    iid: *const Guid,
) -> Result<*mut std::ffi::c_void, String> {
    if object.is_null() {
        return Err("Objet COM nul".to_string());
    }
    type QueryInterfaceFn = unsafe extern "system" fn(
        *mut std::ffi::c_void,
        *const Guid,
        *mut *mut std::ffi::c_void,
    ) -> i32;
    let qi: QueryInterfaceFn = std::mem::transmute(vtable_slot(object, 0));
    let mut out = std::ptr::null_mut();
    let hr = qi(object, iid, &mut out);
    if hr < 0 || out.is_null() {
        Err(format!("Interface COM indisponible (0x{:08X})", hr as u32))
    } else {
        Ok(out)
    }
}

/// Release (emplacement 2) d'un objet COM.
fn release_com(object: *mut std::ffi::c_void) {
    if !object.is_null() {
        type ReleaseFn = unsafe extern "system" fn(*mut std::ffi::c_void) -> u32;
        // SAFETY : objet COM valide.
        let f: ReleaseFn = unsafe { std::mem::transmute(vtable_slot(object, 2)) };
        unsafe {
            f(object);
        }
    }
}

/// Wrapper COM à libération automatique.
pub(super) struct ComRef(*mut std::ffi::c_void);

impl ComRef {
    pub(super) fn new(ptr: *mut std::ffi::c_void) -> Self {
        ComRef(ptr)
    }
    pub(super) fn ptr(&self) -> *mut std::ffi::c_void {
        self.0
    }
}

impl Drop for ComRef {
    fn drop(&mut self) {
        release_com(self.0);
    }
}

use std::sync::mpsc;
use std::sync::OnceLock;
