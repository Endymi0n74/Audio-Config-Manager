// Point d'entrée. Comme l'application originale, tout le travail audio est
// délégué au module PowerShell AudioDeviceCmdlets via un script embarqué
// (voir src/ps.rs) — Windows uniquement, comme l'app elle-même.

// Lié en tant qu'application GUI (subsystem Windows) même en debug : aucune
// fenêtre de terminal ne s'ouvre à côté de l'interface. Les avertissements
// sont conservés dans `%APPDATA%\Audio Config Manager\debug.log` (voir
// src/logging.rs).
#![windows_subsystem = "windows"]

mod app_routing;
mod appearance;
mod commands;
mod logging;
mod profiles;
mod ps;
mod route_cli;
mod settings;

use tauri::{Emitter, Manager};

fn main() {
    // Invocation en ligne de commande : `Audio Config Manager.exe route …`
    // exécute la CLI de débogage (sans démarrer l'interface). L'exe étant
    // lié en subsystem GUI, une console est rattachée avant toute sortie
    // (voir route_cli.rs).
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("route") {
        route_cli::attach_console();
        std::process::exit(route_cli::run(&args[1..]));
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::settings,
            commands::update_settings,
            commands::overview,
            commands::save_profile,
            commands::preview_profile,
            commands::restore_profile,
            commands::delete_profile,
            commands::import_profile,
            commands::profiles_folder,
            commands::choose_profiles_folder,
            commands::open_profiles_folder,
            commands::install_audio_module,
            commands::app_sessions,
            commands::set_app_route,
            appearance::system_accent_color,
            appearance::backdrop_enabled,
        ])
        .setup(|app| {
            // Intégration Windows 11 : fond Mica + couleur d'accent système.
            // (Sur RDP, ou si les effets de transparence sont désactivés,
            // l'interface reste sur ses surfaces opaques thématisées.)
            if let Some(window) = app.get_webview_window("main") {
                let mica = appearance::apply_mica(&window);
                app.manage(appearance::BackdropState(mica));
            }

            let handle = app.handle().clone();

            let Ok(current_settings) = settings::load() else {
                // Sans dossier de configuration, l'app s'ouvre quand même ;
                // les commandes afficheront « Dossier de configuration introuvable ».
                return Ok(());
            };
            let Ok(config_dir) = settings::config_dir() else { return Ok(()) };
            let Ok(script) = ps::ensure_script(&config_dir) else { return Ok(()) };

            // Dossier de profils : s'assure qu'il existe.
            let _ = std::fs::create_dir_all(&current_settings.profiles_folder);

            // Sauvegarde automatique au démarrage (autoSaveOnStart).
            if current_settings.auto_save_on_start {
                let settings = current_settings.clone();
                let script = script.clone();
                std::thread::spawn(move || {
                    let _ = commands::create_auto_backup(&settings, &script);
                });
            }

            // Veille sur les périphériques (watchDevices) : abonnement COM
            // IMMNotificationClient (voir app_routing/watch.rs) — détection
            // instantanée, zéro processus PowerShell en arrière-plan (l'ancien
            // sondage « overview » toutes les 10 s est supprimé).
            // Le thread est abonné quelle que soit l'option au démarrage :
            // chaque événement reverifie settings.watch_devices, donc
            // activation/désactivation prennent effet immédiatement.
            if let Err(e) = app_routing::spawn_default_device_watch(
                move |(playback, recording)| {
                    let _ = (playback, recording);
                    let Ok(current) = settings::load() else { return };
                    if !current.watch_devices {
                        return;
                    }
                    let Ok(config_dir) = settings::config_dir() else { return };
                    let Ok(script) = ps::ensure_script(&config_dir) else { return };
                    if let Ok(path) = commands::create_auto_backup(&current, &script) {
                        let _ = handle.emit("profiles-changed", ());
                        let _ = handle.emit(
                            "devices-changed",
                            serde_json::json!({ "backup": path }),
                        );
                    }
                },
            ) {
                crate::logging::warn(&format!("watchDevices indisponible : {e}"));
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("erreur au lancement d'Audio Config Manager");
}

/// Test anti-régression : le binaire doit rester lié en mode GUI
/// (subsystem PE = 2), faute de quoi une fenêtre de terminal s'ouvrirait
/// au lancement. On lit l'en-tête PE du binaire courant (`cargo test`
/// recompile la crate avec les mêmes attributs, donc le binaire de test
/// hérite de `#![windows_subsystem = "windows"]`).
#[cfg(all(test, windows))]
mod subsystem_test {
    use std::path::Path;

    /// Lit la valeur `Subsystem` de l'en-tête PE d'un exécutable Windows
    /// (2 = GUI, 3 = Console) en suivant la signature `PE\0\0`.
    fn pe_subsystem(exe: &Path) -> u16 {
        let bytes = std::fs::read(exe).expect("lecture de l'exécutable impossible");
        assert!(bytes.len() > 0x40, "fichier trop court pour un exécutable PE");
        let pe_off =
            u32::from_le_bytes([bytes[0x3c], bytes[0x3d], bytes[0x3e], bytes[0x3f]]) as usize;
        assert_eq!(&bytes[pe_off..pe_off + 4], b"PE\0\0", "signature PE absente");
        u16::from_le_bytes([bytes[pe_off + 0x5c], bytes[pe_off + 0x5d]])
    }

    #[test]
    fn main_binary_is_linked_as_gui_subsystem() {
        let exe = std::env::current_exe().expect("chemin du binaire courant introuvable");
        assert_eq!(
            pe_subsystem(&exe),
            2,
            "{} doit être lié en GUI (subsystem 2), pas en console (3) — vérifier \
             l'attribut #![windows_subsystem = \"windows\"] dans main.rs",
            exe.display()
        );
    }
}

/// Test d'intégration anti-régression : le frontend doit être EMBARQUÉ dans
/// le binaire. Dans Tauri 2, l'embarquement est piloté par le feature
/// `custom-protocol` (activé par défaut dans Cargo.toml) : sans lui, le
/// binaire tente de charger le serveur de dev (localhost:1420) → écran
/// `ERR_CONNECTION_REFUSED` au lancement. Ce test échoue si le feature
/// disparaît ou si le dossier frontend (`../src`) est absent/vide.
#[cfg(test)]
mod embed_test {
    #[test]
    fn frontend_is_embedded_via_custom_protocol() {
        // `tauri::is_dev()` vaut vrai si le feature `custom-protocol` est absent.
        assert!(
            !tauri::is_dev(),
            "le feature tauri `custom-protocol` doit être activé (Cargo.toml) : sans lui, \
             le frontend n'est pas embarqué et l'app essaie de charger localhost:1420 \
             (ERR_CONNECTION_REFUSED)"
        );

        // Le contexte généré doit embarquer le frontend (frontendDist = ../src).
        let context: tauri::Context<tauri::Wry> = tauri::generate_context!();
        let key = tauri::utils::assets::AssetKey::from("index.html");
        assert!(
            context.assets.get(&key).is_some(),
            "index.html doit être présent dans les assets embarqués (frontendDist = ../src)"
        );

        // Sanity : le frontend complet contient plusieurs fichiers.
        let count = context.assets.iter().count();
        assert!(
            count >= 3,
            "trop peu d'assets embarqués ({count}) : le frontend semble incomplet"
        );
    }
}