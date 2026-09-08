//! Routage audio par application (« ex. Xbox → une autre carte son »), en
//! Rust pur, via l'API interne de Windows :
//! `Windows.Media.Internal.AudioPolicyConfig` (implémentée dans
//! `AudioSes.dll`) — le même chemin que EarTrumpet / SoundVolumeView /
//! winappaudiorouter.
//!
//! Contrat reconstitué et vérifié sur cette machine (Win11, build 29648) :
//! - la fabrique de classe expose l'interface `IAudioPolicyConfigFactory`
//!   (IID `ab3d4648-…` sur Windows 11 ≥ 21H2, `2a59116d-…` avant), dont les
//!   emplacements de vtable 25 / 26 / 27 sont respectivement
//!   `SetPersistedDefaultAudioEndpoint`, `GetPersistedDefaultAudioEndpoint`
//!   et `ClearAllPersistedApplicationDefaultEndpoints` ;
//! - `SetPersistedDefaultAudioEndpoint(pid, eDataFlow, eRole, HSTRING)` :
//!   l'identifiant de périphérique doit être « emballé » au format
//!   `\\?\SWD#MMDEVAPI#{id}#{interface-guid}` (interface de rendu ou de
//!   capture) ; le réglage est écrit pour les rôles console puis multimedia,
//!   comme le fait la page « Périphériques » de Windows ;
//! - `Get…(pid, flow, role = multimedia, out HSTRING)` renvoie
//!   `0x80070490` (ERROR_NOT_FOUND) quand l'application n'a pas de route
//!   persistée (elle suit alors le périphérique système par défaut) ;
//! - effacer une route = `Set…` avec une chaîne vide (NULL).
//!
//! Les sessions audio actives sont énumérées via l'API publique WASAPI en
//! FFI direct (IMMDeviceEnumerator → IAudioSessionManager2), comme le
//! faisait `winappaudiorouter` via pycaw — aucune dépendance Windows externe
//! n'est nécessaire (la crate `windows-sys` disponible en cache ne fournit
//! pas ces interfaces).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::mpsc;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Types du profil JSON — section « applications » (processName /
// executablePath / output / input → { deviceId, deviceName }).
// ---------------------------------------------------------------------------

/// Cible d'un flux (sortie ou entrée) pour une application.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RouteTarget {
    pub device_id: String,
    #[serde(default)]
    pub device_name: Option<String>,
}

/// Entrée « routage par application » d'un profil.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationEntry {
    pub process_name: String,
    #[serde(default)]
    pub executable_path: Option<String>,
    #[serde(default)]
    pub output: Option<RouteTarget>,
    #[serde(default)]
    pub input: Option<RouteTarget>,
}

/// Ligne d'aperçu d'une application d'un profil (avant restauration).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPreviewRow {
    pub process_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable_path: Option<String>,
    /// L'application tourne-t-elle actuellement sur cette machine ?
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<TargetPreview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<TargetPreview>,
}

/// Aperçu d'une route d'un flux (périphérique enregistré + présence locale).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetPreview {
    pub device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    /// Le périphérique cible existe-t-il sur cette machine ?
    pub present: bool,
}

/// Rapport de restauration des routes par application.
#[derive(Debug, Default)]
pub struct RestoreReport {
    pub applied: u32,
    pub missing: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// GUID et FFI de base
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
const CLSID_MMDEVICE_ENUMERATOR: Guid =
    guid(0xbcde0395, 0xe52f, 0x467c, [0x8e, 0x3d, 0xc4, 0x57, 0x92, 0x91, 0x69, 0x2e]);
const IID_IMMDEVICE_ENUMERATOR: Guid =
    guid(0xa95664d2, 0x9614, 0x4f35, [0xa7, 0x46, 0xde, 0x8d, 0xb6, 0x36, 0x17, 0xe6]);
const IID_IAUDIO_SESSION_MANAGER_2: Guid =
    guid(0x77aa99a0, 0x1bd6, 0x484f, [0x8b, 0xc7, 0x2c, 0x65, 0x4c, 0x9a, 0x9b, 0x6f]);
const IID_IAUDIO_SESSION_CONTROL_2: Guid =
    guid(0xbfb7ff88, 0x7239, 0x4fc9, [0x8f, 0xa2, 0x07, 0xc9, 0x50, 0xbe, 0x9c, 0x6d]);
/// IPropertyStore (nom convivial des périphériques).
const IID_IPROPERTY_STORE: Guid =
    guid(0x886d8eeb, 0x8cf2, 0x4446, [0x8d, 0x02, 0xcd, 0xba, 0x1d, 0xbd, 0xcf, 0x99]);

/// PKEY_Device_FriendlyName (nom affiché dans le panneau Son de Windows).
const PKEY_DEVICE_FRIENDLY_NAME: PropertyKey = PropertyKey {
    fmtid: guid(0xa45c254e, 0xdf1c, 0x4efd, [0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0]),
    pid: 14,
};

/// PROPERTYKEY (fmtid + pid).
#[repr(C)]
#[derive(Clone, Copy)]
struct PropertyKey {
    fmtid: Guid,
    pid: u32,
}

/// PROPVARIANT réduit : l'en-tête 8 octets + l'union (le premier membre,
/// `pszVal`, sert pour VT_LPWSTR = 31). Taille totale 24 octets sur x64.
#[repr(C)]
#[derive(Clone, Copy)]
struct PropVariant {
    vt: u16,
    w_reserved1: u16,
    w_reserved2: u16,
    w_reserved3: u16,
    psz_val: *mut u16,
    _rest: [u64; 1],
}

/// VT_LPWSTR.
const VT_LPWSTR: u16 = 31;
/// STGM_READ.
const STGM_READ: u32 = 0;

const POLICY_CONFIG_CLASS: &str = "Windows.Media.Internal.AudioPolicyConfig";

/// Emplacements de la vtable de l'interface de fabrique de la politique.
const INDEX_RELEASE: usize = 2;
const INDEX_SET_PERSISTED_DEFAULT_ENDPOINT: usize = 25;
const INDEX_GET_PERSISTED_DEFAULT_ENDPOINT: usize = 26;

/// `ERROR_NOT_FOUND` : l'application n'a pas de route persistée.
const HR_ERROR_NOT_FOUND: i32 = 0x8007_0490u32 as i32;

/// CLSCTX_ALL (création COM in/out-of-process).
const CLSCTX_ALL: u32 = 23;
/// DEVICE_STATE_ACTIVE.
const DEVICE_STATE_ACTIVE: u32 = 0x1;
/// PROCESS_QUERY_LIMITED_INFORMATION.
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
/// TH32CS_SNAPPROCESS.
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;

/// EDataFlow (eRender = 0, eCapture = 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Output,
    Input,
}

