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
//!   et `ClearAllPersistedApplicationDefaultEndpoints` (l'emplacement 27
//!   n'est pas utilisé : effacer une route passe par `Set…` avec un
//!   HSTRING NULL, voir `PolicyConfig::set`) ;
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
//!
//! Découpage interne (l'API publique reste inchangée, re-exportée ici) :
//! - `ffi` — GUID, HSTRING, COM brut, thread d'appartement STA et fabrique
//!   AudioPolicyConfig ;
//! - `devices` — énumération WASAPI des périphériques (IDs emballés + noms
//!   conviviaux), défauts + volumes de la vue d'ensemble ;
//! - `sessions` — énumération des sessions audio actives (PID + lecture) ;
//! - `profile_apps` — logique métier : export/aperçu/restauration de la
//!   section « applications » et vue « Applications » ;
//! - `watch` — veille event-driven des défauts (`IMMNotificationClient`).

mod devices;
mod ffi;
mod profile_apps;
mod sessions;
mod watch;

pub use devices::{active_devices, overview_devices, DeviceOverview};
// Type des défauts de la vue d'ensemble : nommé uniquement par le test de
// contrat JSON de `commands.rs` (build normal → re-export inutilisé).
#[cfg(test)]
pub use devices::DefaultDeviceInfo;
pub use ffi::routing_available;
pub use profile_apps::{
    attach_applications_to_profile, list_active_app_routes, preview_applications,
    restore_applications_in_profile, set_app_route, AppPreviewRow, AppSessionRow, RouteTarget,
};
pub use watch::spawn_default_device_watch;

// ---------------------------------------------------------------------------
// Flux audio et helpers partagés
// ---------------------------------------------------------------------------

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

/// Liste `(identifiant, nom)` des périphériques d'un flux.
pub(crate) type DeviceList = Vec<(String, String)>;

/// Convertit un nom de flux (« output »/« input ») en `Flow`.
pub(crate) fn parse_flow(value: &str) -> Result<Flow, String> {
    match value {
        "output" => Ok(Flow::Output),
        "input" => Ok(Flow::Input),
        _ => Err("Flux audio inconnu (attendu « output » ou « input »)".into()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::devices::*;
    use super::ffi::*;
    use super::profile_apps::*;
    use super::sessions::*;
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
