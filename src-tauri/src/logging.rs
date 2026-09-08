//! Journalisation « sans console » : le binaire principal est lié en
//! subsystem Windows (`#![windows_subsystem = "windows"]`), donc aucune
//! fenêtre de terminal n'est ouverte — ni en release, ni en debug. Les
//! `eprintln!` d'un tel programme sont alors perdus.
//!
//! `logging::warn` conserve ces avertissements ailleurs : une ligne
//! horodatée est écrite dans `%APPDATA%\Audio Config Manager\debug.log`,
//! et re-émise sur stderr quand une console existe (par ex. la CLI de
//! débogage `route.exe`, qui garde sa console).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

/// Nom du dossier de configuration (identique à `settings::APP_DATA_FOLDER`).
const LOG_FOLDER: &str = "Audio Config Manager";
/// Nom du fichier journal.
const LOG_FILE: &str = "debug.log";

/// `%APPDATA%\Audio Config Manager\debug.log`, ou `debug.log` dans le
/// dossier courant si `APPDATA` est absent.
fn log_path() -> PathBuf {
    match std::env::var_os("APPDATA") {
        Some(appdata) => PathBuf::from(appdata).join(LOG_FOLDER).join(LOG_FILE),
        None => PathBuf::from(LOG_FILE),
    }
}

/// Journalise un avertissement : écrit une ligne horodatée dans le fichier
/// journal et l'écho sur stderr (visible s'il existe une console). Les
/// erreurs de journalisation elles-mêmes sont silencieuses — ne jamais
/// interrompre l'application pour un simple log.
pub fn warn(message: &str) {
    eprintln!("audio-config-manager: {message}");
    let line = format!(
        "{} WARN {message}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(line.as_bytes());
    }
}