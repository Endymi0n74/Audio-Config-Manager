//! Logique métier « routage par application » : types de la section
//! `applications` du profil JSON, résolution des processus en cours, et
//! opérations publiques (export, aperçu, restauration, vue « Applications »).

use super::ffi::{
    with_apartment, CloseHandle, CreateToolhelp32Snapshot, OpenProcess, PolicyConfig,
    Process32FirstW, Process32NextW, ProcessEntry32W, QueryFullProcessImageNameW,
    PROCESS_QUERY_LIMITED_INFORMATION, TH32CS_SNAPPROCESS,
};
use super::sessions::{list_session_pids_noinit, list_sessions_noinit};
use super::Flow;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

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
// Helpers sur les listes de périphériques
// ---------------------------------------------------------------------------

/// Nom convivial d'un identifiant de périphérique, si présent dans la liste.
fn device_name(devices: &[(String, String)], device_id: &str) -> Option<String> {
    devices
        .iter()
        .find(|(id, _)| id.eq_ignore_ascii_case(device_id))
        .map(|(_, name)| name.clone())
}

/// Cible connue ? Identifiant d'abord, nom ensuite (insensible à la casse).
fn device_known(devices: &[(String, String)], target: &RouteTarget) -> bool {
    devices.iter().any(|(id, name)| {
        id.eq_ignore_ascii_case(&target.device_id)
            || target
                .device_name
                .as_deref()
                .is_some_and(|n| name.eq_ignore_ascii_case(n))
    })
}

/// Lit un profil JSON en ignorant le BOM UTF-8 (PowerShell 5.1).
fn read_profile_json(profile_path: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(profile_path).map_err(|e| format!("Profil illisible : {e}"))?;
    // Set-Content -Encoding UTF8 de PowerShell 5.1 écrit un BOM UTF-8.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let raw = String::from_utf8_lossy(bytes);
    serde_json::from_str(&raw).map_err(|e| format!("Profil JSON invalide : {e}"))
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
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    // SAFETY : tampon assez grand (1024 wchar_t).
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) };
    unsafe {
        CloseHandle(handle);
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
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() {
        return Vec::new();
    }
    let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
    entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
    let mut procs = Vec::new();
    // SAFETY : première entrée de l'instantané.
    let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) };
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
        ok = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    unsafe {
        CloseHandle(snapshot);
    }
    procs
}

pub(super) fn paths_equal(a: &str, b: &str) -> bool {
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
pub(super) fn record_index(
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
pub(crate) fn export_app_routes(
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
                    device_name: device_name(&devices, &device_id),
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
    let mut root: Value = read_profile_json(profile_path)?;

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
pub(crate) fn applications_in_profile(
    profile_path: &Path,
) -> Result<Vec<ApplicationEntry>, String> {
    let root: Value = read_profile_json(profile_path)?;
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
                TargetPreview {
                    present: device_known(devices, route),
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
pub(crate) fn restore_applications(
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
                if !device_known(devices, target) {
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
                        let device_name = device_name(devices, &device_id);
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
