//! Intégration visuelle Windows 11 : fond Mica et couleur d'accent système.
//!
//! - `apply_mica` active le backdrop DWM « Mica » via la crate
//!   `window-vibrancy` (attribut `DWMWA_SYSTEMBACKDROP_TYPE`), puis rend le
//!   fond du WebView transparent pour laisser transparaître l'effet. Refusé
//!   en session distante (RDP) ou quand l'utilisateur a désactivé les effets
//!   de transparence (l'interface reste alors sur les surfaces opaques).
//! - `system_accent_color` lit l'accent Windows (HKCU, DWM) et le renvoie
//!   au format `#rrggbb` pour piloter les variables CSS `--accent*`.
//!
//! Seuls `user32` (détection RDP) et `advapi32` (registre) sont appelés en
//! FFI direct ; le rendu du backdrop est délégué à `window-vibrancy`.

use std::os::windows::ffi::OsStrExt;

/// État exposé à l'interface : le backdrop Mica a-t-il pu être appliqué ?
pub struct BackdropState(pub bool);

#[cfg(windows)]
mod ffi {
    #[link(name = "user32")]
    extern "system" {
        pub fn GetSystemMetrics(n_index: i32) -> i32;
    }

    #[link(name = "advapi32")]
    extern "system" {
        pub fn RegOpenKeyExW(
            h_key: usize,
            lp_sub_key: *const u16,
            ul_options: u32,
            sam_desired: u32,
            phk_result: *mut usize,
        ) -> i32;
        pub fn RegQueryValueExW(
            h_key: usize,
            lp_value_name: *const u16,
            lp_reserved: *mut u32,
            lp_type: *mut u32,
            lp_data: *mut u8,
            lpcb_data: *mut u32,
        ) -> i32;
        pub fn RegCloseKey(h_key: usize) -> i32;
    }
}

const SM_REMOTESESSION: i32 = 0x1000;
const HKEY_CURRENT_USER: usize = 0x8000_0001;
const KEY_READ: u32 = 0x0002_0019;
const REG_DWORD: u32 = 4;

/// Active le fond Mica si l'environnement le permet. Renvoie `true` si le
/// WebView a été rendu transparent (l'interface ajoute alors une classe
/// `backdrop` pour adapter ses surfaces).
#[cfg(windows)]
pub fn apply_mica(window: &tauri::WebviewWindow) -> bool {
    // Session distante : les effets DWM sont désactivés, inutile d'essayer.
    if unsafe { ffi::GetSystemMetrics(SM_REMOTESESSION) } != 0 {
        return false;
    }
    // Effets de transparence désactivés dans Windows → pas de Mica non plus.
    if reg_dword(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "EnableTransparency",
    ) == Some(0)
    {
        return false;
    }

    // Mica via window-vibrancy (couleur de thème automatique) puis fond du
    // WebView transparent pour laisser transparaître le backdrop DWM.
    if window_vibrancy::apply_mica(window, None).is_err() {
        return false;
    }
    window
        .set_background_color(Some(tauri::utils::config::Color(0, 0, 0, 0)))
        .is_ok()
}

#[cfg(not(windows))]
pub fn apply_mica(_window: &tauri::WebviewWindow) -> bool {
    false
}

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(Some(0))
        .collect()
}

/// Lit un DWORD d'une valeur de registre sous HKCU.
#[cfg(windows)]
fn reg_dword(subkey: &str, name: &str) -> Option<u32> {
    let subkey_wide = wide(subkey);
    let name_wide = wide(name);
    let mut key: usize = 0;
    unsafe {
        if ffi::RegOpenKeyExW(HKEY_CURRENT_USER, subkey_wide.as_ptr(), 0, KEY_READ, &mut key) != 0
        {
            return None;
        }
        let mut value: u32 = 0;
        let mut size: u32 = std::mem::size_of::<u32>() as u32;
        let mut kind: u32 = 0;
        let status = ffi::RegQueryValueExW(
            key,
            name_wide.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            &mut value as *mut u32 as *mut u8,
            &mut size,
        );
        ffi::RegCloseKey(key);
        if status != 0 || kind != REG_DWORD {
            return None;
        }
        Some(value)
    }
}

#[cfg(not(windows))]
fn reg_dword(_subkey: &str, _name: &str) -> Option<u32> {
    None
}

/// Couleur d'accent système sous forme `#rrggbb`, lue depuis
/// `HKCU\Software\Microsoft\Windows\DWM\AccentColor` (repli :
/// `Personalize\SystemAccentColor`). Le DWORD est stocké en mémoire sur les
/// octets R,G,B,A — `r = value & 0xFF`, `b = (value >> 16) & 0xFF`.
#[tauri::command]
pub fn system_accent_color() -> Option<String> {
    let value = reg_dword(r"Software\Microsoft\Windows\DWM", "AccentColor").or_else(|| {
        reg_dword(
            r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            "SystemAccentColor",
        )
    })?;
    let r = value & 0xFF;
    let g = (value >> 8) & 0xFF;
    let b = (value >> 16) & 0xFF;
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

/// Le fond Mica a-t-il pu être activé ?
#[tauri::command]
pub fn backdrop_enabled(state: tauri::State<'_, BackdropState>) -> bool {
    state.0
}
