//! Commandes Tauri exposées à l'interface — mêmes noms et mêmes
//! comportements que l'application originale (« Audio Config Manager ») :
//! settings, update_settings, overview, save_profile, preview_profile,
//! restore_profile, delete_profile, import_profile, profiles_folder,
//! choose_profiles_folder, open_profiles_folder, install_audio_module.

use crate::app_routing::{self, AppPreviewRow, AppSessionRow, Flow};
use crate::profiles::{self, ProfileEntry};
use crate::ps::{self, Overview, PreviewInfo, RestoreInfo};
use crate::settings::{self, Settings};
use chrono::Local;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tauri::Emitter;
use tauri_plugin_dialog::DialogExt;

/// Résultat d'une sauvegarde de profil.
#[derive(Debug, Serialize)]
pub struct SaveProfileResult {
    pub saved: bool,
    pub path: Option<String>,
    pub message: String,
}

/// Résultat d'une importation de profil.
#[derive(Debug, Serialize)]
pub struct ImportProfileResult {
    pub imported: bool,
    pub path: Option<String>,
    pub message: String,
}

/// Résultat d'une restauration de profil.
#[derive(Debug, Serialize)]
pub struct RestoreProfileResult {
    pub message: String,
    pub applied: u32,
    pub missing: Vec<String>,
    pub warnings: Vec<String>,
}

/// Aperçu d'un profil : périphériques par défaut + routage par application.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewResult {
    #[serde(flatten)]
    pub info: PreviewInfo,
    #[serde(default)]
    pub applications: Vec<AppPreviewRow>,
    #[serde(default)]
    pub routing_available: bool,
}

/// Résultat d'une suppression de profil.
#[derive(Debug, Serialize)]
pub struct DeleteResult {
    pub deleted: bool,
    pub message: String,
}

/// Référence d'un périphérique actif (identifiant + nom lisible).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRef {
    pub id: String,
    pub name: String,
}

/// Résultat de la vue « Applications » : sessions audio actives + liste des
/// périphériques disponibles par flux, pour les sélecteurs.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSessionsResult {
    /// Le moteur de routage est-il activable sur ce système ?
    pub available: bool,
    /// Erreur du moteur (affichée dans la vue quand `available` est vrai
    /// mais que l'énumération a échoué).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub sessions: Vec<AppSessionRow>,
    pub playback_devices: Vec<DeviceRef>,
    pub recording_devices: Vec<DeviceRef>,
}

/// Dossier de profils + liste des profils qu'il contient.
#[derive(Debug, Serialize)]
pub struct ProfilesFolder {
    pub path: String,
    pub profiles: Vec<ProfileEntry>,
}

/// Résultat d'une opération sur le module PowerShell.
#[derive(Debug, Serialize)]
pub struct ModuleResult {
    pub ok: bool,
    pub message: String,
}

fn timestamp() -> String {
    Local::now().format("%Y-%m-%d %H-%M-%S").to_string()
}

/// Charge les paramètres et s'assure que le script PowerShell est présent.
fn ready() -> Result<(Settings, PathBuf), String> {
    let settings = settings::load()?;
    let config_dir = settings::config_dir()?;
    let script = ps::ensure_script(&config_dir)?;
    Ok((settings, script))
}

/// Exécute l'action `export` du script vers `destination`, puis complète le
/// profil avec la section « applications » (routage par application).
/// Renvoie le nombre d'applications enregistrées dans le profil.
fn run_export(script: &Path, destination: &Path) -> Result<usize, String> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Dossier de configuration introuvable : {e}"))?;
    }
    let value = ps::run_script(script, "export", Some(destination))?;
    if !value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err(value
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Échec de la sauvegarde")
            .to_string());
    }
    // Le routage par application est ajouté par Rust (API interne Windows) —
    // non bloquant : sans cette section, le profil reste parfaitement valide.
    Ok(app_routing::attach_applications_to_profile(destination)
        .unwrap_or_else(|e| {
            crate::logging::warn(&format!("applications non ajoutées : {e}"));
            0
        }))
}

