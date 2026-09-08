//! « Processus audio factice » pour les tests de bout en bout de la vue
//! Applications : ouvre une vraie session de lecture WASAPI (périphérique de
//! rendu actif) et diffuse un son (un très léger bourdonnement, inaudible).
//!
//! Tant qu'il tourne, le processus apparaît dans `app_sessions` / `route
//! sessions` comme une application audio active — de quoi tester le set, le
//! clear et l'état « introuvable » sans toucher aux vraies applications.
//!
//! Affiche `FAKEAUDIO_PID=<pid>` puis tourne jusqu'à être tué.
//!
//! Compilation : `cargo build --release --bin fakeaudio`
//! Binaire : `target/release/fakeaudio.exe`

use std::os::raw::c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
    Guid { data1: d1, data2: d2, data3: d3, data4: d4 }
}

const CLSID_MMDEVICE_ENUMERATOR: Guid =
    guid(0xbcde0395, 0xe52f, 0x467c, [0x8e, 0x3d, 0xc4, 0x57, 0x92, 0x91, 0x69, 0x2e]);
const IID_IMMDEVICE_ENUMERATOR: Guid =
    guid(0xa95664d2, 0x9614, 0x4f35, [0xa7, 0x46, 0xde, 0x8d, 0xb6, 0x36, 0x17, 0xe6]);
const IID_IAUDIO_CLIENT: Guid =
    guid(0x1cb9ad4c, 0xdbfa, 0x4c32, [0xb1, 0x78, 0xc2, 0xf5, 0x68, 0xa7, 0x03, 0xb2]);
const IID_IAUDIO_RENDER_CLIENT: Guid =
    guid(0xf294acfc, 0x3146, 0x4483, [0xa7, 0xbf, 0xad, 0xdc, 0xa7, 0xc2, 0x60, 0xe2]);

const COINIT_APARTMENTTHREADED: u32 = 0x2;
const CLSCTX_ALL: u32 = 23;
const DEVICE_STATE_ACTIVE: u32 = 0x1;
const AUDCLNT_SHAREMODE_SHARED: u32 = 0;
/// AUTOCONVERTPCM | SRC_DEFAULT_QUALITY : laisser Windows convertir le format.
const AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM: u32 = 0x8000_0000;
const AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY: u32 = 0x0800_0000;
const ERENDER: i32 = 0;
const ECONSOLE: i32 = 0;

/// WAVEFORMATEX — PCM 16 bits stéréo 44,1 kHz (partagé : Windows rééchantillonne).
#[repr(C)]
#[derive(Clone, Copy)]
struct WaveFormatEx {
    w_format_tag: u16,
    n_channels: u16,
    n_samples_per_sec: u32,
    n_avg_bytes_per_sec: u32,
    n_block_align: u16,
    w_bits_per_sample: u16,
    cb_size: u16,
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> i32;
    fn CoUninitialize();
    fn CoCreateInstance(
        rclsid: *const Guid,
        outer: *mut c_void,
        ctx: u32,
        riid: *const Guid,
        ppv: *mut *mut c_void,
    ) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn Sleep(ms: u32);
}

type EnumAudioEndpointsFn = unsafe extern "system" fn(*mut c_void, i32, u32, *mut *mut c_void) -> i32;
type ItemFn = unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32;
type ActivateFn = unsafe extern "system" fn(*mut c_void, *const Guid, u32, *mut c_void, *mut *mut c_void) -> i32;
type InitializeFn = unsafe extern "system" fn(*mut c_void, u32, u32, u64, u64, *const WaveFormatEx, *mut c_void) -> i32;
type GetServiceFn = unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32;
type GetBufferSizeFn = unsafe extern "system" fn(*mut c_void, *mut u32) -> i32;
type GetCurrentPaddingFn = unsafe extern "system" fn(*mut c_void, *mut u32) -> i32;
type StartStopFn = unsafe extern "system" fn(*mut c_void) -> i32;
type GetBufferFn = unsafe extern "system" fn(*mut c_void, u32, *mut *mut u8) -> i32;
type ReleaseBufferFn = unsafe extern "system" fn(*mut c_void, u32, u32) -> i32;
type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;

unsafe fn slot(obj: *mut c_void, index: usize) -> usize {
    let table = *(obj as *const *const usize);
    *table.add(index)
}

