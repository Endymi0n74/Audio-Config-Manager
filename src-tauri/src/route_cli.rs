//! CLI de débogage du routage audio par application — sous-commande
//! `route` du binaire principal (« Audio Config Manager.exe route … »).
//!
//! Le binaire principal est lié en subsystem GUI (aucune fenêtre de
//! terminal au lancement normal). Quand on l'invoque comme CLI, il faut
//! donc rattacher une console avant toute sortie : `attach_console`
//! s'attache à la console du parent (terminal cmd/PowerShell) ou en crée
//! une (double-clic), puis redirige stdout/stderr/stdin vers `CONOUT$` /
//! `CONIN$`. Si des descripteurs valides ont été hérités (lancement avec
//! redirection ou pipes, tests), on les laisse intacts.
//!
//! Utilisation :
//!   Audio Config Manager.exe route sessions                     liste les processus audio actifs
//!   Audio Config Manager.exe route devices [output|input]       liste les périphériques actifs
//!   Audio Config Manager.exe route get <process>                routes persistées d'un processus
//!   Audio Config Manager.exe route set <process> <output|input> <device|system>
//!                                                               route (ou efface avec « system »)
//!
//! `process` = PID numérique ou nom d'exécutable (« firefox », « mirc.exe »…).
//! `device`  = identifiant (emballé ou non), nom exact, ou sous-chaîne
//!             unique (« voicemeeter in 2 »).
//!
//! Anciennement un binaire séparé (`src/bin/route.rs` → `route.exe`) ;
//! intégré au binaire principal pour n'avoir qu'un seul exe à distribuer.

use std::ffi::c_void;
use std::path::Path;

use crate::app_routing::{
    active_devices, list_active_app_routes, routing_available, set_app_route, AppSessionRow, Flow,
};

const USAGE: &str = "\
Audio Config Manager — route (routage audio par application)

Usage :
  Audio Config Manager.exe route sessions                      Liste les processus audio actifs et leurs routes
  Audio Config Manager.exe route devices [output|input]        Liste les périphériques actifs (identifiant + nom)
  Audio Config Manager.exe route get <process>                 Routes persistées d'un processus
  Audio Config Manager.exe route set <process> <output|input> <device|system>
                                                               Route le processus vers un périphérique ;
                                                               « system » efface la route (périphérique système)

Exemples :
  Audio Config Manager.exe route sessions
  Audio Config Manager.exe route devices output
  Audio Config Manager.exe route set 1234 output \"Voicemeeter In 2 (VB-Audio Voicemeeter VAIO)\"
  Audio Config Manager.exe route set firefox input system";

// ---------------------------------------------------------------------------
// Attachement de console (l'exe est lié en subsystem GUI)
// ---------------------------------------------------------------------------

const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // -11
const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4; // -12
const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6; // -10
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const OPEN_EXISTING: u32 = 3;
const FILE_TYPE_UNKNOWN: u32 = 0x0000;
const INVALID_HANDLE_VALUE: *mut c_void = usize::MAX as *mut c_void;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn AttachConsole(pid: u32) -> i32;
    fn AllocConsole() -> i32;
    fn GetStdHandle(n_std_handle: u32) -> *mut c_void;
    fn SetStdHandle(n_std_handle: u32, handle: *mut c_void) -> i32;
    fn CreateFileW(
        name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security: *mut c_void,
        creation_disposition: u32,
        flags: u32,
        template: *mut c_void,
    ) -> *mut c_void;
    fn GetFileType(handle: *mut c_void) -> u32;
}

