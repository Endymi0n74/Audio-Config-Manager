# Audio Config Manager

[![Build status](https://github.com/Endymi0n74/Audio-Config-Manager/actions/workflows/build.yml/badge.svg)](https://github.com/Endymi0n74/Audio-Config-Manager/actions/workflows/build.yml)
[![Télécharger](https://img.shields.io/badge/T%C3%A9l%C3%A9charger-Windows-2ea44f?style=flat-square)](https://github.com/Endymi0n74/Audio-Config-Manager/releases/latest)

[🇫🇷 Français](README.md) · **🇬🇧 English**

Windows audio configuration management: default devices
(output/input), volumes and **per-application audio routing**, saved
in **JSON profiles**.

![Overview](docs/screens/overview.png)

## Features

- **Profiles**: back up and restore the default devices
  (output/input) and volumes; preview before restoring; JSON
  import/export; automatic backups (before restoring, at startup, on
  device change) with retention. Change detection is
  **event-native** (`IMMNotificationClient`, COM subscription): zero
  background process, instant detection.
- **Per-application routing**: choose the output and input device of
  each application (e.g. *Xbox → another sound card*), via the
  internal Windows API (`AudioPolicyConfig`).
- **Overview**: defaults, volumes and device counts read
  **directly in COM** (without PowerShell) in ~10 ms; one-click
  installation of the AudioDeviceCmdlets module (detection by file check).
- **Interface**: Mica, automatic light/dark theme, system
  accent color, version tracked automatically (no hardcoded number).

![Applications](docs/screens/apps.png)

## Download

Portable executable (no installation) on the
[Releases](https://github.com/Endymi0n74/Audio-Config-Manager/releases) page —
only requirement: the WebView2 runtime (present by default on Windows 10/11).
Each release is built and published automatically by GitHub Actions.

## Build

Windows only:

```powershell
npm install
npm run tauri dev      # développement
.\build-release.ps1    # build release — une seule commande
```

The routing debug CLI is a subcommand of the main binary:
`"Audio Config Manager.exe" route sessions|devices|get|set`.

## Documentation

- [Architecture](docs/ARCHITECTURE.md) · [Profile format](docs/SCHEMA.md) · [Build](docs/BUILD.md)