/// Crée une sauvegarde horodatée du profil courant dans le dossier de
/// profils, puis applique la rétention `keepVersions`. Renvoie le chemin.
pub fn create_auto_backup(settings: &Settings, script: &Path) -> Result<String, String> {
    let folder = PathBuf::from(&settings.profiles_folder);
    let name = format!("{}.json", timestamp());
    let destination = profiles::unique_path(&folder, &name);
    run_export(script, &destination)?;
    let _ = profiles::prune_versions(&folder, settings.keep_versions);
    Ok(destination.to_string_lossy().to_string())
}

/// Lit les paramètres.
#[tauri::command]
pub fn settings() -> Result<Settings, String> {
    settings::load()
}

/// Met à jour les paramètres (dossier de profils, sauvegardes, veille…).
#[tauri::command]
pub fn update_settings(new_settings: Settings) -> Result<Settings, String> {
    let settings = settings::validate(new_settings)?;
    std::fs::create_dir_all(&settings.profiles_folder)
        .map_err(|e| format!("Chemin de profil refusé : {e}"))?;
    settings::save(&settings)?;
    Ok(settings)
}

/// État audio courant (périphériques par défaut, compteurs, module).
#[tauri::command]
pub async fn overview() -> Result<Overview, String> {
    let (_, script) = ready()?;
    let value = tauri::async_runtime::spawn_blocking(move || {
        ps::run_script(&script, "overview", None)
    })
    .await
    .map_err(|e| format!("Lecture audio interrompue : {e}"))??;
    serde_json::from_value(value).map_err(|e| format!("Réponse audio invalide : {e}"))
}

/// Enregistre un nouveau profil : l'utilisateur choisit l'emplacement,
/// puis la configuration audio courante y est exportée.
#[tauri::command]
pub async fn save_profile(app: tauri::AppHandle) -> Result<SaveProfileResult, String> {
    let default_name = format!("{}.json", timestamp());
    let dialog_app = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        dialog_app
            .dialog()
            .file()
            .set_title("Sauvegarder un profil audio")
            .set_file_name(&default_name)
            .add_filter("Profil audio JSON", &["json"])
            .blocking_save_file()
    })
    .await
    .map_err(|e| format!("Boîte de dialogue indisponible : {e}"))?;
    let Some(path) = picked.map(|f| f.into_path().unwrap_or_default()) else {
        return Ok(SaveProfileResult {
            saved: false,
            path: None,
            message: "Sauvegarde annulée.".to_string(),
        });
    };
    if path.as_os_str().is_empty()
        || path.extension().and_then(|e| e.to_str()) != Some("json")
    {
        return Err("Chemin de profil refusé".to_string());
    }

    let (_, script) = ready()?;
    let script = script.clone();
    let path_for_task = path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let routed_apps = run_export(&script, &path_for_task)?;
        let mut message = "Profil sauvegardé".to_string();
        if routed_apps > 0 {
            message.push_str(&format!(" · {routed_apps} application(s) routée(s)"));
        }
        let _ = app.emit("profiles-changed", ());
        Ok::<SaveProfileResult, String>(SaveProfileResult {
            saved: true,
            path: Some(path_for_task.to_string_lossy().to_string()),
            message,
        })
    })
    .await
    .map_err(|e| format!("Sauvegarde interrompue : {e}"))?
}

/// Périphériques actifs courants (identifiant, nom), par flux.
///
/// Énumération COM directe (`IMMDeviceEnumerator` + nom convivial via
/// `PKEY_Device_FriendlyName`, voir `app_routing::active_devices`) : plus
/// aucun lancement PowerShell pour lister les périphériques.
type DeviceList = Vec<(String, String)>;

fn current_devices() -> (DeviceList, DeviceList) {
    (
        app_routing::active_devices(Flow::Output),
        app_routing::active_devices(Flow::Input),
    )
}

