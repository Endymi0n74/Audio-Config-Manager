//! Moteur PowerShell : exécute le script audio-config-manager.ps1 embarqué
//! via `powershell.exe -NoProfile -ExecutionPolicy Bypass -File <script>
//! -Action <action> -ConfigPath <path>` et analyse la réponse JSON, comme
//! l'application originale (« Audio Config Manager », v3.1).
//!
//! Le script repose sur le module AudioDeviceCmdlets (module PSGallery) qui
//! expose Get-AudioDevice / Set-AudioDevice — l'API WASAPI + IPolicyConfig
//! en PowerShell, beaucoup plus fiable que l'énumération COM directe.

use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::settings::ERR_CONFIG_DIR;

/// Empêche un enfant PowerShell d'afficher une fenêtre de console.
///
/// L'application est liée en GUI (subsystem windows) et n'a donc aucune
/// console : sans ce drapeau, chaque `powershell.exe` lancé crée sa propre
/// console **visible** qui clignote à l'écran (contrairement à
/// `-WindowStyle Hidden`, qui ne masque que la fenêtre de PowerShell lui-
/// même). `CREATE_NO_WINDOW` (0x08000000) interdit la création de la
/// console au niveau du processus, comme le faisait l'application
/// originale via `subprocess.CREATE_NO_WINDOW`.
fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

/// Nom du script embarqué dans la binaire.
pub const SCRIPT_FILENAME: &str = "audio-config-manager.ps1";
/// Délai maximal pour une opération audio (overview/export/preview/restore).
pub const SCRIPT_TIMEOUT: Duration = Duration::from_secs(60);
/// Délai maximal pour l'installation du module (téléchargement NuGet/PSGallery).
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

const PS_EXE: &str = "powershell.exe";

/// Écrit le script PowerShell embarqué dans le dossier de configuration
/// (s'il n'existe pas encore ou s'il a changé) et renvoie son chemin.
///
/// Le fichier est écrit avec un **BOM UTF-8** : sans lui, Windows
/// PowerShell 5.1 lit les scripts `.ps1` en ANSI et les accents des
/// littéraux français (« n’est pas installé », « sauvegardé »…) seraient
/// doublement encodés.
pub fn ensure_script(config_dir: &Path) -> Result<std::path::PathBuf, String> {
    let script_path = config_dir.join(SCRIPT_FILENAME);

    // Cache : `ready()` (chaque commande) rappelle cette fonction — une fois
    // le script vérifié/écrit, inutile de relire+recomparer tout le fichier :
    // une simple existence suffit. Le contenu embarqué étant constant dans un
    // processus, la comparaison complète n'est utile qu'au premier appel (ou
    // si le fichier a été supprimé). Réécrire à chaque appel reviendrait à
    // écraser les éditions de l'utilisateur à chaque commande.
    static ENSURED: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);
    let cached = ENSURED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if cached.as_deref() == Some(script_path.as_path()) && script_path.is_file() {
        return Ok(script_path);
    }

    let embedded = include_str!("../audio-config-manager.ps1");
    let expected = format!("\u{feff}{embedded}");
    match std::fs::read_to_string(&script_path) {
        Ok(existing) if existing == expected => {}
        _ => {
            std::fs::create_dir_all(config_dir)
                .map_err(|e| format!("{ERR_CONFIG_DIR} : {e}"))?;
            std::fs::write(&script_path, expected.as_bytes())
                .map_err(|e| format!("Écriture du script impossible : {e}"))?;
        }
    }
    *ENSURED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(script_path.clone());
    Ok(script_path)
}

/// Lance une commande, récupère stdout/stderr sans blocage (lectures dans
/// des threads pour éviter le deadlock sur les pipes) et impose un délai.
fn run_capture(
    cmd: &mut Command,
    timeout: Duration,
    spawn_error: impl FnOnce(std::io::Error) -> String,
) -> Result<(String, String, Option<i32>), String> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(spawn_error)?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Sortie PowerShell indisponible")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Erreur PowerShell indisponible")?;

    let out_thread = std::thread::spawn(move || {
        let mut out = String::new();
        let mut reader = stdout;
        let _ = reader.read_to_string(&mut out);
        out
    });
    let err_thread = std::thread::spawn(move || {
        let mut out = String::new();
        let mut reader = stderr;
        let _ = reader.read_to_string(&mut out);
        out
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("L'opération PowerShell a dépassé le délai.".into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("Échec d'attente de PowerShell : {e}")),
        }
    };

    let stdout = out_thread
        .join()
        .map_err(|_| String::from("Lecture PowerShell interrompue"))?;
    let stderr = err_thread
        .join()
        .map_err(|_| String::from("Lecture PowerShell interrompue"))?;
    Ok((stdout, stderr, status.code()))
}