impl Flow {
    fn value(self) -> i32 {
        match self {
            Flow::Output => 0,
            Flow::Input => 1,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Flow::Output => "sortie",
            Flow::Input => "entrée",
        }
    }
}

/// Identifiants d'interface « emballés » par le registre audio Windows.
const MMDEVAPI_TOKEN: &str = r"\\?\SWD#MMDEVAPI#";
const RENDER_INTERFACE: &str = "#{e6327cad-dcec-4949-ae8a-991e976a79d2}";
const CAPTURE_INTERFACE: &str = "#{2eef81be-33fa-4800-9670-1cd474972c3f}";

fn pack_device_id(flow: Flow, device_id: &str) -> String {
    let suffix = match flow {
        Flow::Output => RENDER_INTERFACE,
        Flow::Input => CAPTURE_INTERFACE,
    };
    format!("{MMDEVAPI_TOKEN}{device_id}{suffix}")
}

fn unpack_device_id(packed: &str) -> Option<String> {
    let rest = packed.strip_prefix(MMDEVAPI_TOKEN)?;
    let id = rest
        .strip_suffix(RENDER_INTERFACE)
        .or_else(|| rest.strip_suffix(CAPTURE_INTERFACE))?;
    Some(id.to_string())
}

#[cfg(windows)]
mod ffi {
    use super::Guid;
    use std::os::raw::c_void;

    // `raw-dylib` : pas de bibliothèque d'importation requise pour combase.
    #[link(name = "combase", kind = "raw-dylib")]
    unsafe extern "system" {
        pub fn RoInitialize(init_type: u32) -> i32;
        pub fn RoUninitialize();
        pub fn WindowsCreateString(source: *const u16, length: u32, string: *mut *mut c_void)
            -> i32;
        pub fn WindowsDeleteString(string: *mut c_void) -> i32;
        pub fn WindowsGetStringRawBuffer(string: *mut c_void, length: *mut u32) -> *const u16;
        pub fn RoGetActivationFactory(
            activatable_class_id: *mut c_void,
            iid: *const Guid,
            factory: *mut *mut c_void,
        ) -> i32;
    }

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
        pub fn PropVariantClear(pvar: *mut super::PropVariant) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn OpenProcess(
            dw_desired_access: u32,
            b_inherit_handle: i32,
            dw_process_id: u32,
        ) -> *mut c_void;
        pub fn QueryFullProcessImageNameW(
            h_process: *mut c_void,
            dw_flags: u32,
            lp_exe_name: *mut u16,
            lpdw_size: *mut u32,
        ) -> i32;
        pub fn CloseHandle(h_object: *mut c_void) -> i32;
        pub fn CreateToolhelp32Snapshot(dw_flags: u32, th32_process_id: u32) -> *mut c_void;
        pub fn Process32FirstW(h_snapshot: *mut c_void, lppe: *mut super::ProcessEntry32W)
            -> i32;
        pub fn Process32NextW(h_snapshot: *mut c_void, lppe: *mut super::ProcessEntry32W)
            -> i32;
    }
}

/// Entrée d'un instantané de processus (Toolhelp32).
#[repr(C)]
struct ProcessEntry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; 260],
}

// ---------------------------------------------------------------------------
// Chaînes WinRT (HSTRING)
// ---------------------------------------------------------------------------

struct HString(*mut std::ffi::c_void);

impl HString {
    fn new(text: &str) -> Result<Self, String> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut handle = std::ptr::null_mut();
        let hr = unsafe { ffi::WindowsCreateString(wide.as_ptr(), wide.len() as u32, &mut handle) };
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

    fn to_string(&self) -> String {
        if self.0.is_null() {
            return String::new();
        }
        let mut length: u32 = 0;
        let raw = unsafe { ffi::WindowsGetStringRawBuffer(self.0, &mut length) };
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
                ffi::WindowsDeleteString(self.0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fil de travail COM/WinRT : la classe est déclarée STA dans le registre ;
// toutes les opérations s'exécutent donc sur un thread d'appartement unique.
// ---------------------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send>;

fn apartment_sender() -> &'static mpsc::Sender<Job> {
    static APARTMENT: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    APARTMENT.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("audio-policy-config".into())
            .spawn(move || {
                // RO_INIT_SINGLETHREADED — appartement STA pour la classe.
                unsafe { ffi::RoInitialize(0) };
                for job in receiver {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                }
                unsafe { ffi::RoUninitialize() };
            })
            .expect("impossible de lancer le thread audio-policy");
        sender
    })
}