fn main() {
    let format = WaveFormatEx {
        w_format_tag: 1, // WAVE_FORMAT_PCM
        n_channels: 2,
        n_samples_per_sec: 44100,
        n_avg_bytes_per_sec: 44100 * 2 * 2,
        n_block_align: 4,
        w_bits_per_sample: 16,
        cb_size: 0,
    };

    // Périphérique de rendu : le défaut d'abord, puis chacun des actifs.
    let mut candidates = unsafe { render_devices() };
    if !candidates.is_empty() {
        let default = unsafe { default_render_device() };
        if !default.is_null() {
            candidates.insert(0, default);
        }
    }
    if candidates.is_empty() {
        eprintln!("FAKEAUDIO_ERR : aucun périphérique de rendu actif");
        std::process::exit(2);
    }

    let mut client_ptr = std::ptr::null_mut();
    let mut last_hr = 0;
    for device in &candidates {
        // SAFETY : activation du client audio du périphérique.
        let activate: ActivateFn = unsafe { std::mem::transmute(slot(*device, 3)) };
        let hr = unsafe { activate(*device, &IID_IAUDIO_CLIENT, CLSCTX_ALL, std::ptr::null_mut(), &mut client_ptr) };
        if hr < 0 || client_ptr.is_null() {
            last_hr = hr;
            client_ptr = std::ptr::null_mut();
            continue;
        }
        let init: InitializeFn = unsafe { std::mem::transmute(slot(client_ptr, 3)) };
        // SAFETY : Initialize partagé, 1 s de tampon, conversion PCM auto.
        let hr = unsafe {
            init(
                client_ptr,
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                10_000_000,
                0,
                &format,
                std::ptr::null_mut(),
            )
        };
        if hr >= 0 {
            break;
        }
        last_hr = hr;
        // SAFETY : libération du client non initialisé.
        let release: ReleaseFn = unsafe { std::mem::transmute(slot(client_ptr, 2)) };
        unsafe { release(client_ptr) };
        client_ptr = std::ptr::null_mut();
    }
    if client_ptr.is_null() {
        eprintln!("FAKEAUDIO_ERR : Initialize échoué sur tous les périphériques (dernier 0x{:08X})", last_hr as u32);
        std::process::exit(2);
    }
    let client = client_ptr;

    let mut render_ptr = std::ptr::null_mut();
    // SAFETY : service de rendu.
    let get_service: GetServiceFn = unsafe { std::mem::transmute(slot(client, 14)) };
    let hr = unsafe { get_service(client, &IID_IAUDIO_RENDER_CLIENT, &mut render_ptr) };
    if hr < 0 || render_ptr.is_null() {
        eprintln!("FAKEAUDIO_ERR : GetService IAudioRenderClient (0x{:08X})", hr as u32);
        std::process::exit(2);
    }
    let render = render_ptr;

    let mut buffer_frames: u32 = 0;
    // SAFETY : taille du tampon.
    let get_size: GetBufferSizeFn = unsafe { std::mem::transmute(slot(client, 4)) };
    unsafe { get_size(client, &mut buffer_frames) };
    if buffer_frames == 0 {
        buffer_frames = 4410;
    }

    println!("FAKEAUDIO_PID={}", std::process::id());
    println!("FAKEAUDIO_BUFFER={buffer_frames}");
    // Ne pas bufferiser stdout (sinon le PID n'apparaît pas à temps pour les tests).
    let _ = std::io::Write::flush(&mut std::io::stdout());

    // SAFETY : démarrage du flux.
    let start: StartStopFn = unsafe { std::mem::transmute(slot(client, 10)) };
    if unsafe { start(client) } < 0 {
        eprintln!("FAKEAUDIO_ERR : Start");
        std::process::exit(2);
    }

    // Boucle de rendu : écrit une onde (quasi silencieuse) dès qu'il y a de
    // la place. Le flux ouvert maintient la session WASAPI active.
    let mut phase: f64 = 0.0;
    let render_client = render;
    loop {
        let mut padding: u32 = 0;
        // SAFETY : occupation courante.
        let get_padding: GetCurrentPaddingFn = unsafe { std::mem::transmute(slot(client, 6)) };
        unsafe { get_padding(client, &mut padding) };
        let available = buffer_frames.saturating_sub(padding);
        if available > 0 {
            let mut data: *mut u8 = std::ptr::null_mut();
            // SAFETY : tampon de rendu disponible.
            let get_buffer: GetBufferFn = unsafe { std::mem::transmute(slot(render_client, 3)) };
            if unsafe { get_buffer(render_client, available, &mut data) } >= 0 && !data.is_null() {
                let samples = (available as usize) * 2;
                let buf = unsafe { std::slice::from_raw_parts_mut(data, samples) };
                for i in 0..samples {
                    // Amplitude très faible (100 / 32767) — inaudible.
                    let t = (i as f64) / 2.0;
                    let v = (100.0 * (2.0 * std::f64::consts::PI * 220.0 * t / 44100.0 + phase)).sin();
                    buf[i] = (v as i16).to_le_bytes()[0];
                }
                phase += 0.01;
                // SAFETY : libération du tampon rempli.
                let release: ReleaseBufferFn = unsafe { std::mem::transmute(slot(render_client, 4)) };
                unsafe { release(render_client, available, 0) };
            }
        }
        // SAFETY : attente (pas de blocage).
        unsafe { Sleep(10) };
    }
}

