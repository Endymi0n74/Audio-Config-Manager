# Audio Config Manager — Project Memory

Persistent knowledge about this repo. Keep this up to date when you learn something new. Companion file: `agents.md` (how to work on the repo), and `docs/ARCHITECTURE.md` + `docs/SCHEMA.md` (current architecture & profile format).

## What this app is

A Windows-only desktop app for managing audio device configuration (default playback/recording devices, volumes, and **per-application audio routing** — e.g. "Xbox → another sound card"). Built as **Tauri 2 + Rust + vanilla HTML/CSS/JS**. No JS bundler — the frontend uses Tauri's `withGlobalTauri` (`window.__TAURI__`) so ES `import`s from `@tauri-apps/api` are NOT available.

## Layout

- `src/` — frontend: `index.html`, `style.css`, `main.js`, `assets/`.
- `src-tauri/` — Rust backend.
  - `src/main.rs` — entry point; registers all commands; Mica setup; `watchDevices` = **event-driven COM subscription** (`app_routing/watch.rs`, `IMMNotificationClient` — no PowerShell polling anymore; a change on the default devices creates a timestamped backup + emits `profiles-changed`/`devices-changed`). The setting is re-read on EVERY event, so enabling/disabling takes effect immediately (no restart).
  - `src/commands.rs` — every `#[tauri::command]` (the IPC surface).
  - `src/app_routing/` — **per-application routing engine**, pure Rust FFI, split into 4 sub-modules (`mod.rs` keeps the public API + Flow/parse_flow + tests): `ffi.rs` (GUID/HSTRING/COM/apartment thread/AudioPolicyConfig factory), `devices.rs` (WASAPI enumeration + packed IDs + friendly names), `sessions.rs` (active audio sessions), `profile_apps.rs` (profile section + export/preview/restore + Apps view). The crown jewel.
  - `src/appearance.rs` — Mica backdrop + system accent color (pure FFI).
  - `src/ps.rs` — runs the embedded PowerShell engine (60 s timeout; 10 min for module install). Every spawn uses **`CREATE_NO_WINDOW`** (`hide_console`): the app is GUI-subsystem, so without it each `powershell.exe` child flashes its own console window — don't lose it. Also hosts `module_available()` — a **file-system check** (versioned `AudioDeviceCmdlets\<ver>\` folder + manifest under the module roots: USERPROFILE/OneDrive/PSModulePath/ProgramFiles, µs) replacing the old PowerShell `Get-Module -ListAvailable` probe; cached in a `Mutex`, invalidated by `invalidate_module_cache()` after a successful install.
  - `src/settings.rs`, `src/profiles.rs` — settings JSON + profile listing/retention.
  - `src/route_cli.rs` — **CLI debug subcommand** of the main exe: `Audio Config Manager.exe route sessions|devices|get|set` (was `src/bin/route.rs` / `route.exe`; merged so `dist/` ships ONE exe). Reuses `app_routing` directly. Since the exe is GUI-subsystem, `attach_console()` (AttachConsole parent / AllocConsole, then CONOUT$/CONIN$ redirection; skips if a valid std handle was inherited) must run before any print.
  - `audio-config-manager.ps1` — embedded PowerShell script (`export|preview|restore` actions — the `overview` action still exists in the script but **no Rust command calls it anymore**, see below).
