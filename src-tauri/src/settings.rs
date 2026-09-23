//! Paramètres de l'application, persistés dans
//! `%APPDATA%\Audio Config Manager\settings.json` — mêmes clés que
//! l'application originale : profilesFolder, backupBeforeRestore,
//! autoSaveOnStart, keepVersions, watchDevices.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const APP_DATA_FOLDER: &str = "Audio Config Manager";
pub const SETTINGS_FILE: &str = "settings.json";
/// Nom du dossier de profils par défaut, créé dans Documents.
pub const DEFAULT_PROFILES_FOLDER_NAME: &str = "Audio Profiles";
/// Libellés d'erreur partagés (même texte dans tous les modules).
pub const ERR_CONFIG_DIR: &str = "Dossier de configuration introuvable";
pub const ERR_PROFILE_PATH: &str = "Chemin de profil refusé";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Dossier où sont stockés les profils audio (.json).
    pub profiles_folder: String,
    /// Créer une sauvegarde horodatée avant chaque restauration.
    pub backup_before_restore: bool,
    /// Sauvegarder automatiquement la configuration au démarrage.
    pub auto_save_on_start: bool,
    /// Nombre de versions horodatées à conserver (0 = tout garder).
    pub keep_versions: u32,
    /// Surveiller les changements de périphériques et sauvegarder.
    pub watch_devices: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            profiles_folder: default_profiles_folder().unwrap_or_else(|| {
                "Audio Profiles".to_string()
            }),
            backup_before_restore: true,
            auto_save_on_start: false,
            keep_versions: 10,
            watch_devices: false,
        }
    }
}

/// `%APPDATA%\Audio Config Manager`
pub fn config_dir() -> Result<PathBuf, String> {
    match std::env::var_os("APPDATA") {
        Some(appdata) => Ok(PathBuf::from(appdata).join(APP_DATA_FOLDER)),
        None => Err(ERR_CONFIG_DIR.to_string()),
    }
}

/// Dossier de profils par défaut : Documents\Audio Profiles
/// (repli sur le dossier utilisateur si Documents est absent).
fn default_profiles_folder() -> Option<String> {
    let user_profile = std::env::var_os("USERPROFILE")?;
    let user_dir = PathBuf::from(&user_profile);
    let documents = user_dir.join("Documents");
    let base = if documents.is_dir() { documents } else { user_dir };
    Some(base.join(DEFAULT_PROFILES_FOLDER_NAME).to_string_lossy().to_string())
}

/// Charge les paramètres, en créant le fichier avec les valeurs par défaut
/// s'il n'existe pas (ou est illisible).
pub fn load() -> Result<Settings, String> {
    let dir = config_dir()?;
    let path = dir.join(SETTINGS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|e| {
            format!("Paramètres illisibles ({}): {e}", path.display())
        }),
        Err(_) => {
            let settings = Settings::default();
            save(&settings)?;
            Ok(settings)
        }
    }
}

/// Persiste les paramètres.
pub fn save(settings: &Settings) -> Result<(), String> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{ERR_CONFIG_DIR} : {e}"))?;
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Sérialisation des paramètres impossible : {e}"))?;
    std::fs::write(dir.join(SETTINGS_FILE), json)
        .map_err(|e| format!("Écriture des paramètres impossible : {e}"))
}

/// Valide et normalise des paramètres reçus de l'interface.
pub fn validate(mut settings: Settings) -> Result<Settings, String> {
    let folder = settings.profiles_folder.trim().to_string();
    if folder.is_empty() {
        return Err(ERR_PROFILE_PATH.to_string());
    }
    settings.profiles_folder = folder;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_usable() {
        let settings = Settings::default();
        assert!(!settings.profiles_folder.is_empty());
        assert!(settings.backup_before_restore);
        assert_eq!(settings.keep_versions, 10);
    }

    #[test]
    fn round_trip_via_temp_dir() {
        // On ne peut pas injecter APPDATA, donc on teste la sérialisation
        // JSON et la relecture directement.
        let settings = Settings {
            profiles_folder: "C:\\Profils".into(),
            backup_before_restore: false,
            auto_save_on_start: true,
            keep_versions: 3,
            watch_devices: true,
        };
        let json = serde_json::to_string_pretty(&settings).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, settings);
    }

    #[test]
    fn json_uses_camel_case_keys() {
        let settings = Settings::default();
        let json = serde_json::to_value(&settings).unwrap();
        assert!(json.get("profilesFolder").is_some());
        assert!(json.get("backupBeforeRestore").is_some());
        assert!(json.get("autoSaveOnStart").is_some());
        assert!(json.get("keepVersions").is_some());
        assert!(json.get("watchDevices").is_some());
    }

    #[test]
    fn validate_rejects_empty_folder() {
        let settings = Settings {
            profiles_folder: "  ".into(),
            ..Settings::default()
        };
        assert!(validate(settings).is_err());
    }

    #[test]
    fn validate_trims_folder() {
        let settings = Settings {
            profiles_folder: "  C:\\Profils  ".into(),
            ..Settings::default()
        };
        let cleaned = validate(settings).unwrap();
        assert_eq!(cleaned.profiles_folder, "C:\\Profils");
    }

    #[test]
    fn config_dir_under_appdata() {
        let dir = config_dir().unwrap();
        assert!(dir.ends_with("Audio Config Manager"));
    }
}