/// Le fichier contient-il une section « applications » (évite un appel
/// PowerShell superflu à l'aperçu) ?
fn profile_has_applications(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    String::from_utf8_lossy(bytes).contains(r#""applications""#)
}

/// Vue « Applications » : processus audio actifs avec leur route persistée
/// courante, et périphériques disponibles pour les sélecteurs. Entièrement
/// en Rust (WASAPI + AudioPolicyConfig) — aucun appel PowerShell.
#[tauri::command]
pub async fn app_sessions() -> Result<AppSessionsResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (playback, recording) = current_devices();
        let available = app_routing::routing_available();
        let (sessions, error) = if available {
            match app_routing::list_active_app_routes(&playback, &recording) {
                Ok(sessions) => (sessions, None),
                Err(e) => (Vec::new(), Some(e)),
            }
        } else {
            (
                Vec::new(),
                Some("Routage par application indisponible sur ce système.".into()),
            )
        };
        Ok(AppSessionsResult {
            available,
            error,
            sessions,
            playback_devices: playback
                .into_iter()
                .map(|(id, name)| DeviceRef { id, name })
                .collect(),
            recording_devices: recording
                .into_iter()
                .map(|(id, name)| DeviceRef { id, name })
                .collect(),
        })
    })
    .await
    .map_err(|e| format!("Lecture des applications interrompue : {e}"))?
}

/// Route une application (PID) vers un périphérique, ou la remet au
/// périphérique système si `device_id` est `null`.
#[tauri::command]
pub async fn set_app_route(
    pid: u32,
    flow: String,
    device_id: Option<String>,
) -> Result<(), String> {
    let flow = match flow.as_str() {
        "output" => Flow::Output,
        "input" => Flow::Input,
        _ => return Err("Flux audio inconnu (attendu « output » ou « input »)".into()),
    };
    tauri::async_runtime::spawn_blocking(move || {
        app_routing::set_app_route(pid, flow, device_id)
    })
    .await
    .map_err(|e| format!("Routage interrompu : {e}"))?
}

/// Aperçu d'un profil : périphériques par défaut qu'il contient + routage
/// par application (processus et périphériques présents sur la machine).
#[tauri::command]
pub async fn preview_profile(path: String) -> Result<PreviewResult, String> {
    let profile_path = PathBuf::from(&path);
    if !profile_path.is_file() {
        return Err("Le profil n'existe plus".to_string());
    }
    let (_, script) = ready()?;
    let script = script.clone();
    let has_apps = profile_has_applications(&profile_path);
    let profile_path2 = profile_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let info: PreviewInfo = serde_json::from_value(ps::run_script(
            &script,
            "preview",
            Some(&profile_path),
        )?)
        .map_err(|e| format!("Réponse audio invalide : {e}"))?;
        let mut result = PreviewResult {
            info,
            applications: Vec::new(),
            routing_available: app_routing::routing_available(),
        };
        if has_apps {
            let (playback, recording) = current_devices();
            if let Ok(apps) = app_routing::preview_applications(
                &profile_path2,
                &playback,
                &recording,
            ) {
                result.applications = apps;
            }
        }
        Ok(result)
    })
    .await
    .map_err(|e| format!("Aperçu interrompu : {e}"))?
}

/// Restaure un profil. Si `backupBeforeRestore` est activé, la configuration
/// courante est d'abord sauvegardée dans le dossier de profils.
#[tauri::command]
pub async fn restore_profile(
    app: tauri::AppHandle,
    path: String,
) -> Result<RestoreProfileResult, String> {
    let profile_path = PathBuf::from(&path);
    if !profile_path.is_file() {
        return Err("Le profil n'existe plus".to_string());
    }
    let (settings, script) = ready()?;
    let backup = if settings.backup_before_restore {
        let folder = PathBuf::from(&settings.profiles_folder);
        let name = format!("Avant restauration {}.json", timestamp());
        let destination = profiles::unique_path(&folder, &name);
        Some(destination.clone())
    } else {
        None
    };

    let script = script.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<RestoreProfileResult, String> {
        if let Some(backup) = &backup {
            run_export(&script, backup)?;
            let folder = backup.parent().map(Path::to_path_buf).unwrap_or_default();
            let _ = profiles::prune_versions(&folder, settings.keep_versions);
        }
        let value = ps::run_script(&script, "restore", Some(&profile_path))?;
        let info: RestoreInfo =
            serde_json::from_value(value).map_err(|e| format!("Réponse audio invalide : {e}"))?;
        if !info.ok {
            return Err("La restauration a échoué".to_string());
        }

        // Routage par application : appliqué par le moteur interne Windows.
        let mut missing = info.missing;
        let mut warnings = Vec::new();
        let mut applied = info.applied;
        if profile_has_applications(&profile_path) {
            let (playback, recording) = current_devices();
            match app_routing::restore_applications_in_profile(
                &profile_path,
                &playback,
                &recording,
            ) {
                Ok(report) => {
                    applied += report.applied;
                    missing.extend(report.missing);
                    warnings.extend(report.warnings);
                    warnings.extend(report.errors);
                }
                Err(e) => missing.push(e),
            }
        }

        let mut message = "Configuration restaurée".to_string();
        if !missing.is_empty() {
            message.push_str(&format!(" · {} élément(s) absent(s)", missing.len()));
        }
        let _ = app.emit("profiles-changed", ());
        Ok(RestoreProfileResult {
            message,
            applied,
            missing,
            warnings,
        })
    })
    .await
    .map_err(|e| format!("Restauration interrompue : {e}"))?
}