/// Rattache une console à l'exe GUI pour que `println!`/`eprintln!` aient
/// une sortie visible. Sans effet si des descripteurs valides ont déjà été
/// hérités (pipes/redirection, contexte de test) : les sorties existantes
/// sont alors laissées intactes.
pub fn attach_console() {
    unsafe {
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        if !stdout.is_null()
            && stdout != INVALID_HANDLE_VALUE
            && GetFileType(stdout) != FILE_TYPE_UNKNOWN
        {
            return;
        }
        // Pas de console parente (double-clic) : en créer une.
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            AllocConsole();
        }
        for (id, name, access) in [
            (STD_OUTPUT_HANDLE, "CONOUT$", GENERIC_WRITE),
            (STD_ERROR_HANDLE, "CONOUT$", GENERIC_WRITE),
            (STD_INPUT_HANDLE, "CONIN$", GENERIC_READ),
        ] {
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            let handle = CreateFileW(
                wide.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if handle != INVALID_HANDLE_VALUE && !handle.is_null() {
                SetStdHandle(id, handle);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/// Point d'entrée de la sous-commande `route` : code de sortie (0 = OK,
/// 1 = erreur/usage). `main.rs` appelle `attach_console` avant.
pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        None | Some("help" | "-h" | "--help") => {
            println!("{USAGE}");
            if args.is_empty() { 1 } else { 0 }
        }
        Some("sessions") => cmd_sessions(),
        Some("devices") => cmd_devices(args.get(1).map(String::as_str)),
        Some("get") => {
            let Some(process) = args.get(1) else {
                eprintln!("Usage : route get <process>");
                return 1;
            };
            cmd_get(process)
        }
        Some("set") => cmd_set(args),
        Some(other) => {
            eprintln!("Commande inconnue : {other}\n");
            println!("{USAGE}");
            1
        }
    }
}

/// Vérifie le moteur et récupère sessions + périphériques en une fois.
type DeviceList = Vec<(String, String)>;

fn collect() -> Result<(Vec<AppSessionRow>, DeviceList, DeviceList), String> {
    if !routing_available() {
        return Err("Routage par application indisponible sur ce système.".to_string());
    }
    let playback = active_devices(Flow::Output);
    let recording = active_devices(Flow::Input);
    let sessions = list_active_app_routes(&playback, &recording)?;
    Ok((sessions, playback, recording))
}

fn device_label(route: &Option<crate::app_routing::RouteTarget>) -> String {
    route
        .as_ref()
        .map(|t| {
            if let Some(name) = &t.device_name {
                if !name.is_empty() {
                    return name.clone();
                }
            }
            t.device_id.clone()
        })
        .unwrap_or_else(|| "système".to_string())
}

fn cmd_sessions() -> i32 {
    let (sessions, _, _) = match collect() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if sessions.is_empty() {
        println!("Aucune application audio active. Lancez un lecteur puis réessayez.");
        return 0;
    }
    println!("{:<8}{:<5}{:<26}{:<44}ENTRÉE", "PID", "LECT", "PROCESSUS", "SORTIE");
    for session in &sessions {
        println!(
            "{:<8}{:<5}{:<26}{:<44}{}",
            session.pid,
            if session.playing { "●" } else { "·" },
            session.process_name,
            device_label(&session.output),
            device_label(&session.input),
        );
    }
    println!("● = joue actuellement du son");
    0
}

fn cmd_devices(flow_arg: Option<&str>) -> i32 {
    let flows: Vec<(&str, Flow)> = match flow_arg {
        None => vec![("output", Flow::Output), ("input", Flow::Input)],
        Some("output") => vec![("output", Flow::Output)],
        Some("input") => vec![("input", Flow::Input)],
        Some(other) => {
            eprintln!("Flux inconnu : {other} (attendu « output » ou « input »)");
            return 1;
        }
    };
    for (label, flow) in flows {
        println!("== {label} ==");
        let devices = active_devices(flow);
        if devices.is_empty() {
            println!("  (aucun périphérique actif)");
        }
        for (id, name) in devices {
            println!("  {name}\n    {id}");
        }
    }
    0
}

/// Nom de base d'un exécutable, sans l'extension `.exe`.
fn base_name(exe: Option<&str>) -> Option<String> {
    let path = exe?;
    let file = Path::new(path).file_name()?.to_string_lossy().to_string();
    Some(file.strip_suffix(".exe").unwrap_or(&file).to_lowercase())
}

/// Retrouve les PID à router : PID numérique exact, ou nom d'exécutable
/// (avec ou sans `.exe`, insensible à la casse) parmi les processus actifs.
fn resolve_process(query: &str, sessions: &[AppSessionRow]) -> Result<Vec<u32>, String> {
    if let Ok(pid) = query.parse::<u32>() {
        if pid == 0 {
            return Err("PID invalide : 0".into());
        }
        if !sessions.iter().any(|s| s.pid == pid) {
            return Err(format!(
                "PID {pid} : aucune session audio active. Lancez l'application puis réessayez (ou « route sessions »)."
            ));
        }
        return Ok(vec![pid]);
    }
    let query_lower = query.to_lowercase();
    let query_base = query_lower.strip_suffix(".exe").unwrap_or(&query_lower);
    let matches: Vec<&AppSessionRow> = sessions
        .iter()
        .filter(|s| {
            let name = s.process_name.to_lowercase();
            let name_base = name.strip_suffix(".exe").unwrap_or(&name);
            name == query_lower
                || name == format!("{query_base}.exe")
                || name_base == query_base
                || base_name(s.executable_path.as_deref()).as_deref() == Some(query_base)
        })
        .collect();
    if matches.is_empty() {
        return Err(format!(
            "Aucun processus audio actif « {query} ». Utilisez « route sessions » pour lister les processus."
        ));
    }
    Ok(matches.iter().map(|s| s.pid).collect())
}

/// Retrouve l'identifiant d'un périphérique : « system » efface la route ;
/// sinon identifiant (emballé ou non), nom exact, ou sous-chaîne unique.
fn resolve_device(query: &str, flow: Flow, devices: &[(String, String)]) -> Result<Option<String>, String> {
    let lower = query.to_lowercase();
    if matches!(lower.as_str(), "system" | "système" | "default" | "clear" | "-" | "") {
        return Ok(None);
    }
    if devices.is_empty() {
        return Err("Aucun périphérique actif sur ce flux.".to_string());
    }
    // Identifiant direct (non emballé), ou emballé (\\?\SWD#MMDEVAPI#…).
    for (id, _) in devices {
        if id.eq_ignore_ascii_case(query) {
            return Ok(Some(id.clone()));
        }
    }
    if let Some(rest) = query.strip_prefix(r"\\?\SWD#MMDEVAPI#") {
        let mut candidate = rest.to_string();
        if let Some(sharp_brace) = candidate.rfind("#{") {
            candidate.truncate(sharp_brace);
        }
        for (id, _) in devices {
            if id.eq_ignore_ascii_case(&candidate) {
                return Ok(Some(id.clone()));
            }
        }
    }
    // Nom exact, puis sous-chaîne unique.
    let exact: Vec<&(String, String)> = devices
        .iter()
        .filter(|(_, name)| name.eq_ignore_ascii_case(query))
        .collect();
    if exact.len() == 1 {
        return Ok(Some(exact[0].0.clone()));
    }
    let partial: Vec<&(String, String)> = devices
        .iter()
        .filter(|(_, name)| name.to_lowercase().contains(&lower))
        .collect();
    match partial.len() {
        1 => Ok(Some(partial[0].0.clone())),
        0 => Err(format!(
            "Aucun périphérique de sortie correspondant à « {query} ». Utilisez « route devices {} ».",
            match flow {
                Flow::Output => "output",
                Flow::Input => "input",
            }
        )),
        _ => Err(format!(
            "Plusieurs périphériques correspondent à « {query} » : {}",
            partial
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>()
                .join(" · ")
        )),
    }
}

fn cmd_get(process: &str) -> i32 {
    let (sessions, _, _) = match collect() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let pids = match resolve_process(process, &sessions) {
        Ok(pids) => pids,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    for session in sessions.iter().filter(|s| pids.contains(&s.pid)) {
        println!(
            "{} (PID {}) : sortie = {}, entrée = {}",
            session.process_name,
            session.pid,
            device_label(&session.output),
            device_label(&session.input),
        );
    }
    0
}

fn cmd_set(args: &[String]) -> i32 {
    let Some(process) = args.get(1) else {
        eprintln!("Usage : route set <process> <output|input> <device|system>");
        return 1;
    };
    let Some(flow_arg) = args.get(2) else {
        eprintln!("Usage : route set <process> <output|input> <device|system>");
        return 1;
    };
    let Some(device_query) = args.get(3) else {
        eprintln!("Usage : route set <process> <output|input> <device|system>");
        return 1;
    };
    let flow = match flow_arg.as_str() {
        "output" => Flow::Output,
        "input" => Flow::Input,
        _ => {
            eprintln!("Flux inconnu : {flow_arg} (attendu « output » ou « input »)");
            return 1;
        }
    };
    let flow_label = match flow {
        Flow::Output => "sortie",
        Flow::Input => "entrée",
    };

    let (sessions, playback, recording) = match collect() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let pids = match resolve_process(process, &sessions) {
        Ok(pids) => pids,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let devices = match flow {
        Flow::Output => &playback,
        Flow::Input => &recording,
    };
    let device_id = match resolve_device(device_query, flow, devices) {
        Ok(device) => device,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    let mut ok = true;
    for pid in &pids {
        match set_app_route(*pid, flow, device_id.clone()) {
            Ok(()) => {
                let target = match &device_id {
                    Some(id) => {
                        let name = devices
                            .iter()
                            .find(|(d, _)| d == id)
                            .map(|(_, n)| n.as_str())
                            .unwrap_or(id.as_str());
                        format!("→ {name}")
                    }
                    None => "→ périphérique système".to_string(),
                };
                println!("OK : {} (PID {pid}) — {flow_label} {target}", process);
            }
            Err(e) => {
                ok = false;
                eprintln!("Échec pour le PID {pid} : {e}");
            }
        }
    }
    if ok { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, process_name: &str, exe: Option<&str>) -> AppSessionRow {
        AppSessionRow {
            pid,
            process_name: process_name.to_string(),
            executable_path: exe.map(str::to_string),
            playing: false,
            output: None,
            input: None,
        }
    }

    #[test]
    fn resolve_process_by_pid_and_name() {
        let sessions = vec![
            row(1234, "firefox.exe", Some("C:\\Program Files\\Mozilla Firefox\\firefox.exe")),
            row(5678, "spotify.exe", Some("C:\\Users\\x\\AppData\\Roaming\\Spotify\\Spotify.exe")),
        ];
        // PID exact.
        assert_eq!(resolve_process("1234", &sessions).unwrap(), vec![1234]);
        // Nom avec ou sans .exe, insensible à la casse.
        assert_eq!(resolve_process("firefox", &sessions).unwrap(), vec![1234]);
        assert_eq!(resolve_process("FIREFOX.EXE", &sessions).unwrap(), vec![1234]);
        // PID absent → erreur.
        assert!(resolve_process("9999", &sessions).is_err());
        // Nom inconnu → erreur.
        assert!(resolve_process("chrome", &sessions).is_err());
    }

    #[test]
    fn resolve_device_by_id_name_and_substring() {
        let devices = vec![
            ("\\\\?\\SWD#MMDEVAPI#{aaa}#{e}".to_string(), "Casque USB".to_string()),
            ("\\\\?\\SWD#MMDEVAPI#{bbb}#{e}".to_string(), "Voicemeeter In 2 (VB-Audio Voicemeeter VAIO)".to_string()),
        ];
        // « system » efface la route.
        assert_eq!(resolve_device("system", Flow::Output, &devices).unwrap(), None);
        // Identifiant exact.
        assert_eq!(
            resolve_device("\\\\?\\SWD#MMDEVAPI#{aaa}#{e}", Flow::Output, &devices).unwrap(),
            Some("\\\\?\\SWD#MMDEVAPI#{aaa}#{e}".to_string())
        );
        // Nom exact.
        assert_eq!(resolve_device("casque usb", Flow::Output, &devices).unwrap(), Some(devices[0].0.clone()));
        // Sous-chaîne unique.
        assert_eq!(resolve_device("voicemeeter in 2", Flow::Output, &devices).unwrap(), Some(devices[1].0.clone()));
        // Inconnu → erreur.
        assert!(resolve_device("inexistant", Flow::Output, &devices).is_err());
    }
}