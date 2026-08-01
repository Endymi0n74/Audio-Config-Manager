"""Audio Config Manager - Windows 11 audio backup and restore.

The global device/volume operations use AudioDeviceCmdlets.  Per-application
routing uses Windows.Media.Internal.AudioPolicyConfig through
winappaudiorouter (bundled in the executable).
"""

from __future__ import annotations

import argparse
import copy
import datetime as dt
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import threading
import traceback
import urllib.request
import webbrowser
from dataclasses import asdict
from pathlib import Path
from typing import Any, Callable

import tkinter as tk
from tkinter import filedialog, messagebox
from tkinter.scrolledtext import ScrolledText


APP_NAME = "Audio Config Manager"
APP_VERSION = "6.1.0"
SCHEMA_NAME = "audio-config-manager"
SCHEMA_VERSION = 2
GITHUB_REPOSITORY = "Endymi0n74/Audioconfigmanager"
HISTORY_DIR = Path(os.environ.get("LOCALAPPDATA", Path.home())) / "AudioConfigManager" / "history"
POWERSHELL = shutil.which("powershell.exe") or shutil.which("powershell")


def _resource_dir() -> Path:
    return Path(sys.executable if getattr(sys, "frozen", False) else __file__).resolve().parent


if not getattr(sys, "frozen", False):
    vendor = _resource_dir() / "vendor"
    if vendor.is_dir():
        sys.path.insert(0, str(vendor))


class AudioConfigError(RuntimeError):
    """A user-facing audio configuration error."""