/// Exécute le script embarqué avec une action donnée et renvoie le JSON.
pub fn run_script(
    script: &Path,
    action: &str,
    config_path: Option<&Path>,
) -> Result<Value, String> {
    let mut cmd = Command::new(PS_EXE);
    hide_console(&mut cmd);
    cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .arg("-Action")
        .arg(action);
    if let Some(path) = config_path {
        cmd.arg("-ConfigPath").arg(path);
    }

    let (stdout, stderr, code) = run_capture(
        &mut cmd,
        SCRIPT_TIMEOUT,
        |e| format!("PowerShell indisponible : {e}"),
    )?;

    if code != Some(0) {
        let detail = stderr.trim();
        if !detail.is_empty() {
            return Err(detail.to_string());
        }
        return Err(format!("L'action « {action} » a échoué (code {})", code.unwrap_or(-1)));
    }

    parse_json_output(&stdout)
}

/// Extrait le dernier objet JSON de la sortie : l'action `restore` fait
/// aussi imprimer les objets périphériques par `Set-AudioDevice` avant la
/// réponse finale, il faut donc ignorer tout ce qui précède.
fn parse_json_output(stdout: &str) -> Result<Value, String> {
    // Depuis la fin : la réponse finale est la dernière ligne JSON imprimée
    // (restore fait précéder la réponse d'objets périphériques non JSON) —
    // on s'arrête à la première occurrence au lieu de parser tout le début.
    for line in stdout.lines().rev() {
        if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
            return Ok(value);
        }
    }
    let preview: String = stdout.chars().take(200).collect();
    Err(format!("Réponse audio invalide : {preview}"))
}

/// Installe le module AudioDeviceCmdlets (fournisseur NuGet + PSGallery),
/// sans fenêtre (CREATE_NO_WINDOW) — commande identique à l'original.
pub fn install_audio_module() -> Result<String, String> {
    let command = "Install-PackageProvider NuGet -Force -Scope CurrentUser; Set-PSRepository PSGallery -InstallationPolicy Trusted; Install-Module AudioDeviceCmdlets -Scope CurrentUser -Force -AllowClobber";
    let mut cmd = Command::new(PS_EXE);
    hide_console(&mut cmd);
    cmd.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-Command",
        command,
    ]);
    let (_, stderr, code) = run_capture(
        &mut cmd,
        INSTALL_TIMEOUT,
        |e| format!("PowerShell indisponible : {e}"),
    )?;
    if code == Some(0) {
        invalidate_module_cache();
        Ok("Module AudioDeviceCmdlets installé.".to_string())
    } else {
        let detail = stderr.trim();
        if !detail.is_empty() {
            Err(format!("Installation du module impossible : {detail}"))
        } else {
            Err(format!(
                "Installation du module impossible (code {})",
                code.unwrap_or(-1)
            ))
        }
    }
}

/// Disponibilité du module AudioDeviceCmdlets, sans lancer PowerShell :
/// `Get-Module -ListAvailable` revient à chercher un dossier versionné
/// contenant le manifeste dans les racines de modules de la session.
/// Mis en cache (l'aperçu le relit toutes les 15 s) — invalidé par
/// `invalidate_module_cache` après installation.
static MODULE_AVAILABLE: Mutex<Option<bool>> = Mutex::new(None);

/// Le module AudioDeviceCmdlets est-il installé ? (check fichiers, µs)
pub fn module_available() -> bool {
    let cached = *MODULE_AVAILABLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(available) = cached {
        return available;
    }
    let available = module_present_in(&module_roots());
    *MODULE_AVAILABLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(available);
    available
}