- `dist/` — shipped binary: **only** `Audio Config Manager.exe` (the route CLI is a subcommand of it; `fakeaudio.exe` stays in `target/release/` as a dev/test helper), copied from `target/release/` (`bundle.targets` is `[]`, so no installer).
- **Build release = one command: `\build-release.ps1`** — first **stops any running app instance** (`Stop-Process` on names `Audio Config Manager` **and** `audio-config-manager`) **and `fakeaudio.exe`** (dedicated warning: an E2E test in progress would be interrupted), guarding against locked-exe copy failures and zombie processes; then builds the `fakeaudio` test helper, runs `npm run tauri build` (embeds the frontend via Tauri's `custom-protocol` feature — a **default feature** `[features] default = ["tauri/custom-protocol"]` in `Cargo.toml`, so even a plain `cargo build --release` embeds it; `tauri dev` strips it via `--no-default-features`), copies **only the main exe** into `dist/` (and removes stale `route.exe`/`fakeaudio.exe` copies from older builds), then verifies the exe: **PE signature** (MZ / `PE\0\0` / magic PE32/PE32+) + **GUI subsystem (2)** + the **Rust integration test** `frontend_is_embedded_via_custom_protocol` (embedded assets) — no terminal window, no `ERR_CONNECTION_REFUSED`. **`npm run tauri build` alone does NOT update `dist/`** — it only produces `target/release/audio-config-manager.exe`.
- `e2e/` — `apps-view.e2e.mjs` (frontend end-to-end test, CDP-driven) and `watch-devices.e2e.mjs` (event-driven watch E2E — INTRUSIVE: flips the default input device then restores everything in `finally`).

## Backend command surface (`commands.rs`)

`settings`, `update_settings`, `overview`, `save_profile`, `preview_profile`, `restore_profile`, `delete_profile`, `import_profile`, `profiles_folder`, `choose_profiles_folder`, `open_profiles_folder`, `install_audio_module`, `app_sessions`, `set_app_route`, plus `appearance::system_accent_color` and `appearance::backdrop_enabled`.

## Overview command — 100 % COM, no PowerShell

`commands::overview()` = `ps::module_available()` (file check) + `app_routing::overview_devices()`, both inside `spawn_blocking`. **No `powershell.exe` is spawned anymore** (~2 s → ms). The historical **flat camelCase payload** is unchanged and guarded by `overview_serializes_flat_camel_case_payload`: `{ moduleAvailable, playbackCount, recordingCount, defaultPlayback{id,name,volume}, defaultRecording{id,name,volume} }` (`Overview` uses `#[serde(flatten)] DeviceOverview`).

COM facts (in `app_routing/devices.rs`, all `noinit` + wrapped by `with_apartment`):
- `IMMDeviceEnumerator::GetDefaultAudioEndpoint` **slot 4**, flow + **role console = 0** (ERole) — same default the Sound panel and the watch baseline use.
- `IMMDevice::Activate` **slot 3** with `IID_IAudioEndpointVolume` = `5CDF2C82-841E-4546-9722-0CF74078229A`, `CLSCTX_ALL` → `IAudioEndpointVolume::GetMasterVolumeLevelScalar` **slot 9** → `×100` (same math as the old cmdlet). **Slot order (official SDK `endpointvolume.h`, verified on this machine)**: `3 RegisterControlChangeNotify · 4 Unregister · 5 GetChannelCount · 6 SetMasterVolumeLevel · 7 SetMasterVolumeLevelScalar · 8 GetMasterVolumeLevel · 9 GetMasterVolumeLevelScalar`. Slot **6 is the SETTER** — an earlier note claimed 6 was the getter; calling it with our `(ptr, f32*)` signature made the callee read an uninitialized `LPCGUID` from **R8** → garbage-pointer read → deterministic `0xC0000005` (ntdll) crash ~400 ms after launch (frontend calls `loadOverview()` at startup). Always re-check vtable order against the SDK header before trusting a slot number.
- Counts = `EnumAudioEndpoints(DEVICE_STATE_ACTIVE)` per flow — measured identical to `Get-AudioDevice -List` on this machine (13 outputs / 11 inputs).
- Names = `friendly_name()` (`OpenPropertyStore` slot 4 → `IPropertyStore::GetValue` slot 5, `PKEY_Device_FriendlyName`, `VT_LPWSTR`).
- `DefaultDeviceInfo`/`DeviceOverview` are `pub` in `devices.rs`; `mod.rs` re-exports `overview_devices` + `DeviceOverview` always, `DefaultDeviceInfo` **`#[cfg(test)]` only** (named solely by the commands.rs contract test — an unconditional re-export trips `unused_imports` under `clippy -D warnings`).

Tauri v2 converts Rust `snake_case` args to `camelCase` on the JS side (e.g. `device_id` → `deviceId`, `new_settings` → `newSettings`).

## Profile format (JSON)

`Metadata` (ComputerName, Timestamp ISO-8601, Version "3.1") · `PlaybackDevices`/`RecordingDevices` (ID, Name, Volume|null) · `DefaultPlayback`/`DefaultRecording` · **`applications`** (added by Rust at export; optional): array of `{ processName, executablePath?, output?: {deviceId, deviceName?}, input?: {deviceId, deviceName?} }`. PowerShell writes the file with a UTF-8 **BOM** — all Rust readers strip it. A profile without `applications` is still valid (v3.1-compatible).

## Per-application routing engine (`app_routing/`) — key facts

This is the hard-won reverse-engineering. Do not "simplify" it away.

- Uses Windows' internal **`Windows.Media.Internal.AudioPolicyConfig`** (implemented in `AudioSes.dll`) — the same path as EarTrumpet / SoundVolumeView / winappaudiorouter.
- Factory interface IID `ab3d4648-e242-459f-b02f-541c70306324` (Win11 ≥ 21H2), downlevel `2a59116d-6c4f-45e0-a74f-707e3fef9258`.
- Vtable slots: **25** = `SetPersistedDefaultAudioEndpoint(pid, dataFlow, role, HSTRING)`, **26** = `Get…` (out HSTRING). Get returns `0x80070490` (ERROR_NOT_FOUND) when the app has no persisted route → follows the system default.
- Device IDs are **packed**: `\\?\SWD#MMDEVAPI#{id}#{interface-guid}` (render suffix `#{e6327cad-dcec-4949-ae8a-991e976a79d2}`, capture `#{2eef81be-33fa-4800-9670-1cd474972c3f}`). `pack_device_id`/`unpack_device_id` handle this.
- Set is written for **both** roles `eConsole` (0) and `eMultimedia` (1); Get reads `eMultimedia` (1). Clear = Set with a null/empty string.
- The class is **STA** — ALL policy ops must run on a dedicated COM apartment thread (`with_apartment`, thread named `audio-policy-config`). **`with_apartment<R>` returns `Result<R, String>`** where `R` is the closure's return type; when the closure itself returns a `Result<_, String>` (as `list_active_app_routes` and `set_app_route` do), that nests to `Result<Result<_,String>, String>` — flatten with a trailing `?`.
- Session enumeration via public WASAPI FFI: `IMMDeviceEnumerator` → per-device `IAudioSessionManager2` → `IAudioSessionEnumerator` → `IAudioSessionControl2` (QI), `GetProcessId` (slot 14). **`IAudioSessionControl::GetState` (slot 3, inherited by Control2) reports "playing"** — `AudioSessionStateActive = 1` → the `playing` flag in `AppSessionRow`.
- Friendly device names without PowerShell: `PKEY_Device_FriendlyName` (`fmtid {a45c254e-df1c-4efd-8020-67d146a850e0}`, pid 14) via `IMMDevice::OpenPropertyStore` (slot 4, `STGM_READ`) + `IPropertyStore::GetValue` (slot 5) → `PROPVARIANT`, `VT_LPWSTR = 31`. `list_active_devices()`; thread-safe wrapper `active_devices()`.
- **COM init gotcha (bit us in the CLI):** `CoCreateInstance` on a thread with no initialized COM apartment returns empty lists. Always go through `with_apartment` (that's what `active_devices()` and the `app_sessions` command do).
- **RDP/remote limitation:** `Set` was once refused with `0x80070032` (ERROR_NOT_SUPPORTED) on this machine during early RDP testing, but it has since worked repeatedly here — treat it as session-dependent, not a bug. The refusal you're most likely to actually hit is **off-list device ids → `0x80070057` (E_INVALIDARG)** (see Testing section).

## WASAPI / AudioPolicyConfig vtable map (slots used in `app_routing/`)

All COM interfaces start with `IUnknown` at slots 0-2 (`QueryInterface`, `AddRef`, `Release`). Numbers below are the exact vtable indices used in the code and **verified empirically** on this machine:

```
IMMDeviceEnumerator
  [3] EnumAudioEndpoints(flow, stateMask, **collection)
  [4] GetDefaultAudioEndpoint(flow, role, **device)     (used by fakeaudio)

IMMDeviceCollection
  [3] GetCount(*u32)   [4] Item(u32, **device)

IMMDevice
  [3] Activate(iid, CLSCTX, *pv, **iface)
  [4] OpenPropertyStore(STGM_READ, **IPropertyStore)
  [5] GetId(**LPWSTR)                       // CoTaskMemFree l'id

IPropertyStore
  [5] GetValue(REFPROPERTYKEY, *PROPVARIANT)
      // PKEY_Device_FriendlyName: fmtid {a45c254e-df1c-4efd-8020-67d146a850e0}, pid 14
      // PROPVARIANT: 8 octets d'en-tête (vt + 3 réservés) puis union (pszVal à l'offset 8) ; VT_LPWSTR = 31

IAudioSessionManager2            (hérite de IAudioSessionManager: 3-4)
  [5] GetSessionEnumerator(**IAudioSessionEnumerator)

IAudioSessionEnumerator
  [3] GetCount(*i32)   [4] GetSession(i32, **IAudioSessionControl)

IAudioSessionControl2           (hérite de IAudioSessionControl: 3-9)
  [3] GetState(*i32)   // 1 = AudioSessionStateActive  → drapeau « playing »
  [14] GetProcessId(*u32)      // PID du processus de la session
  // NB : GetState sur le slot 3 marche sur le pointeur IAudioSessionControl2
  //      (vtable partagée avec la base). GetProcessId = 14 est vérifié.

AudioPolicyConfig (fabrique, IID ab3d4648… / 2a59116d…)
  [2] Release()
  [25] SetPersistedDefaultAudioEndpoint(pid, dataFlow, role, HSTRING deviceId)
  [26] GetPersistedDefaultAudioEndpoint(pid, dataFlow, role, *HSTRING)

IMMDeviceEnumerator (notifications — watch.rs, standard MMDevice, NOT reverse-engineered)
  [6] RegisterEndpointNotificationCallback(IMMNotificationClient*)   / [7] Unregister…
IMMNotificationClient (IID 7991EEC9-7E89-4D85-8390-6C703CEC60C0, objet COM statique côté Rust)
  [3] OnDeviceStateChanged(id, state)   [4] OnDeviceAdded(id)   [5] OnDeviceRemoved(id)
  [6] OnDefaultDeviceChanged(flow, role, id)   [7] OnPropertyValueChanged(id, key)
  // Enregistré depuis un thread MTA dédié (« audio-devices-watch », RoInitialize(1)) :
  // les callbacks arrivent sur des threads MTA — aucun message loop à pomper.
  // Un seul callback utile : OnDefaultDeviceChanged → WatchState déduplique
  // la rafale des 3 rôles → 1 seule sauvegarde par changement réel.
```

Valeurs utiles : `eRender=0 / eCapture=1` ; rôles `eConsole=0 / eMultimedia=1` ; `CLSCTX_ALL=23` ; `DEVICE_STATE_ACTIVE=0x1` (all = 0xF) ; `STGM_READ=0`. Voir `docs/ARCHITECTURE.md` pour le contexte.

## Rust FFI conventions

- `windows-sys 0.59.0` (cached) does **not** provide the WASAPI COM interfaces → the engine uses **raw FFI** (`combase`/`ole32`/`kernel32`) throughout.
- `combase` uses `#[link(name = "combase", kind = "raw-dylib")]` to avoid import libraries (MSVC linker couldn't find `combase.lib`).
- Link names: `ole32` (`CoCreateInstance`, `CoTaskMemFree`, `PropVariantClear`), `kernel32` (process/snapshot APIs), `advapi32`+`user32`+`dwmapi` in `appearance.rs`.
- COM pattern: `Guid`, `HString` (WinRT strings), `ComRef` (auto-Release via vtable slot 2), `query_interface` (slot 0), `vtable_slot`.

## Appearance (`appearance.rs`)

- **Real Mica** via `window-vibrancy 0.6.0` (`apply_mica`), WebView2 background made transparent; frontend toggles `html.backdrop` class → translucent surfaces. **Auto-disables** on remote sessions (`SM_REMOTESESSION` = `0x1000`) or when `EnableTransparency` = 0 — falls back to solid themed surfaces (avoids showing raw desktop).
- **System accent**: reads `HKCU\Software\Microsoft\Windows\DWM\AccentColor` (fallback `Personalize\SystemAccentColor`), returns `#rrggbb`; frontend sets `--accent` + computes `--on-accent` from luminance. Accent-derived tints use `color-mix()` in CSS.

## Frontend details

- **Views** (sidebar `data-view`): `overview`, `profiles`, `apps`, `settings`.
- **Apps view** (`loadAppSessions`): lists active audio processes, each with an output + input native `<select>` of active devices ("Périphérique système (par défaut)" clears the route), an "En lecture" pulsing indicator when `session.playing`, and a manual "Actualiser" button. **`app_sessions` is 100 % PowerShell-free**: the device lists come from `app_routing::active_devices()` (COM `IMMDeviceEnumerator` + `PKEY_Device_FriendlyName`), so the apps view no longer spawns PowerShell at all (the PS `devices` action remains in the script only as reference, unused by Rust).
- **Auto-refresh**: while the apps view is active, `appsTimer = setInterval(loadAppSessions, 15000)`; cleared when leaving the view. `loadAppSessions` **skips a cycle** if any `.app-select` is focused or disabled (don't yank an open dropdown or an in-flight route write). Three refresh levels: (1) full `JSON.stringify(result)` unchanged → nothing; (2) only the `playing` flags changed (structure = result minus `playing` identical) → the « En lecture » badges are updated **in place** (`updatePlayingBadges`, keyed by `data-pid` on each `.app-item`) with no rebuild; (3) anything else (processes, routes, devices, error state) → full rebuild. The view never flickers and selects/scroll survive an activity-only change.
- Design system in `style.css`: Segoe UI Variable, system theme via `prefers-color-scheme`, `--accent`, Fluent states, WCAG-AA contrast, `prefers-reduced-motion`, single status indicator, empty states, keyboard-accessible modals (Esc / overlay click / focus return).

## Testing & live verification

- Rust: `cargo test` — 37 tests + 2 ignored (the ignored ones are the hardware-only live tests `live_policy_round_trip` and `e2e_apps_view_set_clear_missing`). Since the CLI merged into the main exe (`route_cli.rs`), there is no separate CLI bin anymore — its tests run as part of the main bin.
- Manual hardware test (RDP skip): `cargo test -- --ignored live_policy_round_trip` — spawns a sound player, sets/get/clears a route.
- **Fake audio process** `src-tauri/src/bin/fakeaudio.rs` → `src-tauri/target/release/fakeaudio.exe` (dev/test helper — **stays in `target/release/`, never shipped in `dist/`**): a tiny Rust bin that opens a real WASAPI render session (a near-inaudible tone) and appears as an active audio app — the basis for E2E tests.
- **E2E (backend)** `e2e_apps_view_set_clear_missing` (`#[ignore]`): spawns fakeaudio, verifies its session appears, then **set** (persisted + read back), **clear**, and **introuvable** (either a route to an inactive device persisted with `device_name=None`, or — since Windows refuses off-list ids with 0x80070057 — proof of the guard). Run: `cargo test --bin audio-config-manager -- --ignored --nocapture e2e_apps_view_set_clear_missing`.
- **E2E (frontend)** `e2e/apps-view.e2e.mjs`: drives the REAL app over CDP (Node's global WebSocket) with fakeaudio — real **set**/**clear** through the UI (verified against the backend), the « En lecture » indicator, and the **« (introuvable) »** option when a route points off-list (SKIPs if the machine has no inactive device). Run: `node e2e/apps-view.e2e.mjs`.
- **E2E (watch)** `e2e/watch-devices.e2e.mjs`: INTRUSIVE — flips the default INPUT device for ~3-5 s (prefers a virtual Voicemeeter target), then proves the full chain: COM callback → `create_auto_backup` → `devices-changed` event → real timestamped file. Restores the default (read back), the `watchDevices` setting, and deletes its test backups in `finally`. Run: `node e2e/watch-devices.e2e.mjs`. Validated 7× green on this machine.
- **Important**: `list_active_app_routes` (and `with_apartment`-wrapped fns generally) must NOT be called from inside another `with_apartment` closure — the apartment thread would deadlock (it happened during E2E development). Do the name resolution inline instead.
- Windows **refuses** per-app routes to ids outside the active render set (0x80070057): confirmed for an input id, a bogus GUID, and a disabled device. A route only becomes « introuvable » when its device disappears after being routed.
- **Live CDP verification** (no `ws` lib installed; Node ≥ 21 has a global `WebSocket`): launch the exe with `$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=NNNN"` + `Start-Process`, then probe `http://127.0.0.1:NNNN/json` and drive `Runtime.evaluate` over the WebSocket. See `agents.md`.
- CLI quick check: `"./dist/Audio Config Manager.exe" route sessions` shows a `LECT` column with `●` for apps playing sound right now (console auto-attached).

## Version / misc

- Cargo package + app version: **1.3.0**.
- Release profile: `lto = "thin"` (was fat `true`) + `codegen-units = 1` + `opt-level = "s"` + `strip = true` — full release build ≈ 3m48 on this machine, exe ≈ 4.98 MB (thin LTO is ~2x faster to link, ~1-4 % bigger than fat LTO).
- Reference original binary: `D:\0day\Audio Config Manager.exe` (the user's v3.1-era exe — reference only, do not delete).
- The app is committed to git (repo `Endymi0n74/Audio-Config-Manager`, branch `main`) and released as tag **v1.0.0**; the GitHub Actions workflow (`.github/workflows/build.yml`) builds on every push/PR and publishes the exe to a GitHub Release on `v*` tags. Screenshots for the README live in `docs/screens/`.