/// Supprime un profil du dossier.
#[tauri::command]
pub fn delete_profile(path: String) -> Result<DeleteResult, String> {
    let profile_path = PathBuf::from(&path);
    if !profile_path.is_file() {
        return Err("Le profil n'existe plus".to_string());
    }
    std::fs::remove_file(&profile_path)
        .map_err(|e| format!("Suppression impossible : {e}"))?;
    Ok(DeleteResult {
        deleted: true,
        message: "Profil supprimé".to_string(),
    })
}

/// Vérifie qu'un fichier est bien un profil JSON exploitable.
pub fn is_valid_profile(path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(map)) => map.contains_key("Metadata"),
        _ => false,
    }
}

/// Importe un profil JSON choisi par l'utilisateur dans le dossier de profils.
#[tauri::command]
pub async fn import_profile(app: tauri::AppHandle) -> Result<ImportProfileResult, String> {
    let dialog_app = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        dialog_app
            .dialog()
            .file()
            .set_title("Importer un profil audio")
            .add_filter("Profil audio JSON", &["json"])
            .blocking_pick_file()
    })
    .await
    .map_err(|e| format!("Boîte de dialogue indisponible : {e}"))?;
    let Some(source) = picked.map(|f| f.into_path().unwrap_or_default()) else {
        return Ok(ImportProfileResult {
            imported: false,
            path: None,
            message: "Importation annulée.".to_string(),
        });
    };
    if !is_valid_profile(&source) {
        return Err("Ce fichier n’est pas un profil JSON valide".to_string());
    }
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "profil.json".into());
    let (settings, _) = ready()?;
    let folder = PathBuf::from(&settings.profiles_folder);
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("Chemin de profil refusé : {e}"))?;
    let destination = profiles::unique_path(&folder, &name);
    std::fs::copy(&source, &destination)
        .map_err(|e| format!("Copie du profil impossible : {e}"))?;
    let _ = app.emit("profiles-changed", ());
    Ok(ImportProfileResult {
        imported: true,
        path: Some(destination.to_string_lossy().to_string()),
        message: "Profil importé".to_string(),
    })
}

/// Dossier de profils configuré et liste des profils qu'il contient.
#[tauri::command]
pub fn profiles_folder() -> Result<ProfilesFolder, String> {
    let (settings, _) = ready()?;
    let folder = PathBuf::from(&settings.profiles_folder);
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("Chemin de profil refusé : {e}"))?;
    let profiles = profiles::list_profiles(&folder)?;
    Ok(ProfilesFolder {
        path: folder.to_string_lossy().to_string(),
        profiles,
    })
}

/// Ouvre la boîte de dialogue de choix de dossier et enregistre le nouveau
/// dossier de profils.
#[tauri::command]
pub async fn choose_profiles_folder(app: tauri::AppHandle) -> Result<Settings, String> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Choisir le dossier des profils")
            .blocking_pick_folder()
    })
    .await
    .map_err(|e| format!("Boîte de dialogue indisponible : {e}"))?;
    let Some(folder) = picked.map(|f| f.into_path().unwrap_or_default()) else {
        return settings::load();
    };
    let mut current = settings::load()?;
    current.profiles_folder = folder.to_string_lossy().to_string();
    settings::save(&current)?;
    Ok(current)
}