/// Invalide le cache de `module_available` (après installation du module).
pub fn invalidate_module_cache() {
    *MODULE_AVAILABLE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// Racines de recherche des modules PowerShell, comme PowerShell les calcule
/// au démarrage : modules du compte courant (sous Documents, éventuellement
/// redirigé vers OneDrive — c'est là qu'installe
/// `Install-Module -Scope CurrentUser`), entrées `PSModulePath` de la
/// session, puis modules système.
fn module_roots() -> Vec<PathBuf> {
    fn documents_modules(base: &Path) -> [PathBuf; 2] {
        [
            base.join("Documents").join("WindowsPowerShell").join("Modules"),
            base.join("Documents").join("PowerShell").join("Modules"),
        ]
    }

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(home) = std::env::var("USERPROFILE") {
        roots.extend(documents_modules(Path::new(&home)));
    }
    for var in ["OneDrive", "OneDriveConsumer"] {
        if let Ok(base) = std::env::var(var) {
            roots.extend(documents_modules(Path::new(&base)));
        }
    }
    if let Ok(psp) = std::env::var("PSModulePath") {
        roots.extend(
            psp.split(';')
                .filter(|entry| !entry.is_empty())
                .map(PathBuf::from),
        );
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(base) = std::env::var(var) {
            roots.push(Path::new(&base).join("WindowsPowerShell").join("Modules"));
            roots.push(Path::new(&base).join("PowerShell").join("Modules"));
        }
    }
    roots
}

/// Existe-t-il, sous une racine, un dossier `AudioDeviceCmdlets\<version>\`
/// contenant un manifeste (`.psd1`/`.psm1`/`.dll`) ? Équivalent fichier de
/// `Get-Module -ListAvailable -Name AudioDeviceCmdlets`.
fn module_present_in(roots: &[PathBuf]) -> bool {
    const MANIFESTS: [&str; 3] = [
        "AudioDeviceCmdlets.psd1",
        "AudioDeviceCmdlets.psm1",
        "AudioDeviceCmdlets.dll",
    ];
    roots.iter().any(|root| {
        let Ok(versions) = std::fs::read_dir(root.join("AudioDeviceCmdlets")) else {
            return false;
        };
        versions.flatten().any(|version| {
            let dir = version.path();
            MANIFESTS.iter().any(|file| dir.join(file).is_file())
        })
    })
}

/// Contenu de l'aperçu d'un profil avant restauration.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewInfo {
    pub playback_name: Option<String>,
    pub playback_volume: Option<f64>,
    pub playback_found: bool,
    pub recording_name: Option<String>,
    pub recording_volume: Option<f64>,
    pub recording_found: bool,
}

/// Résultat brut de la restauration renvoyé par le script.
#[derive(Debug, serde::Deserialize)]
pub struct RestoreInfo {
    pub ok: bool,
    pub applied: u32,
    pub missing: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_json() {
        let out = "{\"ok\":true,\"message\":\"Profil sauvegardé\"}";
        let value = parse_json_output(out).unwrap();
        assert_eq!(value["ok"], true);
    }

    #[test]
    fn parses_last_json_after_cmdlet_noise() {
        // Set-AudioDevice imprime des objets périphériques avant la réponse.
        let out = "\r\n\r\nIndex : 1\r\nDefault : True\r\nType : Playback\r\n\r\n{\"ok\":true,\"applied\":2,\"missing\":[]}\r\n";
        let value = parse_json_output(out).unwrap();
        assert_eq!(value["applied"], 2);
    }

    #[test]
    fn rejects_output_without_json() {
        let out = "Index : 1\r\nDefault : True\r\n";
        assert!(parse_json_output(out).is_err());
    }

    #[test]
    fn module_present_requires_versioned_manifest() {
        let root = crate::profiles::temp_dir("acm-modcheck");
        // Rien → module absent.
        assert!(!module_present_in(std::slice::from_ref(&root)));
        // Dossier de module sans manifeste → toujours absent.
        let version = root.join("AudioDeviceCmdlets").join("3.1.0.2");
        std::fs::create_dir_all(&version).unwrap();
        assert!(!module_present_in(std::slice::from_ref(&root)));
        // Manifeste présent → module détecté (comme Get-Module -ListAvailable).
        std::fs::write(version.join("AudioDeviceCmdlets.psd1"), "@{}").unwrap();
        assert!(module_present_in(&[root]));
    }
}