/// Énumère les périphériques de rendu actifs (pointeurs `IMMDevice*`).
type GetCountU32Fn = unsafe extern "system" fn(*mut c_void, *mut u32) -> i32;
type GetDefaultEndpointFn =
    unsafe extern "system" fn(*mut c_void, i32, i32, *mut *mut c_void) -> i32;

unsafe fn render_devices() -> Vec<*mut c_void> {
    if unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED) } < 0 {
        return Vec::new();
    }

    let mut enumerator_ptr = std::ptr::null_mut();
    // SAFETY : création de l'énumérateur MMDevice.
    let hr = CoCreateInstance(
        &CLSID_MMDEVICE_ENUMERATOR,
        std::ptr::null_mut(),
        CLSCTX_ALL,
        &IID_IMMDEVICE_ENUMERATOR,
        &mut enumerator_ptr,
    );
    if hr < 0 || enumerator_ptr.is_null() {
        return Vec::new();
    }
    let enumerator = enumerator_ptr;

    let mut devices_ptr = std::ptr::null_mut();
    // SAFETY : énumération des périphériques de rendu actifs.
    let enum_fn: EnumAudioEndpointsFn = unsafe { std::mem::transmute(slot(enumerator, 3)) };
    let hr = unsafe { enum_fn(enumerator, ERENDER, DEVICE_STATE_ACTIVE, &mut devices_ptr) };
    if hr < 0 || devices_ptr.is_null() {
        return Vec::new();
    }

    let mut n: u32 = 0;
    // SAFETY : nombre de périphériques.
    let count_fn: GetCountU32Fn = unsafe { std::mem::transmute(slot(devices_ptr, 3)) };
    unsafe { count_fn(devices_ptr, &mut n) };
    if n == 0 {
        return Vec::new();
    }

    let mut devices = Vec::new();
    for i in 0..n {
        let mut device_ptr = std::ptr::null_mut();
        // SAFETY : accès au périphérique i.
        let item_fn: ItemFn = unsafe { std::mem::transmute(slot(devices_ptr, 4)) };
        if unsafe { item_fn(devices_ptr, i, &mut device_ptr) } >= 0 && !device_ptr.is_null() {
            devices.push(device_ptr);
        }
    }
    devices
}

/// Périphérique de rendu par défaut (`eRender` + rôle console), s'il existe.
unsafe fn default_render_device() -> *mut c_void {
    let mut enumerator_ptr = std::ptr::null_mut();
    // SAFETY : création de l'énumérateur MMDevice.
    let hr = CoCreateInstance(
        &CLSID_MMDEVICE_ENUMERATOR,
        std::ptr::null_mut(),
        CLSCTX_ALL,
        &IID_IMMDEVICE_ENUMERATOR,
        &mut enumerator_ptr,
    );
    if hr < 0 || enumerator_ptr.is_null() {
        return std::ptr::null_mut();
    }
    let mut device_ptr = std::ptr::null_mut();
    // SAFETY : périphérique de rendu par défaut.
    let get_default: GetDefaultEndpointFn =
        unsafe { std::mem::transmute(slot(enumerator_ptr, 4)) };
    unsafe { get_default(enumerator_ptr, ERENDER, ECONSOLE, &mut device_ptr) };
    device_ptr
}