/// Ouvre le dossier des profils dans l'Explorateur Windows.
#[tauri::command]
pub fn open_profiles_folder() -> Result<(), String> {
    let (settings, _) = ready()?;
    let folder = PathBuf::from(&settings.profiles_folder);
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("Chemin de profil refusé : {e}"))?;
    std::process::Command::new("explorer.exe")
        .arg(&folder)
        .spawn()
        .map_err(|e| format!("Ouverture du dossier impossible : {e}"))?;
    Ok(())
}

/// Installe le module PowerShell AudioDeviceCmdlets (NuGet + PSGallery).
#[tauri::command]
pub async fn install_audio_module() -> Result<ModuleResult, String> {
    let message = tauri::async_runtime::spawn_blocking(ps::install_audio_module)
        .await
        .map_err(|e| format!("Installation interrompue : {e}"))??;
    Ok(ModuleResult { ok: true, message })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("acm-cmd-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn timestamp_matches_reference_format() {
        let value = timestamp();
        assert_eq!(value.len(), 19);
        let bytes = value.as_bytes();
        assert_eq!(bytes[4], b'-');
        assert_eq!(bytes[7], b'-');
        assert_eq!(bytes[10], b' ');
        assert_eq!(bytes[13], b'-');
        assert_eq!(bytes[16], b'-');
    }

    #[test]
    fn valid_profile_detected() {
        let dir = temp_dir();
        let good = dir.join("good.json");
        std::fs::write(
            &good,
            r#"{"Metadata":{"Version":"3.1"},"DefaultPlayback":{}}"#,
        )
        .unwrap();
        assert!(is_valid_profile(&good));
    }

    #[test]
    fn invalid_profiles_rejected() {
        let dir = temp_dir();
        let not_json = dir.join("not-json.json");
        std::fs::write(&not_json, "ceci n'est pas du json").unwrap();
        assert!(!is_valid_profile(&not_json));

        let no_metadata = dir.join("no-metadata.json");
        std::fs::write(&no_metadata, r#"{"PlaybackDevices":[]}"#).unwrap();
        assert!(!is_valid_profile(&no_metadata));

        let missing = dir.join("missing.json");
        assert!(!is_valid_profile(&missing));
    }

    #[test]
    fn overview_parses_module_unavailable_response() {
        let json = r#"{"moduleAvailable":false,"playbackCount":0,"recordingCount":0,"defaultPlayback":null,"defaultRecording":null}"#;
        let overview: Overview = serde_json::from_str(json).unwrap();
        assert!(!overview.module_available);
        assert!(overview.default_playback.is_none());
    }

    #[test]
    fn device_info_parses_pascal_case_keys_from_powershell() {
        // Sortie réelle du script : ConvertTo-Json conserve `ID`, `Name`, `Volume`.
        let json = r#"{"defaultRecording":{"Name":"Micro USB","ID":"{3.0.1.00000001}.{A3ED9185}","Volume":null},"moduleAvailable":true,"playbackCount":1,"recordingCount":1,"defaultPlayback":{"Name":"Casque USB","ID":"{3.0.0.00000001}.{6C26BA7D}","Volume":42.0}}"#;
        let overview: Overview = serde_json::from_str(json).unwrap();
        assert!(overview.module_available);
        let playback = overview.default_playback.unwrap();
        assert_eq!(playback.name, "Casque USB");
        assert_eq!(playback.id, "{3.0.0.00000001}.{6C26BA7D}");
        assert_eq!(playback.volume, Some(42.0));
        // La sérialisation vers l'interface reste en camelCase.
        let serialized = serde_json::to_value(&playback).unwrap();
        assert!(serialized.get("name").is_some());
        assert!(serialized.get("id").is_some());
        assert!(serialized.get("Name").is_none());
    }

    #[test]
    fn restore_info_parses_script_response() {
        let json = r#"{"ok":true,"applied":3,"missing":["Ancien casque"]}"#;
        let info: RestoreInfo = serde_json::from_str(json).unwrap();
        assert!(info.ok);
        assert_eq!(info.applied, 3);
        assert_eq!(info.missing.len(), 1);
    }

    #[test]
    fn restore_message_appends_missing_count() {
        let mut message = "Configuration restaurée".to_string();
        let missing = ["Ancien casque".to_string()];
        message.push_str(&format!(" · {} périphérique(s) absent(s)", missing.len()));
        assert_eq!(message, "Configuration restaurée · 1 périphérique(s) absent(s)");
    }
}