def _powershell_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def _run_powershell(script: str, timeout: int = 120) -> dict[str, Any]:
    if not POWERSHELL:
        raise AudioConfigError("Windows PowerShell 5.1 est introuvable.")
    fd, script_name = tempfile.mkstemp(prefix="audio-config-", suffix=".ps1")
    os.close(fd)
    script_path = Path(script_name)
    try:
        script_path.write_text(script, encoding="utf-8-sig")
        flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
        result = subprocess.run(
            [POWERSHELL, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", str(script_path)],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=timeout,
            creationflags=flags,
        )
    finally:
        script_path.unlink(missing_ok=True)

    marker = "ACM_RESULT:"
    payload = next((line[len(marker):] for line in reversed(result.stdout.splitlines()) if line.startswith(marker)), None)
    if payload is None:
        detail = (result.stderr or result.stdout or f"code {result.returncode}").strip()
        raise AudioConfigError(f"PowerShell n'a pas renvoyé de résultat valide : {detail}")
    try:
        decoded = json.loads(payload)
    except json.JSONDecodeError as exc:
        raise AudioConfigError("Réponse PowerShell illisible.") from exc
    if result.returncode != 0 or decoded.get("fatal"):
        raise AudioConfigError(decoded.get("fatal") or result.stderr.strip() or "Erreur PowerShell.")
    return decoded


def _raw_audio_device(device: Any) -> Any:
    """Resolve a public AudioDeviceInfo to pycaw's underlying endpoint."""
    if hasattr(device, "_dev"):
        return device
    from pycaw.pycaw import AudioUtilities

    wanted = str(device.id).casefold()
    found = next((item for item in AudioUtilities.GetAllDevices() if item.id.casefold() == wanted), None)
    if found is None:
        raise AudioConfigError(f"Endpoint audio introuvable : {device.id}")
    return found


def _endpoint_volume(device: Any) -> float:
    """Read one endpoint's master volume as a percentage."""
    import comtypes
    from pycaw.api.endpointvolume import IAudioEndpointVolume

    raw = _raw_audio_device(device)
    interface = raw._dev.Activate(IAudioEndpointVolume._iid_, comtypes.CLSCTX_ALL, None)
    endpoint = interface.QueryInterface(IAudioEndpointVolume)
    return round(float(endpoint.GetMasterVolumeLevelScalar()) * 100.0, 4)


def _set_endpoint_volume(device: Any, percentage: float) -> None:
    """Set one endpoint's master volume; zero is deliberately valid."""
    import comtypes
    from pycaw.api.endpointvolume import IAudioEndpointVolume

    value = max(0.0, min(100.0, float(percentage))) / 100.0
    raw = _raw_audio_device(device)
    interface = raw._dev.Activate(IAudioEndpointVolume._iid_, comtypes.CLSCTX_ALL, None)
    endpoint = interface.QueryInterface(IAudioEndpointVolume)
    endpoint.SetMasterVolumeLevelScalar(value, None)


def check_dependencies() -> dict[str, Any]:
    checks: dict[str, Any] = {
        "windows": os.name == "nt",
        "powershell": bool(POWERSHELL),
        "audioDeviceCmdlets": False,
        "appRouting": False,
        "errors": [],
    }
    if not checks["windows"]:
        checks["errors"].append("Cette application nécessite Windows 11.")
    if not POWERSHELL:
        checks["errors"].append("Windows PowerShell 5.1 est introuvable.")
    else:
        try:
            probe = _run_powershell(r"""
$ErrorActionPreference = 'Stop'
$r = @{ fatal = $null; available = $false; version = $null }
try {
  $m = Get-Module -ListAvailable AudioDeviceCmdlets | Sort-Object Version -Descending | Select-Object -First 1
  if ($null -eq $m) { throw "Module PowerShell AudioDeviceCmdlets manquant. Installez-le avec : Install-Module AudioDeviceCmdlets -Scope CurrentUser" }
  Import-Module $m.Path -ErrorAction Stop
  $r.available = $true
  $r.version = $m.Version.ToString()
} catch { $r.fatal = $_.Exception.Message }
Write-Output ('ACM_RESULT:' + ($r | ConvertTo-Json -Compress))
if ($r.fatal) { exit 1 }
""")
            checks["audioDeviceCmdlets"] = bool(probe.get("available"))
            checks["audioDeviceCmdletsVersion"] = probe.get("version")
        except Exception as exc:
            checks["errors"].append(str(exc))
    try:
        import winappaudiorouter as war  # noqa: F401
        checks["appRouting"] = True
    except Exception as exc:
        checks["errors"].append(f"Moteur de routage par application indisponible : {exc}")
    return checks


GLOBAL_EXPORT_SCRIPT = r"""
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$r = @{ fatal = $null; playbackDevices = @(); recordingDevices = @(); defaults = @{}; warnings = @() }
function Obj($d) {
  if ($null -eq $d) { return $null }
  return @{ id = [string]$d.ID; name = [string]$d.Name; volume = if ($null -eq $d.Volume) { $null } else { [double]$d.Volume } }
}
try {
  $m = Get-Module -ListAvailable AudioDeviceCmdlets | Sort-Object Version -Descending | Select-Object -First 1
  if ($null -eq $m) { throw "Module AudioDeviceCmdlets manquant. Exécutez : Install-Module AudioDeviceCmdlets -Scope CurrentUser" }
  Import-Module $m.Path -ErrorAction Stop
  foreach ($d in @(Get-AudioDevice -List | Where-Object Type -eq 'Playback')) {
    try { $r.playbackDevices += Obj (Get-AudioDevice -ID $d.ID) } catch { $r.warnings += "Volume lecture non lu : $($d.Name) — $($_.Exception.Message)" }
  }
  foreach ($d in @(Get-AudioDevice -List | Where-Object Type -eq 'Recording')) {
    try { $r.recordingDevices += Obj (Get-AudioDevice -ID $d.ID) } catch { $r.warnings += "Volume enregistrement non lu : $($d.Name) — $($_.Exception.Message)" }
  }
  $r.defaults.playback = Obj (Get-AudioDevice -Playback)
  $r.defaults.playbackCommunication = Obj (Get-AudioDevice -PlaybackCommunication)
  $r.defaults.recording = Obj (Get-AudioDevice -Recording)
  $r.defaults.recordingCommunication = Obj (Get-AudioDevice -RecordingCommunication)
} catch { $r.fatal = $_.Exception.Message }
Write-Output ('ACM_RESULT:' + ($r | ConvertTo-Json -Depth 6 -Compress))
if ($r.fatal) { exit 1 }
"""


def _process_identity(pid: int, fallback_name: str | None) -> tuple[str | None, str | None]:
    try:
        import psutil
        process = psutil.Process(pid)
        return process.name(), process.exe()
    except Exception:
        return fallback_name, None


def _find_device(devices: list[Any], device_id: str) -> Any | None:
    return next((device for device in devices if device.id.casefold() == device_id.casefold()), None)


def export_application_routes() -> tuple[list[dict[str, Any]], list[str]]:
    import winappaudiorouter as war

    output_devices = war.list_output_devices()
    input_devices = war.list_input_devices()
    flows = [
        ("output", war.list_app_sessions(), war.get_app_output_device, output_devices),
        ("input", war.list_input_sessions(), war.get_app_input_device, input_devices),
    ]
    records: dict[tuple[str, str], dict[str, Any]] = {}
    warnings: list[str] = []
    for flow, sessions, getter, devices in flows:
        unique = {(s.process_id, s.process_name) for s in sessions if s.process_id > 0}
        for pid, fallback_name in sorted(unique):
            try:
                routes = getter(process_id=pid)
                device_id = routes.get(pid)
                if not device_id:
                    continue
                name, executable = _process_identity(pid, fallback_name)
                identity = ((executable or "").casefold(), (name or fallback_name or str(pid)).casefold())
                record = records.setdefault(identity, {
                    "processName": name or fallback_name,
                    "executablePath": executable,
                    "output": None,
                    "input": None,
                })
                device = _find_device(devices, device_id)
                record[flow] = {
                    "deviceId": device_id,
                    "deviceName": device.name if device else None,
                }
            except Exception as exc:
                warnings.append(f"Préférence {flow} non lue pour PID {pid} : {exc}")
    return sorted(records.values(), key=lambda x: ((x.get("processName") or "").casefold(), x.get("executablePath") or "")), warnings


def export_config(path: str | Path) -> dict[str, Any]:
    dependencies = check_dependencies()
    if dependencies["errors"]:
        raise AudioConfigError("\n".join(dependencies["errors"]))
    global_config = _run_powershell(GLOBAL_EXPORT_SCRIPT)
    import winappaudiorouter as war
    active_devices = {
        "playbackDevices": war.list_output_devices(),
        "recordingDevices": war.list_input_devices(),
    }
    for section, devices in active_devices.items():
        by_id = {device.id.casefold(): device for device in devices}
        for saved in global_config.get(section, []):
            device = by_id.get(str(saved.get("id", "")).casefold())
            if not device:
                continue
            saved["name"] = device.name
            try:
                saved["volume"] = _endpoint_volume(device)
            except Exception as exc:
                global_config.setdefault("warnings", []).append(f"Volume non lu : {device.name} — {exc}")
        for saved_default in global_config.get("defaults", {}).values():
            if not saved_default:
                continue
            device = by_id.get(str(saved_default.get("id", "")).casefold())
            if device:
                saved_default["name"] = device.name
                matching = next((item for item in global_config.get(section, []) if item.get("id", "").casefold() == device.id.casefold()), None)
                if matching:
                    saved_default["volume"] = matching.get("volume")
    applications, app_warnings = export_application_routes()
    config = {
        "schema": SCHEMA_NAME,
        "schemaVersion": SCHEMA_VERSION,
        "metadata": {
            "createdAt": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
            "computerName": platform.node(),
            "windowsVersion": platform.platform(),
            "applicationVersion": APP_VERSION,
        },
        "global": {
            "playbackDevices": global_config.get("playbackDevices", []),
            "recordingDevices": global_config.get("recordingDevices", []),
            "defaults": global_config.get("defaults", {}),
        },
        "applications": applications,
    }
    destination = Path(path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    temp = destination.with_suffix(destination.suffix + ".tmp")
    temp.write_text(json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8")
    os.replace(temp, destination)
    try:
        HISTORY_DIR.mkdir(parents=True, exist_ok=True)
        history_name = dt.datetime.now().strftime("%Y%m%d-%H%M%S") + "-audio-config.json"
        shutil.copy2(destination, HISTORY_DIR / history_name)
    except OSError as exc:
        global_config.setdefault("warnings", []).append(f"Historique non créé : {exc}")
    warnings = list(global_config.get("warnings", [])) + app_warnings
    return {
        "operation": "export",
        "success": True,
        "file": str(destination),
        "counts": {
            "playbackDevices": len(config["global"]["playbackDevices"]),
            "recordingDevices": len(config["global"]["recordingDevices"]),
            "applications": len(applications),
        },
        "restored": [], "missing": [], "warnings": warnings, "errors": [],
    }


def _validate_config(config: Any) -> None:
    if not isinstance(config, dict):
        raise AudioConfigError("Le fichier JSON ne contient pas un objet.")
    if config.get("schema") != SCHEMA_NAME:
        raise AudioConfigError("Ce fichier n'est pas une sauvegarde Audio Config Manager v2.")
    version = config.get("schemaVersion")
    if version != SCHEMA_VERSION:
        raise AudioConfigError(f"Version JSON non prise en charge : {version!r} (attendue : {SCHEMA_VERSION}).")
    if not isinstance(config.get("global"), dict) or not isinstance(config.get("applications"), list):
        raise AudioConfigError("Structure JSON incomplète ou invalide.")


def _global_import_script(global_config: dict[str, Any]) -> str:
    payload = json.dumps(global_config, ensure_ascii=False, separators=(",", ":"))
    return r"""
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$cfg = ConvertFrom-Json -InputObject """ + _powershell_literal(payload) + r"""
$r = @{ fatal = $null; restored = @(); missing = @(); warnings = @(); errors = @() }
function FindDevice($saved, $type) {
  if ($null -eq $saved) { return $null }
  $all = @(Get-AudioDevice -List | Where-Object Type -eq $type)
  $byId = @($all | Where-Object { [string]$_.ID -eq [string]$saved.id })
  if ($byId.Count -ge 1) { return $byId[0] }
  $byName = @($all | Where-Object { [string]$_.Name -ieq [string]$saved.name })
  if ($byName.Count -eq 1) { return $byName[0] }
  return $null
}
function RestoreVolumes($savedList, $type, $label) {
  foreach ($saved in @($savedList)) {
    $dev = FindDevice $saved $type
    if ($null -eq $dev) { $r.missing += "$label introuvable : $($saved.name) [$($saved.id)]"; continue }
    if ($null -ne $saved.volume) {
      try { Set-AudioDevice -ID $dev.ID -Volume ([double]$saved.volume) | Out-Null; $r.restored += "$label volume : $($dev.Name) = $($saved.volume)%" }
      catch { $r.errors += "$label volume non restauré : $($dev.Name) — $($_.Exception.Message)" }
    }
  }
}
function RestoreDefault($saved, $type, $label, [bool]$communication) {
  if ($null -eq $saved) { return }
  $dev = FindDevice $saved $type
  if ($null -eq $dev) { $r.missing += "$label introuvable : $($saved.name) [$($saved.id)]"; return }
  try {
    if ($communication) { Set-AudioDevice -ID $dev.ID -CommunicationOnly | Out-Null }
    else { Set-AudioDevice -ID $dev.ID -DefaultOnly | Out-Null }
    $r.restored += "$label : $($dev.Name)"
  } catch { $r.errors += "$label non restauré : $($dev.Name) — $($_.Exception.Message)" }
}
try {
  $m = Get-Module -ListAvailable AudioDeviceCmdlets | Sort-Object Version -Descending | Select-Object -First 1
  if ($null -eq $m) { throw "Module AudioDeviceCmdlets manquant. Exécutez : Install-Module AudioDeviceCmdlets -Scope CurrentUser" }
  Import-Module $m.Path -ErrorAction Stop
  RestoreDefault $cfg.defaults.playback 'Playback' 'Lecture par défaut' $false
  RestoreDefault $cfg.defaults.playbackCommunication 'Playback' 'Lecture communications' $true
  RestoreDefault $cfg.defaults.recording 'Recording' 'Enregistrement par défaut' $false
  RestoreDefault $cfg.defaults.recordingCommunication 'Recording' 'Enregistrement communications' $true
} catch { $r.fatal = $_.Exception.Message }
Write-Output ('ACM_RESULT:' + ($r | ConvertTo-Json -Depth 6 -Compress))
if ($r.fatal) { exit 1 }
"""


def _running_processes() -> list[dict[str, Any]]:
    import psutil
    result = []
    for proc in psutil.process_iter(["pid", "name", "exe"]):
        try:
            result.append({"pid": proc.pid, "name": proc.info.get("name"), "exe": proc.info.get("exe")})
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            continue
    return result


def _match_processes(saved: dict[str, Any], running: list[dict[str, Any]]) -> tuple[list[dict[str, Any]], str]:
    saved_path = saved.get("executablePath")
    if saved_path:
        exact = [p for p in running if p.get("exe") and os.path.normcase(p["exe"]) == os.path.normcase(saved_path)]
        if exact:
            return exact, "chemin"
    saved_name = saved.get("processName")
    if saved_name:
        matches = [p for p in running if p.get("name") and p["name"].casefold() == saved_name.casefold()]
        if matches:
            return matches, "nom"
    return [], "aucune"


def _match_route_device(saved: dict[str, Any], devices: list[Any]) -> tuple[Any | None, str]:
    device_id = saved.get("deviceId")
    if device_id:
        by_id = [d for d in devices if d.id.casefold() == str(device_id).casefold()]
        if by_id:
            return by_id[0], "identifiant"
    name = saved.get("deviceName")
    if name:
        by_name = [d for d in devices if d.name.casefold() == str(name).casefold()]
        if len(by_name) == 1:
            return by_name[0], "nom"
    return None, "aucune"


def restore_application_routes(applications: list[dict[str, Any]]) -> dict[str, list[str]]:
    import winappaudiorouter as war
    devices = {"output": war.list_output_devices(), "input": war.list_input_devices()}
    setters = {"output": war.set_app_output_device, "input": war.set_app_input_device}
    running = _running_processes()
    report = {"restored": [], "missing": [], "warnings": [], "errors": []}
    for app in applications:
        label = app.get("processName") or app.get("executablePath") or "application inconnue"
        processes, process_match = _match_processes(app, running)
        if not processes:
            report["missing"].append(f"Application non active : {label}")
            continue
        if process_match == "nom" and app.get("executablePath"):
            report["warnings"].append(f"{label} : chemin différent, correspondance par nom utilisée.")
        for flow, flow_label in (("output", "sortie"), ("input", "entrée")):
            route = app.get(flow)
            if not route:
                continue
            device, device_match = _match_route_device(route, devices[flow])
            if not device:
                report["missing"].append(f"{label} — {flow_label} introuvable : {route.get('deviceName') or route.get('deviceId')}")
                continue
            for proc in processes:
                try:
                    setters[flow](process_id=proc["pid"], device=device.id)
                    report["restored"].append(f"{label} (PID {proc['pid']}) — {flow_label} : {device.name} [{device_match}]")
                except Exception as exc:
                    report["errors"].append(f"{label} (PID {proc['pid']}) — {flow_label} non restaurée : {exc}")
    return report


def restore_endpoint_volumes(global_config: dict[str, Any]) -> dict[str, list[str]]:
    """Restore active playback and recording endpoint volumes by id, then name."""
    import winappaudiorouter as war

    report = {"restored": [], "missing": [], "warnings": [], "errors": []}
    sections = (
        ("playbackDevices", "Lecture", war.list_output_devices()),
        ("recordingDevices", "Enregistrement", war.list_input_devices()),
    )
    for section, label, devices in sections:
        for saved in global_config.get(section, []):
            if saved.get("volume") is None:
                report["warnings"].append(f"{label} sans volume sauvegardé : {saved.get('name') or saved.get('id')}")
                continue
            route_shape = {"deviceId": saved.get("id"), "deviceName": saved.get("name")}
            device, matched_by = _match_route_device(route_shape, devices)
            if not device:
                report["missing"].append(f"{label} introuvable : {saved.get('name')} [{saved.get('id')}]")
                continue
            try:
                _set_endpoint_volume(device, saved["volume"])
                report["restored"].append(f"{label} volume : {device.name} = {saved['volume']}% [{matched_by}]")
            except Exception as exc:
                report["errors"].append(f"{label} volume non restauré : {device.name} — {exc}")
    return report


def _selected_config(config: dict[str, Any], selection: dict[str, bool] | None) -> dict[str, Any]:
    """Return a filtered copy for selective restore without changing the backup."""
    if not selection:
        return config
    selected = copy.deepcopy(config)
    global_part = selected["global"]
    if not selection.get("playbackVolumes", True):
        global_part["playbackDevices"] = []
    if not selection.get("recordingVolumes", True):
        global_part["recordingDevices"] = []
    if not selection.get("defaults", True):
        global_part["defaults"] = {}
    if not selection.get("applications", True):
        selected["applications"] = []
    return selected


def verify_restoration(config: dict[str, Any]) -> list[str]:
    """Read back global defaults/volumes and report meaningful mismatches."""
    warnings: list[str] = []
    try:
        current = _run_powershell(GLOBAL_EXPORT_SCRIPT)
        current_defaults = current.get("defaults", {})
        for key, expected in config.get("global", {}).get("defaults", {}).items():
            if not expected:
                continue
            actual = current_defaults.get(key)
            if not actual or (str(actual.get("id", "")).casefold() != str(expected.get("id", "")).casefold() and str(actual.get("name", "")).casefold() != str(expected.get("name", "")).casefold()):
                warnings.append(f"Vérification : périphérique {key} différent de la sauvegarde.")
    except Exception as exc:
        warnings.append(f"Vérification après restauration impossible : {exc}")
    return warnings


def import_config(path: str | Path, selection: dict[str, bool] | None = None) -> dict[str, Any]:
    dependencies = check_dependencies()
    if dependencies["errors"]:
        raise AudioConfigError("\n".join(dependencies["errors"]))
    try:
        config = json.loads(Path(path).read_text(encoding="utf-8-sig"))
    except (OSError, json.JSONDecodeError) as exc:
        raise AudioConfigError(f"Impossible de lire le JSON : {exc}") from exc
    _validate_config(config)
    config = _selected_config(config, selection)
    global_report = _run_powershell(_global_import_script(config["global"]))
    volume_report = restore_endpoint_volumes(config["global"])
    app_report = restore_application_routes(config["applications"])
    report = {
        "operation": "import",
        "success": not global_report.get("errors") and not app_report["errors"],
        "file": str(path),
        "restored": list(global_report.get("restored", [])) + volume_report["restored"] + app_report["restored"],
        "missing": list(global_report.get("missing", [])) + volume_report["missing"] + app_report["missing"],
        "warnings": list(global_report.get("warnings", [])) + volume_report["warnings"] + app_report["warnings"],
        "errors": list(global_report.get("errors", [])) + volume_report["errors"] + app_report["errors"],
    }
    report["counts"] = {key: len(report[key]) for key in ("restored", "missing", "warnings", "errors")}
    report["warnings"].extend(verify_restoration(config))
    report["counts"] = {key: len(report[key]) for key in ("restored", "missing", "warnings", "errors")}
    return report


def list_history() -> list[Path]:
    HISTORY_DIR.mkdir(parents=True, exist_ok=True)
    return sorted(HISTORY_DIR.glob("*.json"), key=lambda p: p.stat().st_mtime, reverse=True)


def latest_release() -> dict[str, Any] | None:
    request = urllib.request.Request(
        f"https://api.github.com/repos/{GITHUB_REPOSITORY}/releases/latest",
        headers={"Accept": "application/vnd.github+json", "User-Agent": f"AudioConfigManager/{APP_VERSION}"},
    )
    try:
        with urllib.request.urlopen(request, timeout=8) as response:
            return json.loads(response.read().decode("utf-8"))
    except Exception:
        return None


def format_report(report: dict[str, Any]) -> str:
    counts = report.get("counts", {})
    lines = [
        f"Opération : {report.get('operation', '?')}",
        f"Fichier : {report.get('file', '')}",
        "",
        "Résumé : " + ", ".join(f"{key}={value}" for key, value in counts.items()),
    ]
    titles = (("restored", "Restaurés"), ("missing", "Introuvables / non actifs"), ("warnings", "Avertissements"), ("errors", "Erreurs"))
    for key, title in titles:
        items = report.get(key, [])
        if items:
            lines.extend(["", f"{title} ({len(items)})"])
            lines.extend(f"  • {item}" for item in items)
    return "\n".join(lines)


class AudioConfigGUI:
    BG = "#080e20"
    PANEL = "#101b35"
    CARD = "#17233f"
    CARD_HOVER = "#1c2a4b"
    BORDER = "#2b3d64"
    FG = "#f7f8ff"
    MUTED = "#9db1de"
    DIM = "#6177a8"
    GREEN = "#4bd4a7"
    BLUE = "#6b88f7"
    YELLOW = "#f2c86b"
    RED = "#f06f82"

    def __init__(self, root: tk.Tk):
        self.root = root
        root.title("Audio Config Manager V6")
        root.geometry("1128x720")
        root.minsize(900, 620)
        root.configure(bg=self.BG)
        self.default_folder = str(Path.home() / "Documents")
        self.last_report: dict[str, Any] | None = None
        self.action_cards: list[tk.Frame] = []
        self._build_ui()
        root.after(150, self._initial_check)

    def _build_ui(self) -> None:
        shell = tk.Frame(self.root, bg=self.BG)
        shell.pack(fill=tk.BOTH, expand=True, padx=36, pady=(32, 28))

        header = tk.Frame(shell, bg=self.BG, height=92)
        header.pack(fill=tk.X)
        header.pack_propagate(False)
        icon = tk.Canvas(header, width=60, height=60, bg=self.BG, highlightthickness=0)
        icon.pack(side=tk.LEFT, pady=4)
        icon.create_rectangle(1, 1, 59, 59, fill=self.BLUE, outline=self.BLUE)
        icon.create_polygon(18, 25, 27, 25, 38, 16, 38, 44, 27, 35, 18, 35, fill=self.BG)
        icon.create_arc(34, 19, 51, 41, start=-55, extent=110, style=tk.ARC, width=2, outline=self.BG)
        heading = tk.Frame(header, bg=self.BG)
        heading.pack(side=tk.LEFT, padx=(20, 0), pady=(2, 0))
        tk.Label(heading, text="Audio Config Manager", font=("Segoe UI", 24, "bold"), bg=self.BG, fg=self.FG).pack(anchor="w")
        tk.Label(heading, text="Sauvegarde et restaure ta configuration audio Windows", font=("Segoe UI", 10), bg=self.BG, fg=self.MUTED).pack(anchor="w", pady=(6, 0))
        version = tk.Label(header, text="V6", font=("Segoe UI", 10, "bold"), bg="#15274c", fg="#afc0ff", padx=17, pady=8, cursor="hand2")
        version.pack(side=tk.RIGHT, anchor="n", pady=0)
        version.bind("<Button-1>", lambda _e: self._check_update())

        hero = tk.Frame(shell, bg=self.PANEL, highlightbackground=self.BORDER, highlightthickness=1, height=145)
        hero.pack(fill=tk.X, pady=(0, 24))
        hero.pack_propagate(False)
        hero_text = tk.Frame(hero, bg=self.PANEL)
        hero_text.pack(side=tk.LEFT, fill=tk.BOTH, expand=True, padx=26, pady=19)
        tk.Label(hero_text, text="Ta configuration audio, en sécurité.", font=("Segoe UI", 17, "bold"), bg=self.PANEL, fg=self.FG).pack(anchor="w")
        tk.Label(hero_text, text="Une sauvegarde portable pour retrouver rapidement tes sorties, micros, volumes et choix par application.", font=("Segoe UI", 9), bg=self.PANEL, fg=self.MUTED).pack(anchor="w", pady=(6, 14))
        tags = tk.Frame(hero_text, bg=self.PANEL)
        tags.pack(anchor="w")
        for label in ("Sorties audio", "Microphones", "Volumes", "Par application"):
            chip = tk.Frame(tags, bg="#17294e", padx=10, pady=5)
            chip.pack(side=tk.LEFT, padx=(0, 8))
            tk.Label(chip, text="•", font=("Segoe UI", 9, "bold"), bg="#17294e", fg=self.GREEN).pack(side=tk.LEFT)
            tk.Label(chip, text=label, font=("Segoe UI", 8, "bold"), bg="#17294e", fg="#c4d2fa").pack(side=tk.LEFT, padx=(5, 0))
        note = tk.Canvas(hero, width=100, height=90, bg=self.PANEL, highlightthickness=0)
        note.pack(side=tk.RIGHT, padx=30)
        note.create_text(50, 43, text="♫", font=("Segoe UI Symbol", 40, "bold"), fill="#29457e")

        tk.Label(shell, text="Que veux-tu faire ?", font=("Segoe UI", 13, "bold"), bg=self.BG, fg=self.FG).pack(anchor="w", pady=(0, 14))
        actions = tk.Frame(shell, bg=self.BG)
        actions.pack(fill=tk.X)
        actions.columnconfigure(0, weight=1, uniform="actions")
        actions.columnconfigure(1, weight=1, uniform="actions")
        self.export_card = self._action_card(actions, 0, "⇧", self.GREEN, "Exporter la configuration", "Sauvegarde les périphériques, les volumes, les choix par application\net les périphériques par défaut.", self._choose_export)
        self.import_card = self._action_card(actions, 1, "⇩", self.BLUE, "Importer une configuration", "Restaure une sauvegarde JSON et remet en place tous les réglages audio.", self._choose_import)

        bottom = tk.Frame(shell, bg="#0e172d", highlightbackground=self.BORDER, highlightthickness=1, height=134)
        bottom.pack(fill=tk.X, pady=(20, 0))
        bottom.pack_propagate(False)
        path_row = tk.Frame(bottom, bg="#0e172d")
        path_row.pack(fill=tk.X, padx=22, pady=(18, 13))
        path_text = tk.Frame(path_row, bg="#0e172d")
        path_text.pack(side=tk.LEFT, fill=tk.X, expand=True)
        tk.Label(path_text, text="DOSSIER PAR DÉFAUT", font=("Segoe UI", 8, "bold"), bg="#0e172d", fg="#7894cd").pack(anchor="w")
        self.path_label = tk.Label(path_text, text=self.default_folder, font=("Segoe UI", 9), bg="#0e172d", fg=self.FG)
        self.path_label.pack(anchor="w", pady=(8, 0))
        change = tk.Label(path_row, text="Modifier", font=("Segoe UI", 9, "bold"), bg="#0e172d", fg=self.BLUE, cursor="hand2")
        change.pack(side=tk.RIGHT, padx=3, pady=(13, 0))
        change.bind("<Button-1>", self._choose_folder)
        tk.Frame(bottom, height=1, bg="#263655").pack(fill=tk.X, padx=20)
        status_row = tk.Frame(bottom, bg="#0e172d")
        status_row.pack(fill=tk.X, padx=22, pady=14)
        self.status_dot = tk.Label(status_row, text="●", font=("Segoe UI", 8), bg="#0e172d", fg=self.DIM)
        self.status_dot.pack(side=tk.LEFT)
        self.status = tk.Label(status_row, text="Vérification des dépendances…", font=("Segoe UI", 9, "bold"), bg="#0e172d", fg=self.MUTED)
        self.status.pack(side=tk.LEFT, padx=(10, 0))
        self.report_link = tk.Label(status_row, text="Voir le rapport", font=("Segoe UI", 9, "bold"), bg="#0e172d", fg=self.BLUE, cursor="hand2")
        self.report_link.pack(side=tk.RIGHT)
        self.report_link.bind("<Button-1>", lambda _e: self._show_report())
        self.report_link.pack_forget()
        self.history_link = tk.Label(status_row, text="Historique", font=("Segoe UI", 9, "bold"), bg="#0e172d", fg=self.BLUE, cursor="hand2")
        self.history_link.pack(side=tk.RIGHT, padx=(0, 18))
        self.history_link.bind("<Button-1>", lambda _e: self._show_history())
        self.install_link = tk.Label(status_row, text="Installer les dépendances", font=("Segoe UI", 9, "bold"), bg="#0e172d", fg=self.YELLOW, cursor="hand2")
        self.install_link.bind("<Button-1>", lambda _e: self._install_dependencies())
        self.dependency_label = tk.Label(status_row, text="AudioDeviceCmdlets est requis pour les périphériques globaux", font=("Segoe UI", 8), bg="#0e172d", fg=self.DIM)
        self.dependency_label.pack(side=tk.RIGHT, padx=(0, 18))

    def _build_ui(self) -> None:
        """V6.1 dashboard layout inspired by the supplied visual reference."""
        self.root.geometry("1120x840")
        self.root.minsize(1000, 720)
        app = tk.Frame(self.root, bg=self.BG)
        app.pack(fill=tk.BOTH, expand=True)

        sidebar = tk.Frame(app, bg="#070d1a", width=232, highlightbackground="#1d2942", highlightthickness=1)
        sidebar.pack(side=tk.LEFT, fill=tk.Y)
        sidebar.pack_propagate(False)
        brand = tk.Frame(sidebar, bg="#070d1a")
        brand.pack(fill=tk.X, padx=20, pady=(30, 28))
        logo = tk.Canvas(brand, width=50, height=50, bg="#070d1a", highlightthickness=0)
        logo.pack(side=tk.LEFT)
        logo.create_oval(2, 2, 48, 48, fill=self.BLUE, outline=self.BLUE)
        for x, h in ((16, 12), (21, 20), (26, 27), (31, 18), (36, 10)):
            logo.create_line(x, 25-h//2, x, 25+h//2, fill="white", width=2)
        brand_text = tk.Frame(brand, bg="#070d1a")
        brand_text.pack(side=tk.LEFT, padx=(12, 0))
        tk.Label(brand_text, text="Audio Config", font=("Segoe UI", 12, "bold"), bg="#070d1a", fg=self.FG).pack(anchor="w")
        tk.Label(brand_text, text="Manager", font=("Segoe UI", 12, "bold"), bg="#070d1a", fg=self.FG).pack(anchor="w")
        tk.Label(brand_text, text="Windows 10 & 11", font=("Segoe UI", 8), bg="#070d1a", fg=self.MUTED).pack(anchor="w", pady=(4, 0))

        nav = tk.Frame(sidebar, bg="#171532", highlightbackground="#8b5cf6", highlightthickness=1, cursor="hand2")
        nav.pack(fill=tk.X, padx=18, pady=(0, 10))
        tk.Label(nav, text="≋  Tableau de bord", font=("Segoe UI", 10, "bold"), bg="#171532", fg=self.FG, padx=14, pady=14).pack(anchor="w")
        settings = tk.Label(sidebar, text="⚙  Paramètres", font=("Segoe UI Symbol", 10, "bold"), bg="#070d1a", fg="#7785a5", padx=30, pady=12, cursor="hand2")
        settings.pack(fill=tk.X, anchor="w")
        settings.bind("<Button-1>", self._choose_folder)

        side_bottom = tk.Frame(sidebar, bg="#101827", highlightbackground="#263451", highlightthickness=1)
        side_bottom.pack(side=tk.BOTTOM, fill=tk.X, padx=18, pady=(0, 18))
        self.status_dot = tk.Label(side_bottom, text="●", font=("Segoe UI", 9), bg="#101827", fg=self.DIM)
        self.status_dot.pack(side=tk.LEFT, padx=(12, 8), pady=14)
        self.status = tk.Label(side_bottom, text="Vérification…", font=("Segoe UI", 9, "bold"), bg="#101827", fg=self.MUTED)
        self.status.pack(side=tk.LEFT, pady=14)
        tk.Label(sidebar, text="Version 6.1.0 portable", font=("Segoe UI", 7), bg="#070d1a", fg="#4e5b78").pack(side=tk.BOTTOM, pady=8)

        main = tk.Frame(app, bg="#080d1d")
        main.pack(side=tk.LEFT, fill=tk.BOTH, expand=True)
        content = tk.Frame(main, bg="#080d1d")
        content.pack(fill=tk.BOTH, expand=True, padx=46, pady=(38, 28))
        title_row = tk.Frame(content, bg="#080d1d")
        title_row.pack(fill=tk.X)
        title_text = tk.Frame(title_row, bg="#080d1d")
        title_text.pack(side=tk.LEFT)
        tk.Label(title_text, text="S A U V E G A R D E   A U D I O", font=("Segoe UI", 7, "bold"), bg="#080d1d", fg="#8b5cf6").pack(anchor="w")
        tk.Label(title_text, text="Retrouvez votre son,", font=("Segoe UI", 25, "bold"), bg="#080d1d", fg=self.FG).pack(anchor="w", pady=(10, 0))
        tk.Label(title_text, text="exactement comme avant.", font=("Segoe UI", 25), bg="#080d1d", fg="#a9b9df").pack(anchor="w")
        tk.Label(title_text, text="Profils rapides, restauration contrôlée et dossier de sauvegarde personnalisable.", font=("Segoe UI", 10), bg="#080d1d", fg=self.MUTED).pack(anchor="w", pady=(8, 0))
        refresh = tk.Label(title_row, text="⟳", font=("Segoe UI Symbol", 20), bg="#101827", fg=self.MUTED, padx=12, pady=8, cursor="hand2")
        refresh.pack(side=tk.RIGHT, anchor="n")
        refresh.bind("<Button-1>", lambda _e: self._refresh_overview())

        overview = tk.Frame(content, bg="#101728", highlightbackground="#2a3857", highlightthickness=1)
        overview.pack(fill=tk.X, pady=(28, 16))
        overview_head = tk.Frame(overview, bg="#101728")
        overview_head.pack(fill=tk.X, padx=22, pady=(18, 10))
        head_left = tk.Frame(overview_head, bg="#101728")
        head_left.pack(side=tk.LEFT)
        tk.Label(head_left, text="S Y S T È M E   A C T U E L", font=("Segoe UI", 7, "bold"), bg="#101728", fg="#8b5cf6").pack(anchor="w")
        tk.Label(head_left, text="Vue d’ensemble", font=("Segoe UI", 13, "bold"), bg="#101728", fg=self.FG).pack(anchor="w", pady=(7, 0))
        self.ready_badge = tk.Label(overview_head, text="●  Analyse…", font=("Segoe UI", 8, "bold"), bg="#102a28", fg=self.GREEN, padx=12, pady=7)
        self.ready_badge.pack(side=tk.RIGHT)
        summaries = tk.Frame(overview, bg="#101728")
        summaries.pack(fill=tk.X, padx=22, pady=(4, 22))
        for index in range(3):
            summaries.columnconfigure(index, weight=1, uniform="summary")
        self.output_summary = self._summary_card(summaries, 0, "◖", "Sortie par défaut", "Détection…", "Périphériques de lecture", "#261e52", "#a78bfa")
        self.input_summary = self._summary_card(summaries, 1, "♩", "Entrée par défaut", "Détection…", "Périphériques d’entrée", "#12334b", "#58c7ec")
        self.path_summary = self._summary_card(summaries, 2, "□", "Dossier des profils", Path(self.default_folder).name, self.default_folder, "#123c39", self.GREEN)
        self.path_label = self.path_summary

        actions = tk.Frame(content, bg="#080d1d")
        actions.pack(fill=tk.X)
        actions.columnconfigure(0, weight=1, uniform="action")
        actions.columnconfigure(1, weight=1, uniform="action")
        save_card = tk.Frame(actions, bg="#25214d", highlightbackground="#574e92", highlightthickness=1)
        save_card.grid(row=0, column=0, sticky="nsew", padx=(0, 8))
        tk.Label(save_card, text="▣", font=("Segoe UI Symbol", 19), bg="#313052", fg=self.FG, padx=10, pady=6).pack(anchor="w", padx=22, pady=(20, 10))
        tk.Label(save_card, text="Sauvegarder", font=("Segoe UI", 16, "bold"), bg="#25214d", fg=self.FG).pack(anchor="w", padx=22)
        tk.Label(save_card, text="Capture les appareils, volumes et choix par application\ndans un profil réutilisable.", justify=tk.LEFT, font=("Segoe UI", 8), bg="#25214d", fg=self.MUTED).pack(anchor="w", padx=22, pady=(10, 12))
        save_row = tk.Frame(save_card, bg="#25214d")
        save_row.pack(fill=tk.X, padx=22, pady=(0, 20))
        self.profile_name = tk.Entry(save_row, font=("Segoe UI", 10), bg="#12152b", fg=self.FG, insertbackground=self.FG, relief=tk.FLAT)
        self.profile_name.insert(0, "Mon profil audio")
        self.profile_name.pack(side=tk.LEFT, fill=tk.X, expand=True, ipady=10)
        self.export_card = tk.Button(save_row, text="▣  Enregistrer", command=self._choose_export, font=("Segoe UI", 10, "bold"), bg="#8057f5", fg="white", activebackground="#936fff", border=0, padx=16, pady=10, cursor="hand2")
        self.export_card.pack(side=tk.RIGHT, padx=(8, 0))

        restore_card = tk.Frame(actions, bg="#123145", highlightbackground="#28516b", highlightthickness=1)
        restore_card.grid(row=0, column=1, sticky="nsew", padx=(8, 0))
        tk.Label(restore_card, text="◴", font=("Segoe UI Symbol", 19), bg="#263b4d", fg=self.FG, padx=10, pady=6).pack(anchor="w", padx=22, pady=(20, 10))
        tk.Label(restore_card, text="Restaurer", font=("Segoe UI", 16, "bold"), bg="#123145", fg=self.FG).pack(anchor="w", padx=22)
        tk.Label(restore_card, text="Prévisualisez et choisissez précisément les éléments\nà remettre en place.", justify=tk.LEFT, font=("Segoe UI", 8), bg="#123145", fg=self.MUTED).pack(anchor="w", padx=22, pady=(10, 12))
        self.import_card = tk.Button(restore_card, text="↥     Importer un JSON                         ›", command=self._choose_import, font=("Segoe UI", 10), bg="#10283a", fg=self.FG, activebackground="#173a52", highlightbackground="#39728f", highlightthickness=1, border=0, pady=11, cursor="hand2")
        self.import_card.pack(fill=tk.X, padx=22, pady=(0, 20))
        self.action_cards = [self.export_card, self.import_card]

        history_panel = tk.Frame(content, bg="#101728", highlightbackground="#2a3857", highlightthickness=1)
        history_panel.pack(fill=tk.X, pady=(16, 0))
        history_text = tk.Frame(history_panel, bg="#101728")
        history_text.pack(side=tk.LEFT, padx=22, pady=16)
        tk.Label(history_text, text="P R O F I L S   &   H I S T O R I Q U E", font=("Segoe UI", 7, "bold"), bg="#101728", fg="#8b5cf6").pack(anchor="w")
        self.history_count = tk.Label(history_text, text="Vos sauvegardes", font=("Segoe UI", 13, "bold"), bg="#101728", fg=self.FG)
        self.history_count.pack(anchor="w", pady=(6, 0))
        self.history_link = tk.Button(history_panel, text="Ouvrir", command=self._show_history, font=("Segoe UI", 9, "bold"), bg="#182239", fg=self.FG, border=0, padx=16, pady=9, cursor="hand2")
        self.history_link.pack(side=tk.RIGHT, padx=22)
        self.report_link = tk.Button(history_panel, text="Rapport", command=self._show_report, font=("Segoe UI", 9, "bold"), bg="#182239", fg=self.FG, border=0, padx=16, pady=9, cursor="hand2")
        self.report_link.pack(side=tk.RIGHT)
        self.dependency_label = tk.Label(history_panel, text="", bg="#101728", fg=self.DIM)
        self.install_link = tk.Button(history_panel, text="Installer AudioDeviceCmdlets", command=self._install_dependencies, font=("Segoe UI", 8, "bold"), bg=self.YELLOW, fg=self.BG, border=0)

    def _summary_card(self, parent: tk.Widget, column: int, symbol: str, label: str, value: str, detail: str, icon_bg: str, icon_fg: str) -> tk.Label:
        card = tk.Frame(parent, bg="#0e1628", highlightbackground="#263451", highlightthickness=1)
        card.grid(row=0, column=column, sticky="nsew", padx=(0, 7) if column < 2 else (7, 0))
        tk.Label(card, text=symbol, font=("Segoe UI Symbol", 17), bg=icon_bg, fg=icon_fg, padx=10, pady=8).pack(side=tk.LEFT, padx=14, pady=16)
        text = tk.Frame(card, bg="#0e1628")
        text.pack(side=tk.LEFT, fill=tk.BOTH, expand=True, pady=14)
        tk.Label(text, text=label, font=("Segoe UI", 7), bg="#0e1628", fg=self.MUTED).pack(anchor="w")
        value_label = tk.Label(text, text=value, font=("Segoe UI", 9, "bold"), bg="#0e1628", fg=self.FG, anchor="w")
        value_label.pack(fill=tk.X, pady=(4, 0))
        value_label.detail_label = tk.Label(text, text=detail, font=("Segoe UI", 7), bg="#0e1628", fg=self.DIM, anchor="w")
        value_label.detail_label.pack(fill=tk.X, pady=(4, 0))
        return value_label

    def _refresh_overview(self) -> None:
        self.ready_badge.config(text="●  Analyse…", fg=self.YELLOW)
        def load() -> dict[str, Any]:
            return _run_powershell(GLOBAL_EXPORT_SCRIPT)
        def done(data: dict[str, Any]) -> None:
            defaults = data.get("defaults", {})
            playback = defaults.get("playback") or {}
            recording = defaults.get("recording") or {}
            self.output_summary.config(text=playback.get("name") or "Aucune sortie")
            self.output_summary.detail_label.config(text=f"{len(data.get('playbackDevices', []))} sorties détectées")
            self.input_summary.config(text=recording.get("name") or "Aucune entrée")
            self.input_summary.detail_label.config(text=f"{len(data.get('recordingDevices', []))} entrées détectées")
            self.history_count.config(text=f"{len(list_history())} sauvegarde(s)")
            self.ready_badge.config(text="●  Système prêt", fg=self.GREEN)
        self._background(load, done)

    def _action_card(self, parent: tk.Widget, column: int, symbol: str, accent: str, title: str, description: str, command: Callable[[], None]) -> tk.Frame:
        card = tk.Frame(parent, bg=self.CARD, highlightbackground=self.BORDER, highlightthickness=1, height=140, cursor="hand2")
        card.grid(row=0, column=column, sticky="nsew", padx=(0, 9) if column == 0 else (9, 0))
        card.grid_propagate(False)
        badge = tk.Label(card, text=symbol, font=("Segoe UI Symbol", 22), bg=accent, fg=self.BG, width=2, height=1, padx=6, pady=9, cursor="hand2")
        badge.pack(side=tk.LEFT, padx=(20, 22), pady=24)
        text = tk.Frame(card, bg=self.CARD, cursor="hand2")
        text.pack(side=tk.LEFT, fill=tk.BOTH, expand=True, pady=34)
        title_label = tk.Label(text, text=title, font=("Segoe UI", 14, "bold"), bg=self.CARD, fg=self.FG, cursor="hand2")
        title_label.pack(anchor="w")
        desc_label = tk.Label(text, text=description, justify=tk.LEFT, font=("Segoe UI", 8), bg=self.CARD, fg=self.MUTED, cursor="hand2")
        desc_label.pack(anchor="w", pady=(5, 0))
        arrow = tk.Label(card, text="→", font=("Segoe UI Symbol", 18), bg=self.CARD, fg=accent, cursor="hand2")
        arrow.pack(side=tk.RIGHT, padx=22, pady=(73, 0))
        widgets = (card, badge, text, title_label, desc_label, arrow)
        for widget in widgets:
            widget.bind("<Button-1>", lambda _e, cmd=command: cmd())
            widget.bind("<Enter>", lambda _e, c=card, t=text, tl=title_label, dl=desc_label, a=arrow: self._card_color(c, t, tl, dl, a, self.CARD_HOVER))
            widget.bind("<Leave>", lambda _e, c=card, t=text, tl=title_label, dl=desc_label, a=arrow: self._card_color(c, t, tl, dl, a, self.CARD))
        self.action_cards.append(card)
        return card

    @staticmethod
    def _card_color(card: tk.Frame, text: tk.Frame, title: tk.Label, desc: tk.Label, arrow: tk.Label, color: str) -> None:
        for widget in (card, text, title, desc, arrow):
            widget.configure(bg=color)

    def _set_details(self, text: str) -> None:
        self._latest_details = text

    def _set_status(self, text: str, color: str) -> None:
        self.status.config(text=text, fg=color)
        self.status_dot.config(fg=color)

    def _initial_check(self) -> None:
        def done(checks: dict[str, Any]) -> None:
            if checks["errors"]:
                self._set_details("Dépendances manquantes :\n\n" + "\n".join(f"• {e}" for e in checks["errors"]))
                self._set_status("Dépendances manquantes", self.RED)
                self._set_cards_enabled(False)
                self.install_link.pack(side=tk.RIGHT, padx=(0, 18))
                self.report_link.pack(side=tk.RIGHT)
            else:
                self._set_details(f"Prêt. AudioDeviceCmdlets {checks.get('audioDeviceCmdletsVersion', '?')} détecté.\nLe moteur de routage par application est disponible.")
                self._set_status("Prêt à sauvegarder ou restaurer", self.GREEN)
                self.dependency_label.config(text=f"AudioDeviceCmdlets {checks.get('audioDeviceCmdletsVersion', '?')} détecté")
                self._refresh_overview()
        self._background(check_dependencies, done)

    def _set_cards_enabled(self, enabled: bool) -> None:
        state = "hand2" if enabled else "arrow"
        for card in self.action_cards:
            card.config(cursor=state)

    def _choose_folder(self, _event: Any = None) -> None:
        folder = filedialog.askdirectory(title="Dossier par défaut", initialdir=self.default_folder)
        if folder:
            self.default_folder = folder
            self.path_label.config(text=folder)

    def _choose_export(self) -> None:
        profile = self.profile_name.get().strip() if hasattr(self, "profile_name") else "audio-config"
        safe_name = "".join(c if c.isalnum() or c in "-_ " else "-" for c in profile).strip() or "audio-config"
        path = filedialog.asksaveasfilename(title="Exporter la configuration audio", initialdir=self.default_folder, initialfile=f"{safe_name}.json", defaultextension=".json", filetypes=[("Configuration JSON", "*.json")])
        if path:
            self._start_operation(lambda: export_config(path), "Export en cours…")

    def _choose_import(self) -> None:
        path = filedialog.askopenfilename(title="Restaurer une configuration audio", initialdir=self.default_folder, filetypes=[("Configuration JSON v2", "*.json")])
        if path:
            self._show_restore_preview(path)

    def _show_restore_preview(self, path: str) -> None:
        try:
            config = json.loads(Path(path).read_text(encoding="utf-8-sig"))
            _validate_config(config)
        except Exception as exc:
            self._operation_error(exc)
            return
        window = tk.Toplevel(self.root)
        window.title("Aperçu de la restauration V6")
        window.geometry("680x500")
        window.configure(bg=self.BG)
        window.transient(self.root)
        window.grab_set()
        tk.Label(window, text="Choisir les éléments à restaurer", font=("Segoe UI", 17, "bold"), bg=self.BG, fg=self.FG).pack(anchor="w", padx=28, pady=(25, 6))
        tk.Label(window, text=Path(path).name, font=("Segoe UI", 9), bg=self.BG, fg=self.MUTED).pack(anchor="w", padx=28)
        options = tk.Frame(window, bg=self.PANEL, highlightbackground=self.BORDER, highlightthickness=1)
        options.pack(fill=tk.X, padx=28, pady=22)
        values = {key: tk.BooleanVar(value=True) for key in ("defaults", "playbackVolumes", "recordingVolumes", "applications")}
        rows = (
            ("defaults", "Périphériques par défaut", len(config["global"].get("defaults", {}))),
            ("playbackVolumes", "Volumes des sorties", len(config["global"].get("playbackDevices", []))),
            ("recordingVolumes", "Volumes des microphones", len(config["global"].get("recordingDevices", []))),
            ("applications", "Routage par application", len(config.get("applications", []))),
        )
        for key, label, count in rows:
            tk.Checkbutton(options, text=f"  {label}   ({count})", variable=values[key], anchor="w", font=("Segoe UI", 11, "bold"), bg=self.PANEL, activebackground=self.PANEL, selectcolor=self.CARD, fg=self.FG, activeforeground=self.FG, padx=18, pady=13).pack(fill=tk.X)
        meta = config.get("metadata", {})
        tk.Label(window, text=f"Créée le {meta.get('createdAt', '?')} • {meta.get('computerName', '?')}", font=("Segoe UI", 8), bg=self.BG, fg=self.DIM).pack(anchor="w", padx=28)
        buttons = tk.Frame(window, bg=self.BG)
        buttons.pack(fill=tk.X, padx=28, pady=24)
        tk.Button(buttons, text="Annuler", command=window.destroy, font=("Segoe UI", 10, "bold"), bg=self.CARD, fg=self.FG, activebackground=self.CARD_HOVER, border=0, padx=22, pady=10).pack(side=tk.RIGHT)
        def confirm() -> None:
            selection = {key: value.get() for key, value in values.items()}
            if not any(selection.values()):
                messagebox.showwarning("Sélection vide", "Choisis au moins une catégorie.", parent=window)
                return
            window.destroy()
            self._start_operation(lambda: import_config(path, selection), "Restauration et vérification en cours…")
        tk.Button(buttons, text="Restaurer la sélection", command=confirm, font=("Segoe UI", 10, "bold"), bg=self.BLUE, fg=self.BG, activebackground=self.BLUE, border=0, padx=22, pady=10).pack(side=tk.RIGHT, padx=(0, 10))

    def _show_history(self) -> None:
        window = tk.Toplevel(self.root)
        window.title("Historique des sauvegardes")
        window.geometry("760x480")
        window.configure(bg=self.BG)
        tk.Label(window, text="Historique des sauvegardes", font=("Segoe UI", 17, "bold"), bg=self.BG, fg=self.FG).pack(anchor="w", padx=24, pady=(22, 14))
        container = tk.Frame(window, bg=self.BG)
        container.pack(fill=tk.BOTH, expand=True, padx=24, pady=(0, 20))
        history = list_history()
        if not history:
            tk.Label(container, text="Aucune sauvegarde dans l’historique.", font=("Segoe UI", 10), bg=self.BG, fg=self.MUTED).pack(anchor="w")
            return
        listbox = tk.Listbox(container, bg=self.PANEL, fg=self.FG, selectbackground=self.BLUE, selectforeground=self.BG, relief=tk.FLAT, font=("Consolas", 10), activestyle="none")
        listbox.pack(fill=tk.BOTH, expand=True)
        for item in history:
            listbox.insert(tk.END, f"{item.stem}   •   {item.stat().st_size // 1024} Ko")
        def restore_selected() -> None:
            selected = listbox.curselection()
            if selected:
                chosen = history[selected[0]]
                window.destroy()
                self._show_restore_preview(str(chosen))
        tk.Button(container, text="Prévisualiser et restaurer", command=restore_selected, font=("Segoe UI", 10, "bold"), bg=self.GREEN, fg=self.BG, border=0, padx=18, pady=10).pack(anchor="e", pady=(12, 0))

    def _install_dependencies(self) -> None:
        if not messagebox.askyesno("Installer AudioDeviceCmdlets", "Installer le module AudioDeviceCmdlets pour l’utilisateur actuel ?"):
            return
        command = "Install-PackageProvider NuGet -Force -Scope CurrentUser; Install-Module AudioDeviceCmdlets -Force -Scope CurrentUser -AllowClobber"
        def install() -> dict[str, Any]:
            if not POWERSHELL:
                raise AudioConfigError("Windows PowerShell est introuvable.")
            flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
            result = subprocess.run([POWERSHELL, "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", command], capture_output=True, text=True, timeout=240, creationflags=flags)
            if result.returncode:
                raise AudioConfigError(result.stderr.strip() or "Installation impossible.")
            return check_dependencies()
        self._set_status("Installation des dépendances…", self.YELLOW)
        def done(checks: dict[str, Any]) -> None:
            if checks.get("errors"):
                self._operation_error(AudioConfigError("\n".join(checks["errors"])))
            else:
                self.install_link.pack_forget()
                self._set_cards_enabled(True)
                self._set_status("Dépendances installées", self.GREEN)
        self._background(install, done)

    def _check_update(self) -> None:
        self._set_status("Recherche d’une mise à jour…", self.YELLOW)
        def done(release: dict[str, Any] | None) -> None:
            if not release:
                self._set_status("Impossible de vérifier les mises à jour", self.YELLOW)
                return
            tag = str(release.get("tag_name", "")).lstrip("vV")
            if tag and tag != APP_VERSION:
                if messagebox.askyesno("Mise à jour disponible", f"La version {tag} est disponible. Ouvrir la page de téléchargement ?"):
                    webbrowser.open(release.get("html_url") or f"https://github.com/{GITHUB_REPOSITORY}/releases")
            else:
                messagebox.showinfo("À jour", f"Audio Config Manager {APP_VERSION} est à jour.")
            self._set_status("Prêt à sauvegarder ou restaurer", self.GREEN)
        self._background(latest_release, done)

    def _background(self, action: Callable[[], Any], done: Callable[[Any], None]) -> None:
        def worker() -> None:
            try:
                value = action()
                self.root.after(0, lambda: done(value))
            except Exception as exc:
                self.root.after(0, lambda: self._operation_error(exc))
        threading.Thread(target=worker, daemon=True).start()

    def _start_operation(self, action: Callable[[], dict[str, Any]], status: str) -> None:
        self._set_cards_enabled(False)
        self._set_status(status, self.YELLOW)
        self._set_details(status)
        self._background(action, self._operation_done)

    def _operation_done(self, report: dict[str, Any]) -> None:
        self.last_report = report
        self._set_cards_enabled(True)
        self.report_link.pack(side=tk.RIGHT)
        self.dependency_label.pack_forget()
        text = format_report(report)
        self._set_details(text)
        incomplete = bool(report.get("missing") or report.get("warnings") or report.get("errors"))
        self._set_status("Terminé avec réserves" if incomplete else "Terminé avec succès", self.YELLOW if incomplete else self.GREEN)
        if incomplete:
            messagebox.showwarning("Opération terminée avec réserves", "Certains éléments n'ont pas été traités. Consultez le rapport détaillé dans la fenêtre.")
        else:
            messagebox.showinfo("Succès", "L'opération s'est terminée correctement.")

    def _operation_error(self, exc: Exception) -> None:
        self._set_cards_enabled(True)
        self._set_status("Échec", self.RED)
        detail = f"{type(exc).__name__}: {exc}"
        self._set_details(detail)
        self.report_link.pack(side=tk.RIGHT)
        messagebox.showerror("Erreur", detail)

    def _show_report(self) -> None:
        text = format_report(self.last_report) if self.last_report else getattr(self, "_latest_details", "Aucun rapport disponible.")
        window = tk.Toplevel(self.root)
        window.title("Rapport détaillé")
        window.geometry("780x500")
        window.configure(bg=self.BG)
        def save_report() -> None:
            destination = filedialog.asksaveasfilename(parent=window, title="Exporter le rapport", initialfile="audio-config-report.txt", defaultextension=".txt", filetypes=[("Rapport texte", "*.txt")])
            if destination:
                Path(destination).write_text(text, encoding="utf-8")
        tk.Button(window, text="Exporter le rapport", command=save_report, font=("Segoe UI", 9, "bold"), bg=self.BLUE, fg=self.BG, border=0, padx=16, pady=8).pack(side=tk.BOTTOM, anchor="e", padx=18, pady=(0, 16))
        viewer = ScrolledText(window, bg="#0e172d", fg=self.FG, insertbackground=self.FG, relief=tk.FLAT, font=("Consolas", 9), padx=16, pady=16)
        viewer.pack(fill=tk.BOTH, expand=True, padx=18, pady=18)
        viewer.insert(tk.END, text)
        viewer.config(state=tk.DISABLED)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=APP_NAME)
    parser.add_argument("--export", metavar="JSON")
    parser.add_argument("--import", dest="import_file", metavar="JSON")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)
    try:
        if args.check:
            print(json.dumps(check_dependencies(), ensure_ascii=False, indent=2))
        elif args.export:
            print(json.dumps(export_config(args.export), ensure_ascii=False, indent=2))
        elif args.import_file:
            print(json.dumps(import_config(args.import_file), ensure_ascii=False, indent=2))
        else:
            root = tk.Tk()
            AudioConfigGUI(root)
            root.mainloop()
        return 0
    except Exception as exc:
        print(f"ERREUR: {exc}", file=sys.stderr)
        if os.environ.get("ACM_DEBUG"):
            traceback.print_exc()
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