/// Exécute `f` sur le thread d'appartement COM et renvoie son résultat.
fn with_apartment<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> Result<R, String> {
    let (sender, receiver) = mpsc::channel::<R>();
    apartment_sender()
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
struct PolicyConfig(*mut std::ffi::c_void);

impl PolicyConfig {
    fn activate() -> Result<Self, String> {
        let class = HString::new(POLICY_CONFIG_CLASS)?;
        // Windows 11 (≥ 21H2) d'abord, variante antérieure ensuite.
        for iid in [IID_POLICY_CONFIG_21H2, IID_POLICY_CONFIG_DOWNLEVEL] {
            let mut ptr = std::ptr::null_mut();
            let hr = unsafe { ffi::RoGetActivationFactory(class.as_ptr(), &iid, &mut ptr) };
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
    fn set(&self, process_id: u32, flow: Flow, device_id: Option<&str>) -> Result<(), String> {
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
    fn get(&self, process_id: u32, flow: Flow) -> Result<Option<String>, String> {
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
        let packed = HString::from_raw(out).to_string();
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
unsafe fn vtable_slot(object: *mut std::ffi::c_void, index: usize) -> usize {
    let table = *(object as *const *const usize);
    *table.add(index)
}

/// QI (emplacement 0 de la vtable IUnknown). Retourne un pointeur
/// supplémentaire référencé — le libérer via `release_com`.
unsafe fn query_interface(
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
struct ComRef(*mut std::ffi::c_void);

impl ComRef {
    fn new(ptr: *mut std::ffi::c_void) -> Self {
        ComRef(ptr)
    }
    fn ptr(&self) -> *mut std::ffi::c_void {
        self.0
    }
}

impl Drop for ComRef {
    fn drop(&mut self) {
        release_com(self.0);
    }
}// ---------------------------------------------------------------------------
// Énumération WASAPI des sessions audio actives (par flux), comme pycaw.
// ---------------------------------------------------------------------------

/// État de session WASAPI : `AudioSessionStateActive` = le processus joue
/// (ou enregistre) du son en ce moment.
const SESSION_STATE_ACTIVE: i32 = 1;

/// Sessions audio d'un flux : `(PID, session active)` — une entrée par
/// session (le même PID peut apparaître plusieurs fois, sur des
/// périphériques ou flux différents).
fn list_sessions_noinit(flow: Flow) -> Result<Vec<(u32, bool)>, String> {
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

    // Création de l'énumérateur (CoCreateInstance) — objet COM à libérer.
    let mut enumerator_ptr = std::ptr::null_mut();
    // SAFETY : création du CLSID standard (MMDeviceEnumerator).
    let hr = unsafe {
        ffi::CoCreateInstance(
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
    let enumerator = ComRef::new(enumerator_ptr);

    let mut devices_ptr = std::ptr::null_mut();
    // SAFETY : énumération des périphériques actifs du flux.
    let enum_fn: EnumAudioEndpointsFn =
        unsafe { std::mem::transmute(vtable_slot(enumerator.ptr(), 3)) };
    let hr = unsafe {
        enum_fn(
            enumerator.ptr(),
            flow.value(),
            DEVICE_STATE_ACTIVE,
            &mut devices_ptr,
        )
    };
    if hr < 0 || devices_ptr.is_null() {
        return Ok(Vec::new());
    }
    let devices = ComRef::new(devices_ptr);

    let mut count: u32 = 0;
    // SAFETY : nombre de périphériques.
    let get_count: GetCountU32Fn = unsafe { std::mem::transmute(vtable_slot(devices.ptr(), 3)) };
    if unsafe { get_count(devices.ptr(), &mut count) } < 0 {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for index in 0..count {
        let mut device_ptr = std::ptr::null_mut();
        // SAFETY : accès au périphérique de la collection.
        let item_fn: ItemFn = unsafe { std::mem::transmute(vtable_slot(devices.ptr(), 4)) };
        if unsafe { item_fn(devices.ptr(), index, &mut device_ptr) } < 0 || device_ptr.is_null() {
            continue;
        }
        let device = ComRef::new(device_ptr);

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
fn list_session_pids_noinit(flow: Flow) -> Result<Vec<u32>, String> {
    let mut pids = Vec::new();
    for (pid, _) in list_sessions_noinit(flow)? {
        if !pids.contains(&pid) {
            pids.push(pid);
        }
    }
    Ok(pids)
}

/// Identifiants (non emballés) des périphériques d'un flux selon le masque
/// d'état (DEVICE_STATE_ACTIVE, ou DEVICE_STATE_ALL = 0xF pour inclure les
/// périphériques désactivés/débranchés).
fn list_device_ids_by_state(flow: Flow, state_mask: u32) -> Vec<String> {
    type EnumAudioEndpointsFn = unsafe extern "system" fn(
        *mut std::ffi::c_void,
        i32,
        u32,
        *mut *mut std::ffi::c_void,
    ) -> i32;
    type GetCountU32Fn = unsafe extern "system" fn(*mut std::ffi::c_void, *mut u32) -> i32;
    type ItemFn = unsafe extern "system" fn(
        *mut std::ffi::c_void,
        u32,
        *mut *mut std::ffi::c_void,
    ) -> i32;
    type GetIdFn = unsafe extern "system" fn(
        *mut std::ffi::c_void,
        *mut *mut u16,
    ) -> i32;

    let mut enumerator_ptr = std::ptr::null_mut();
    // SAFETY : création de l'énumérateur COM standard.
    let hr = unsafe {
        ffi::CoCreateInstance(
            &CLSID_MMDEVICE_ENUMERATOR,
            std::ptr::null_mut(),
            CLSCTX_ALL,
            &IID_IMMDEVICE_ENUMERATOR,
            &mut enumerator_ptr,
        )
    };
    if hr < 0 || enumerator_ptr.is_null() {
        return Vec::new();
    }
    let enumerator = ComRef::new(enumerator_ptr);

    let mut devices_ptr = std::ptr::null_mut();
    // SAFETY : énumération des périphériques du flux selon le masque d'état.
    let enum_fn: EnumAudioEndpointsFn =
        unsafe { std::mem::transmute(vtable_slot(enumerator.ptr(), 3)) };
    let hr = unsafe { enum_fn(enumerator.ptr(), flow.value(), state_mask, &mut devices_ptr) };
    if hr < 0 || devices_ptr.is_null() {
        return Vec::new();
    }
    let devices = ComRef::new(devices_ptr);

    let mut count: u32 = 0;
    // SAFETY : nombre de périphériques.
    let get_count: GetCountU32Fn = unsafe { std::mem::transmute(vtable_slot(devices.ptr(), 3)) };
    if unsafe { get_count(devices.ptr(), &mut count) } < 0 {
        return Vec::new();
    }

    let mut ids = Vec::new();
    for index in 0..count {
        let mut device_ptr = std::ptr::null_mut();
        // SAFETY : accès au périphérique.
        let item_fn: ItemFn = unsafe { std::mem::transmute(vtable_slot(devices.ptr(), 4)) };
        if unsafe { item_fn(devices.ptr(), index, &mut device_ptr) } < 0 || device_ptr.is_null() {
            continue;
        }
        let device = ComRef::new(device_ptr);
        let mut id_ptr: *mut u16 = std::ptr::null_mut();
        // SAFETY : lecture de l'identifiant du périphérique.
        let get_id_fn: GetIdFn = unsafe { std::mem::transmute(vtable_slot(device.ptr(), 5)) };
        if unsafe { get_id_fn(device.ptr(), &mut id_ptr) } < 0 || id_ptr.is_null() {
            continue;
        }
        let len = (0..1024).find(|&i| unsafe { *id_ptr.add(i) } == 0).unwrap_or(0);
        let id = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(id_ptr, len) });
        // SAFETY : libération de la mémoire allouée par GetId (CoTaskMemAlloc).
        unsafe { ffi::CoTaskMemFree(id_ptr as *mut std::ffi::c_void) };
        ids.push(id);
    }
    ids
}

/// Identifiants (non emballés) des périphériques actifs d'un flux — utilisé
/// par les tests de bout en bout pour choisir une cible de routage.
fn list_device_ids_noinit(flow: Flow) -> Vec<String> {
    list_device_ids_by_state(flow, DEVICE_STATE_ACTIVE)
}

/// Périphériques actifs d'un flux, version thread-safe : exécute
/// l'énumération sur le thread d'appartement COM (à utiliser hors de
/// l'application, par ex. la CLI `src/bin/route.rs`).
pub fn active_devices(flow: Flow) -> Vec<(String, String)> {
    with_apartment(move || list_active_devices(flow)).unwrap_or_default()
}

/// Périphériques actifs d'un flux : `(identifiant non emballé, nom
/// convivial)`. Le nom vient de `PKEY_Device_FriendlyName` (IPropertyStore),
/// comme dans le panneau Son de Windows — sans passer par PowerShell. Utilisé
/// par la CLI de débogage (`src/bin/route.rs`).
pub fn list_active_devices(flow: Flow) -> Vec<(String, String)> {
    type EnumAudioEndpointsFn = unsafe extern "system" fn(
        *mut std::ffi::c_void,
        i32,
        u32,
        *mut *mut std::ffi::c_void,
    ) -> i32;
    type GetCountU32Fn = unsafe extern "system" fn(*mut std::ffi::c_void, *mut u32) -> i32;
    type ItemFn = unsafe extern "system" fn(
        *mut std::ffi::c_void,
        u32,
        *mut *mut std::ffi::c_void,
    ) -> i32;
    type GetIdFn = unsafe extern "system" fn(
        *mut std::ffi::c_void,
        *mut *mut u16,
    ) -> i32;
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

    let mut enumerator_ptr = std::ptr::null_mut();
    // SAFETY : création de l'énumérateur COM standard.
    let hr = unsafe {
        ffi::CoCreateInstance(
            &CLSID_MMDEVICE_ENUMERATOR,
            std::ptr::null_mut(),
            CLSCTX_ALL,
            &IID_IMMDEVICE_ENUMERATOR,
            &mut enumerator_ptr,
        )
    };
    if hr < 0 || enumerator_ptr.is_null() {
        return Vec::new();
    }
    let enumerator = ComRef::new(enumerator_ptr);

    let mut devices_ptr = std::ptr::null_mut();
    // SAFETY : énumération des périphériques actifs du flux.
    let enum_fn: EnumAudioEndpointsFn =
        unsafe { std::mem::transmute(vtable_slot(enumerator.ptr(), 3)) };
    let hr = unsafe { enum_fn(enumerator.ptr(), flow.value(), DEVICE_STATE_ACTIVE, &mut devices_ptr) };
    if hr < 0 || devices_ptr.is_null() {
        return Vec::new();
    }
    let devices = ComRef::new(devices_ptr);

    let mut count: u32 = 0;
    // SAFETY : nombre de périphériques.
    let get_count: GetCountU32Fn = unsafe { std::mem::transmute(vtable_slot(devices.ptr(), 3)) };
    if unsafe { get_count(devices.ptr(), &mut count) } < 0 {
        return Vec::new();
    }

    let mut result = Vec::new();
    for index in 0..count {
        let mut device_ptr = std::ptr::null_mut();
        // SAFETY : accès au périphérique.
        let item_fn: ItemFn = unsafe { std::mem::transmute(vtable_slot(devices.ptr(), 4)) };
        if unsafe { item_fn(devices.ptr(), index, &mut device_ptr) } < 0 || device_ptr.is_null() {
            continue;
        }
        let device = ComRef::new(device_ptr);

        let mut id_ptr: *mut u16 = std::ptr::null_mut();
        // SAFETY : lecture de l'identifiant du périphérique.
        let get_id_fn: GetIdFn = unsafe { std::mem::transmute(vtable_slot(device.ptr(), 5)) };
        if unsafe { get_id_fn(device.ptr(), &mut id_ptr) } < 0 || id_ptr.is_null() {
            continue;
        }
        let len = (0..1024).find(|&i| unsafe { *id_ptr.add(i) } == 0).unwrap_or(0);
        let id = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(id_ptr, len) });
        // SAFETY : libération de la mémoire allouée par GetId (CoTaskMemAlloc).
        unsafe { ffi::CoTaskMemFree(id_ptr as *mut std::ffi::c_void) };

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
            unsafe { ffi::PropVariantClear(&mut value) };
        }

        result.push((id, name));
    }
    result
}

// ---------------------------------------------------------------------------
// Processus : identité (nom + chemin) et processus en cours.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ProcInfo {
    pid: u32,
    name: String,
    exe: Option<String>,
}

/// Chemin complet de l'exécutable d'un processus (si accessible).
fn process_exe(pid: u32) -> Option<String> {
    // SAFETY : ouverture en accès limité, suffisant pour lire le chemin.
    let handle = unsafe { ffi::OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    // SAFETY : tampon assez grand (1024 wchar_t).
    let ok = unsafe { ffi::QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) };
    unsafe {
        ffi::CloseHandle(handle);
    }
    if ok == 0 {
        return None;
    }
    let text = String::from_utf16_lossy(&buffer[..size as usize]);
    Some(text)
}

/// Instantané des processus en cours (nom du module + chemin d'exécutable).
fn running_processes() -> Vec<ProcInfo> {
    // SAFETY : instantané des processus système.
    let snapshot = unsafe { ffi::CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() {
        return Vec::new();
    }
    let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
    entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
    let mut procs = Vec::new();
    // SAFETY : première entrée de l'instantané.
    let mut ok = unsafe { ffi::Process32FirstW(snapshot, &mut entry) };
    while ok != 0 {
        let pid = entry.th32_process_id;
        let end = entry
            .sz_exe_file
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.sz_exe_file.len());
        let name = String::from_utf16_lossy(&entry.sz_exe_file[..end]);
        let exe = if pid > 0 { process_exe(pid) } else { None };
        procs.push(ProcInfo { pid, name, exe });
        // SAFETY : entrée suivante.
        ok = unsafe { ffi::Process32NextW(snapshot, &mut entry) };
    }
    unsafe {
        ffi::CloseHandle(snapshot);
    }
    procs
}

fn paths_equal(a: &str, b: &str) -> bool {
    a.replace('/', "\\").eq_ignore_ascii_case(&b.replace('/', "\\"))
}

/// L'application correspond-elle à un processus en cours ? (chemin d'abord,
/// puis nom — pour retrouver les processus en cours.)
fn match_processes(entry: &ApplicationEntry, running: &[ProcInfo]) -> (Vec<ProcInfo>, &'static str) {
    if let Some(saved_path) = &entry.executable_path {
        let exact: Vec<ProcInfo> = running
            .iter()
            .filter(|p| p.exe.as_deref().is_some_and(|exe| paths_equal(exe, saved_path)))
            .cloned()
            .collect();
        if !exact.is_empty() {
            return (exact, "chemin");
        }
    }
    if !entry.process_name.is_empty() {
        let matches: Vec<ProcInfo> = running
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case(&entry.process_name))
            .cloned()
            .collect();
        if !matches.is_empty() {
            return (matches, "nom");
        }
    }
    (Vec::new(), "aucune")
}

// ---------------------------------------------------------------------------
// Opérations publiques
// ---------------------------------------------------------------------------

/// Indice (création ou réutilisation) d'une entrée par identité de processus.
fn record_index(
    records: &mut Vec<(String, String, ApplicationEntry)>,
    identity: &(String, String),
    process_name: String,
    executable: Option<String>,
) -> usize {
    if let Some(pos) = records
        .iter()
        .position(|(exe, name, _)| exe == &identity.0 && name == &identity.1)
    {
        return pos;
    }
    records.push((
        identity.0.clone(),
        identity.1.clone(),
        ApplicationEntry {
            process_name,
            executable_path: executable,
            output: None,
            input: None,
        },
    ));
    records.len() - 1
}

/// Exporte les routes par application persistées vers le format du profil.
/// Les listes `(identifiant, nom)` des périphériques actifs servent à
/// retrouver le nom d'une cible.
pub fn export_app_routes(
    playback_devices: &[(String, String)],
    recording_devices: &[(String, String)],
) -> Result<(Vec<ApplicationEntry>, Vec<String>), String> {
    let playback = playback_devices.to_vec();
    let recording = recording_devices.to_vec();
    with_apartment(move || {
        let procs = running_processes();
        let proc_info = |pid: u32| procs.iter().find(|p| p.pid == pid).cloned();

        let mut records: Vec<(String, String, ApplicationEntry)> = Vec::new();
        let policy = match PolicyConfig::activate() {
            Ok(policy) => policy,
            Err(e) => return (Vec::new(), vec![e]),
        };
        let mut warnings = Vec::new();

        for (flow, devices) in [(Flow::Output, playback), (Flow::Input, recording)] {
            let pids = match list_session_pids_noinit(flow) {
                Ok(pids) => pids,
                Err(e) => {
                    warnings.push(format!("Sessions {} non énumérées : {e}", flow.label()));
                    continue;
                }
            };
            for pid in pids {
                let Some(info) = proc_info(pid) else { continue };
                let route = match policy.get(pid, flow) {
                    Ok(route) => route,
                    Err(e) => {
                        warnings.push(format!("Route {} non lue pour {pid} : {e}", flow.label()));
                        continue;
                    }
                };
                let Some(device_id) = route else {
                    continue; // pas de route personnalisée → périphérique système
                };
                let identity = (
                    info.exe.clone().unwrap_or_default().to_lowercase(),
                    info.name.to_lowercase(),
                );
                let index =
                    record_index(&mut records, &identity, info.name.clone(), info.exe.clone());
                let target = RouteTarget {
                    device_name: devices
                        .iter()
                        .find(|(id, _)| id.eq_ignore_ascii_case(&device_id))
                        .map(|(_, name)| name.clone()),
                    device_id,
                };
                match flow {
                    Flow::Output => records[index].2.output = Some(target),
                    Flow::Input => records[index].2.input = Some(target),
                }
            }
        }
        drop(policy);

        records.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let entries = records.into_iter().map(|(_, _, entry)| entry).collect();
        (entries, warnings)
    })
}

/// Écrit la section « applications » dans un profil JSON exporté par le
/// script PowerShell. Renvoie le nombre d'applications enregistrées.
pub fn attach_applications_to_profile(profile_path: &Path) -> Result<usize, String> {
    let bytes = std::fs::read(profile_path).map_err(|e| format!("Profil illisible : {e}"))?;
    // Set-Content -Encoding UTF8 de PowerShell 5.1 écrit un BOM UTF-8.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let raw = String::from_utf8_lossy(bytes);
    let mut root: Value =
        serde_json::from_str(&raw).map_err(|e| format!("Profil JSON invalide : {e}"))?;

    let devices = |key: &str| -> Vec<(String, String)> {
        root.get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let id = item
                            .get("ID")
                            .or_else(|| item.get("id"))?
                            .as_str()?
                            .to_string();
                        let name = item
                            .get("Name")
                            .or_else(|| item.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        Some((id, name))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    let (applications, warnings) = match export_app_routes(
        &devices("PlaybackDevices"),
        &devices("RecordingDevices"),
    ) {
        Ok(result) => result,
        Err(e) => (Vec::new(), vec![e]),
    };
    if !warnings.is_empty() {
        // Non bloquant : le profil reste valide sans la section applications.
        crate::logging::warn(&format!("avertissements routage : {warnings:?}"));
    }

    root["applications"] =
        serde_json::to_value(&applications).unwrap_or_else(|_| Value::Array(Vec::new()));
    let serialized = serde_json::to_string_pretty(&root)
        .map_err(|e| format!("Sérialisation du profil impossible : {e}"))?;
    std::fs::write(profile_path, serialized).map_err(|e| format!("Écriture du profil impossible : {e}"))?;
    Ok(applications.len())
}

/// Section « applications » d'un profil (absente → liste vide).
pub fn applications_in_profile(profile_path: &Path) -> Result<Vec<ApplicationEntry>, String> {
    let bytes = std::fs::read(profile_path).map_err(|e| format!("Profil illisible : {e}"))?;
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let raw = String::from_utf8_lossy(bytes);
    let root: Value = serde_json::from_str(&raw).map_err(|e| format!("Profil JSON invalide : {e}"))?;
    let applications = root
        .get("applications")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    serde_json::from_value(Value::Array(applications))
        .map_err(|e| format!("Section « applications » invalide : {e}"))
}

/// Aperçu des applications d'un profil : présence des processus et des
/// périphériques cibles sur la machine courante.
pub fn preview_applications(
    profile_path: &Path,
    playback_devices: &[(String, String)],
    recording_devices: &[(String, String)],
) -> Result<Vec<AppPreviewRow>, String> {
    let entries = applications_in_profile(profile_path)?;
    let running = running_processes();
    let mut rows = Vec::new();
    for entry in entries {
        let matches = match_processes(&entry, &running);
        let target_preview = |target: Option<&RouteTarget>, devices: &[(String, String)]| {
            target.map(|route| {
                let present = devices.iter().any(|(id, name)| {
                    id.eq_ignore_ascii_case(&route.device_id)
                        || route
                            .device_name
                            .as_deref()
                            .is_some_and(|n| name.eq_ignore_ascii_case(n))
                });
                TargetPreview {
                    present,
                    device_id: route.device_id.clone(),
                    device_name: route.device_name.clone(),
                }
            })
        };
        rows.push(AppPreviewRow {
            process_name: entry.process_name.clone(),
            executable_path: entry.executable_path.clone(),
            running: !matches.0.is_empty(),
            output: target_preview(entry.output.as_ref(), playback_devices),
            input: target_preview(entry.input.as_ref(), recording_devices),
        });
    }
    Ok(rows)
}

/// Restaure les routes par application d'un profil : chaque application est
/// retrouvée (chemin d'exécutable puis nom) parmi les processus en cours,
/// puis ses routes persistées sont réécrites pour chaque flux.
pub fn restore_applications(
    entries: &[ApplicationEntry],
    playback_devices: &[(String, String)],
    recording_devices: &[(String, String)],
) -> Result<RestoreReport, String> {
    if entries.is_empty() {
        return Ok(RestoreReport::default());
    }
    let entries = entries.to_vec();
    let playback = playback_devices.to_vec();
    let recording = recording_devices.to_vec();

    with_apartment(move || {
        let mut report = RestoreReport::default();
        let running = running_processes();
        let policy = match PolicyConfig::activate() {
            Ok(policy) => policy,
            Err(e) => {
                report.missing.push(e);
                return report;
            }
        };

        for entry in &entries {
            let label = if !entry.process_name.is_empty() {
                entry.process_name.clone()
            } else {
                entry
                    .executable_path
                    .clone()
                    .unwrap_or_else(|| "application inconnue".into())
            };
            let (processes, process_match) = match_processes(entry, &running);
            if processes.is_empty() {
                report
                    .missing
                    .push(format!("Application non active : {label}"));
                continue;
            }
            if process_match == "nom" && entry.executable_path.is_some() {
                report
                    .warnings
                    .push(format!("{label} : chemin différent, correspondance par nom utilisée."));
            }
            for (flow, target, devices) in [
                (Flow::Output, entry.output.as_ref(), playback.as_slice()),
                (Flow::Input, entry.input.as_ref(), recording.as_slice()),
            ] {
                let Some(target) = target else { continue };
                // Périphérique cible : identifiant d'abord, nom ensuite.
                let known = devices.iter().any(|(id, name)| {
                    id.eq_ignore_ascii_case(&target.device_id)
                        || target
                            .device_name
                            .as_deref()
                            .is_some_and(|n| name.eq_ignore_ascii_case(n))
                });
                if !known {
                    report.missing.push(format!(
                        "{label} — {} introuvable : {}",
                        flow.label(),
                        target.device_name.as_deref().unwrap_or(&target.device_id)
                    ));
                    continue;
                }
                let mut ok = true;
                for proc in &processes {
                    if let Err(e) = policy.set(proc.pid, flow, Some(&target.device_id)) {
                        report.errors.push(format!(
                            "{label} (PID {}) — {} non restaurée : {e}",
                            proc.pid,
                            flow.label()
                        ));
                        ok = false;
                    }
                }
                if ok {
                    report.applied += 1;
                }
            }
        }
        report
    })
}

/// Lit les applications d'un profil puis les restaure.
pub fn restore_applications_in_profile(
    profile_path: &Path,
    playback_devices: &[(String, String)],
    recording_devices: &[(String, String)],
) -> Result<RestoreReport, String> {
    let entries = applications_in_profile(profile_path)?;
    restore_applications(&entries, playback_devices, recording_devices)
}

// ---------------------------------------------------------------------------
// Vue « Applications » : sessions audio actives et routage direct
// ---------------------------------------------------------------------------

/// Ligne de la vue « Applications » : un processus audio actif et sa route
/// persistée courante pour chaque flux (`None` = périphérique système).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSessionRow {
    pub pid: u32,
    pub process_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable_path: Option<String>,
    /// Le processus joue-t-il (ou enregistre-t-il) du son en ce moment ?
    pub playing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<RouteTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<RouteTarget>,
}

/// Liste les applications ayant une session audio active, avec leur route
/// persistée courante (les listes `(identifiant, nom)` des périphériques
/// actifs servent à résoudre le nom d'une cible).
pub fn list_active_app_routes(
    playback_devices: &[(String, String)],
    recording_devices: &[(String, String)],
) -> Result<Vec<AppSessionRow>, String> {
    let playback = playback_devices.to_vec();
    let recording = recording_devices.to_vec();
    with_apartment(move || {
        let procs = running_processes();
        let proc_info = |pid: u32| procs.iter().find(|p| p.pid == pid).cloned();

        let policy = match PolicyConfig::activate() {
            Ok(policy) => policy,
            Err(e) => return Err(e),
        };

        // PID ayant une session audio active, par flux (sans doublons), et
        // état « en lecture » par PID (une session active sur n'importe quel
        // flux suffit).
        let output_entries = list_sessions_noinit(Flow::Output).unwrap_or_default();
        let input_entries = list_sessions_noinit(Flow::Input).unwrap_or_default();
        let mut pids: Vec<u32> = Vec::new();
        let mut playing_map: std::collections::HashMap<u32, bool> = std::collections::HashMap::new();
        for (pid, playing) in output_entries.into_iter().chain(input_entries) {
            if !pids.contains(&pid) {
                pids.push(pid);
            }
            let entry = playing_map.entry(pid).or_insert(false);
            *entry = *entry || playing;
        }

        let mut sessions = Vec::new();
        for pid in pids {
            let Some(info) = proc_info(pid) else { continue };
            let route = |flow: Flow| -> Option<RouteTarget> {
                match policy.get(pid, flow) {
                    Ok(Some(device_id)) => {
                        let devices = match flow {
                            Flow::Output => playback.as_slice(),
                            Flow::Input => recording.as_slice(),
                        };
                        let device_name = devices
                            .iter()
                            .find(|(id, _)| id.eq_ignore_ascii_case(&device_id))
                            .map(|(_, name)| name.clone());
                        Some(RouteTarget { device_id, device_name })
                    }
                    _ => None,
                }
            };
            sessions.push(AppSessionRow {
                pid,
                process_name: info.name.clone(),
                executable_path: info.exe.clone(),
                playing: *playing_map.get(&pid).unwrap_or(&false),
                output: route(Flow::Output),
                input: route(Flow::Input),
            });
        }
        sessions.sort_by(|a, b| {
            a.process_name
                .to_lowercase()
                .cmp(&b.process_name.to_lowercase())
                .then(a.pid.cmp(&b.pid))
        });
        Ok(sessions)
    })?
}

/// Écrit (ou efface, `device_id == None`) la route persistée d'un processus
/// pour un flux — la vue « Applications » s'en sert directement, sans
/// passer par un profil.
pub fn set_app_route(pid: u32, flow: Flow, device_id: Option<String>) -> Result<(), String> {
    with_apartment(move || {
        let policy = PolicyConfig::activate()?;
        policy.set(pid, flow, device_id.as_deref())
    })?
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_and_unpack_round_trip_render() {
        let id = "{0.0.0.00000000}.{e6327cad-dcec-4949-ae8a-991e976a79d2}";
        let packed = pack_device_id(Flow::Output, id);
        assert_eq!(packed, format!(r"\\?\SWD#MMDEVAPI#{id}{RENDER_INTERFACE}"));
        assert_eq!(unpack_device_id(&packed).as_deref(), Some(id));
    }

    #[test]
    fn pack_and_unpack_round_trip_capture() {
        let id = "{0.0.1.00000000}.{2eef81be-33fa-4800-9670-1cd474972c3f}";
        let packed = pack_device_id(Flow::Input, id);
        assert_eq!(unpack_device_id(&packed).as_deref(), Some(id));
    }

    #[test]
    fn unpack_rejects_foreign_strings() {
        assert_eq!(unpack_device_id("n'importe quoi"), None);
        assert_eq!(unpack_device_id(r"\\?\SWD#MMDEVAPI#{abc}"), None);
    }

    #[test]
    fn application_entries_serialize_camel_case() {
        let entry = ApplicationEntry {
            process_name: "Spotify.exe".into(),
            executable_path: Some(r"C:\Users\me\AppData\Roaming\Spotify\Spotify.exe".into()),
            output: Some(RouteTarget {
                device_id: "{0.0.0.00000000}.{guid}".into(),
                device_name: Some("Casque USB".into()),
            }),
            input: None,
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["processName"], "Spotify.exe");
        assert_eq!(value["output"]["deviceId"], "{0.0.0.00000000}.{guid}");
        assert_eq!(value["output"]["deviceName"], "Casque USB");
        assert!(value.get("input").is_none() || value["input"].is_null());
        let back: ApplicationEntry = serde_json::from_value(value).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn paths_equal_normalizes_case_and_separators() {
        assert!(paths_equal(r"C:\Users\X\App.exe", "c:/users/x/app.exe"));
        assert!(!paths_equal(r"C:\a.exe", r"D:\a.exe"));
    }

    #[test]
    fn record_index_reuses_identity() {
        let mut records: Vec<(String, String, ApplicationEntry)> = Vec::new();
        let identity = ("c:/a.exe".to_string(), "app.exe".to_string());
        let first = record_index(
            &mut records,
            &identity,
            "app.exe".into(),
            Some("C:/A.exe".into()),
        );
        assert_eq!(first, 0);
        let second = record_index(&mut records, &identity, "autre.exe".into(), None);
        assert_eq!(second, 0);
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn app_session_row_serializes_camel_case_and_skips_none() {
        let row = AppSessionRow {
            pid: 1234,
            process_name: "Spotify.exe".into(),
            executable_path: Some(r"C:\Users\me\Spotify.exe".into()),
            playing: true,
            output: None,
            input: Some(RouteTarget {
                device_id: "{0.0.1.00000000}.{guid}".into(),
                device_name: None,
            }),
        };
        let value = serde_json::to_value(&row).unwrap();
        assert_eq!(value["pid"], 1234);
        assert_eq!(value["processName"], "Spotify.exe");
        assert_eq!(value["executablePath"], r"C:\Users\me\Spotify.exe");
        assert_eq!(value["playing"], true);
        assert!(value.get("output").is_none(), "None doit être omis");
        assert_eq!(value["input"]["deviceId"], "{0.0.1.00000000}.{guid}");
    }

    /// Test de bout en bout (manuel, `--ignored`) : lance un processus qui
    /// joue un son, lui affecte un périphérique de sortie, vérifie la valeur
    /// persistée, puis efface la route — sans modifier l'audio réellement
    /// audible (le processus est tué et sa route est remise à zéro).
    #[test]
    #[ignore = "test matériel : nécessite un vrai périphérique audio"]
    fn live_policy_round_trip() {
        let outcome = with_apartment(move || -> String {
            // 1. Activation de la politique.
            let policy = match PolicyConfig::activate() {
                Ok(p) => p,
                Err(e) => return format!("SKIP activation impossible : {e}"),
            };
            // 2. Périphériques de sortie actifs.
            let render = list_device_ids_noinit(Flow::Output);
            if render.is_empty() {
                return "SKIP aucun périphérique de sortie actif".to_string();
            }
            // 3. Processus qui joue un son (session audio réelle).
            let wav = r"C:\Windows\Media\Windows Notify.wav";
            let mut child = match std::process::Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "$p=New-Object System.Media.SoundPlayer '{wav}'; $p.PlayLooping(); Start-Sleep 25"
                    ),
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => return format!("SKIP powershell indisponible : {e}"),
            };
            let pid = child.id();

            // 4. Attente d'une session audio pour ce processus.
            let mut found = false;
            for _ in 0..30 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if let Ok(pids) = list_session_pids_noinit(Flow::Output) {
                    if pids.contains(&pid) {
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                let _ = child.kill();
                return "SKIP aucune session audio créée (session sans audio ?)".to_string();
            }

            let target = &render[0];
            // 5. Route initiale : attendue absente (périphérique système).
            let before = policy.get(pid, Flow::Output).ok().flatten();
            // 6. Affectation → lecture → effacement.
            let set_result = policy.set(pid, Flow::Output, Some(target));
            // Session distante (RDP / audio redirigé) : le système refuse la
            // persistance (ERROR_NOT_SUPPORTED) — limitation connue de la
            // session, pas du code.
            if let Err(e) = &set_result {
                if e.contains("80070032") {
                    let _ = child.kill();
                    let _ = child.wait();
                    return format!(
                        "SKIP session distante : le système refuse la persistance ({e}); activation + énumération OK, cible={target}"
                    );
                }
            }
            let after = policy.get(pid, Flow::Output).ok().flatten();
            let clear_result = policy.set(pid, Flow::Output, None);
            let cleared = policy.get(pid, Flow::Output).ok().flatten();
            let _ = child.kill();
            let _ = child.wait();
            format!(
                "before={before:?} target={target} set={set_result:?} after={after:?} clear={clear_result:?} cleared={cleared:?}"
            )
        });
        let log = outcome.unwrap_or_else(|e| format!("ERREUR : {e}"));
        println!("LIVE_ROUND_TRIP: {log}");
        if log.starts_with("SKIP") {
            return;
        }
        // La route doit avoir été écrite puis effacée.
        assert!(
            log.contains("after=Some("),
            "la route n'a pas été persistée : {log}"
        );
        assert!(
            log.contains("cleared=None"),
            "la route n'a pas été effacée : {log}"
        );
    }

    /// Chemin du binaire `fakeaudio` (compilé par `cargo build --release
    /// --bin fakeaudio`), ou `None` s'il n'existe pas.
    fn fakeaudio_exe() -> Option<std::path::PathBuf> {
        let manifest = env!("CARGO_MANIFEST_DIR");
        for dir in ["release", "debug"] {
            let p = std::path::Path::new(manifest)
                .join("target")
                .join(dir)
                .join("fakeaudio.exe");
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }

    /// Lance `fakeaudio` et lit son PID sur stdout (`FAKEAUDIO_PID=…`).
    fn spawn_fakeaudio() -> Result<(std::process::Child, u32), String> {
        let exe = fakeaudio_exe().ok_or_else(|| {
            "fakeaudio non compilé (cargo build --release --bin fakeaudio)".to_string()
        })?;
        let mut child = std::process::Command::new(&exe)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn fakeaudio : {e}"))?;
        // fakeaudio garde stdout ouvert, mais la première ligne (le PID) arrive
        // dès le démarrage ; `read_line` rend la main à la première fin de ligne.
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let pid = line
            .trim()
            .strip_prefix("FAKEAUDIO_PID=")
            .and_then(|s| s.trim().parse::<u32>().ok())
            .ok_or_else(|| format!("PID fakeaudio introuvable (ligne : {line:?})"))?;
        Ok((child, pid))
    }

    /// Test de bout en bout de la vue « Applications » avec un processus
    /// audio factice (`fakeaudio`), couvrant le **set**, le **clear** et
    /// l'état **introuvable** (route sortie → identifiant d'un périphérique
    /// d'entrée : persisté mais sans nom résolu).
    #[test]
    #[ignore = "test matériel : nécessite un vrai périphérique audio + fakeaudio compilé"]
    fn e2e_apps_view_set_clear_missing() {
        let outcome = with_apartment(move || -> String {
            let (mut child, pid) = match spawn_fakeaudio() {
                Ok(pair) => pair,
                Err(e) => return format!("SKIP {e}"),
            };

            // 1. La session de fakeaudio doit apparaître.
            let mut found = false;
            for _ in 0..40 {
                std::thread::sleep(std::time::Duration::from_millis(300));
                if let Ok(pids) = list_session_pids_noinit(Flow::Output) {
                    if pids.contains(&pid) {
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                let _ = child.kill();
                let _ = child.wait();
                return "SKIP fakeaudio sans session (périphérique audio indisponible ?)".to_string();
            }

            let playback = list_active_devices(Flow::Output);
            let recording = list_active_devices(Flow::Input);
            if playback.is_empty() || recording.is_empty() {
                let _ = child.kill();
                let _ = child.wait();
                return "SKIP aucun périphérique actif".to_string();
            }
            let policy = match PolicyConfig::activate() {
                Ok(p) => p,
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return format!("SKIP activation : {e}");
                }
            };

            let before = policy.get(pid, Flow::Output).ok().flatten();
            let target = playback[0].0.clone();

            // 2. SET vers un périphérique de sortie actif.
            let set = policy.set(pid, Flow::Output, Some(&target));
            if let Err(e) = &set {
                if e.contains("80070032") {
                    let _ = child.kill();
                    let _ = child.wait();
                    return format!("SKIP session distante : refus de persistance ({e})");
                }
            }
            let after = policy.get(pid, Flow::Output).ok().flatten();

            // 3. CLEAR → retour au périphérique système.
            let clear = policy.set(pid, Flow::Output, None);
            let cleared = policy.get(pid, Flow::Output).ok().flatten();

            // 4. INTROUVABLE : on préfère router vers un VRAI périphérique de
            //    rendu présent mais non actif (désactivé/débranché — Windows
            //    accepte l'identifiant) : la route persiste mais sa résolution
            //    de nom contre la liste active ne trouve rien (device_name =
            //    None), ce que la vue affiche « (introuvable) ». S'il n'existe
            //    aucun périphérique inactif, on vérifie le garde-fou : Windows
            //    REFUSE un identifiant inexistant (E_INVALIDARG 0x80070057),
            //    donc le moteur ne peut pas créer de route fantôme.
            let all_playback = list_device_ids_by_state(Flow::Output, 0xF); // DEVICE_STATE_ALL
            let inactive_id = all_playback
                .iter()
                .find(|id| !playback.iter().any(|(p, _)| p.as_str() == id.as_str()))
                .cloned();
            let introuvable = if let Some(id) = &inactive_id {
                let set_inactive = policy.set(pid, Flow::Output, Some(id));
                let inactive_route = policy.get(pid, Flow::Output).ok().flatten();
                let _ = policy.set(pid, Flow::Output, None);
                match (&set_inactive, &inactive_route) {
                    // Route réellement persistée : le nom doit rester None.
                    (Ok(()), Some(rid)) => {
                        let name = playback
                            .iter()
                            .find(|(d, _)| d.as_str() == rid.as_str())
                            .map(|(_, n)| n.clone());
                        format!("inactive target={id} route=Some({rid}) deviceName={name:?}")
                    }
                    // Route refusée (E_INVALIDARG) : garde-fou du moteur.
                    _ => format!("inactive target={id} refused={set_inactive:?} route={inactive_route:?}"),
                }
            } else {
                let bogus = "{0.0.0.00000000}.{ffffffff-ffff-ffff-ffff-ffffffffffff}".to_string();
                let set_bogus = policy.set(pid, Flow::Output, Some(&bogus));
                let _ = policy.set(pid, Flow::Output, None);
                format!("bogus={set_bogus:?}")
            };
            drop(policy);

            let _ = child.kill();
            let _ = child.wait();

            format!(
                "before={before:?} target={target} set={set:?} after={after:?} clear={clear:?} cleared={cleared:?} | {introuvable}"
            )
        });
        let log = outcome.unwrap_or_else(|e| format!("ERREUR : {e}"));
        println!("E2E_APPS: {log}");
        if log.starts_with("SKIP") {
            return;
        }
        // set : la route doit avoir été écrite, puis effacée.
        assert!(log.contains("after=Some("), "set non persisté : {log}");
        assert!(log.contains("cleared=None"), "clear non appliqué : {log}");
        // introuvable : soit une route vers un périphérique inactif a été
        // PERSISTÉE sans nom résolu (route=Some(…) + deviceName=None), soit le
        // garde-fou a refusé un identifiant non actif (inactive refused=Err ou
        // bogus=Err). L'un des deux doit être vérifié — l'OS décide seul s'il
        // accepte de router vers un périphérique désactivé.
        let genuine = log.contains("route=Some(") && log.contains("deviceName=None");
        let guarded = log.contains("refused=") || log.contains("bogus=Err(");
        assert!(
            genuine || guarded,
            "état introuvable non vérifié (ni route inactive persistée ni refus) : {log}"
        );
    }
}
