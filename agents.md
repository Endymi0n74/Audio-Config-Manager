# Audio Config Manager — Agent Operations Guide

How to actually work on this repo without breaking things. Project knowledge lives in `memory.md`; this is the "how to build / test / run / avoid foot-guns" file.

## Scope & working directory

- **Work only inside `D:\Codex\Audio Manager Windows`** — the user forbids going outside it. Treat everything else on the machine as read-only.
- The app is Windows-only. All terminal commands below use **bash (Git Bash)** — POSIX syntax (`mv`/`rm`), not `move`/`del`.

## Build / test / deploy loop

```bash
# Rust tests (app binary incl. the route CLI module, plus fakeaudio):
cd "Audio Manager Windows/src-tauri" && cargo test

# Frontend syntax check:
cd "Audio Manager Windows" && node --check src/main.js

# Release build of the app (main exe only — fakeaudio is built separately, below):
cd "Audio Manager Windows" && npm run tauri build

# Test helper build (needed before the E2E tests):
cd "Audio Manager Windows/src-tauri" && cargo build --release --bin fakeaudio

# Deploy (no installer — bundle.targets is []): dist/ ships ONLY the main exe —
# the route CLI is a subcommand of it, and fakeaudio.exe (plus the legacy
# route.exe) are dev/test helpers that STAY in target/release/. Never copy them
# into dist/.
cp -f "src-tauri/target/release/audio-config-manager.exe" "dist/Audio Config Manager.exe"

# Full release = ONE command (PowerShell): .\build-release.ps1 — stops running
# instances, builds fakeaudio + main exe, copies only the main exe into dist/,
# removes stale route.exe/fakeaudio.exe copies, verifies PE/GUI/embedded frontend.
```

## CRITICAL: zombie-process trap (already caused real confusion)

Windows keeps a process alive after its `.exe` is deleted/replaced, and then reports it as running from `D:\$RECYCLE.BIN\...` — these ghosts show the **old UI** and made the user think "nothing works / still the old look". The real `dist` exe was fine all along.

**Always:**
1. Close any running instance **before** copying over/replacing the exe.
2. Check for running instances first: `tasklist //FO CSV 2>/dev/null | grep -iE "Audio Config|audio-config"` (both process names — see below).
3. Kill zombies by name if present. They lose nothing (the binary no longer exists as a file).

## Process names (grep / Get-Process / Stop-Process)

The process name depends on WHICH exe was launched:

| exe | `Get-Process -Name` | `tasklist` shows |
|---|---|---|
| `dist\Audio Config Manager.exe` (livré) | `Audio Config Manager` (**with spaces**) | `Audio Config Manager.exe` |
| `target\release\` / `target\debug\` `audio-config-manager.exe` | `audio-config-manager` | `audio-config-manager.exe` |
| `fakeaudio.exe` (test helper) | `fakeaudio` | `fakeaudio.exe` |

Gotcha: `Get-Process -Name 'audio-config-manager'` finds **nothing** when the user
launched the shipped `dist` exe — this already caused a false « app not running »
reading during verification. Always check **both** names:

```powershell
Get-Process -Name 'Audio Config Manager', 'audio-config-manager' -ErrorAction SilentlyContinue
Stop-Process  -Name 'Audio Config Manager', 'audio-config-manager' -Force -ErrorAction SilentlyContinue
```

- `tasklist` appends `.exe`, so grep both: `tasklist //FO CSV 2>/dev/null | grep -iE "Audio Config|audio-config"`.
- `build-release.ps1` step 0 already stops both app names **and** `fakeaudio` (with a dedicated warning).
- `grep -i "audio"` alone also matches `audiodg.exe` (Windows' own audio service) — don't confuse it with the app.

## Environment quirks (this machine)

- **The shell intermittently wedges/times out** on network and process-listing commands. Mitigate:
  - Use short `timeout_seconds` and retry.
  - Prefer writing small **script files** (`.mjs`, `.ps1`) over huge heredocs — heredocs get mangled by bash escaping, especially `$`/backticks/backslashes.
  - When a `powershell -Command "..."` string is eating `$vars`, put the logic in a `.ps1` file instead.
- PowerShell on this box is often Windows PowerShell 5.1: `Set-Content -Encoding UTF8` writes a **BOM**.
- `route set` to an id outside the active device list is refused with `0x80070057` (E_INVALIDARG) — that's the OS guard, not a bug. A 0x80070032 refusal has only rarely appeared in remote sessions; don't chase it as a bug.

## Live verification (CDP) — how we actually test the real exe

There is **no `ws` npm lib** installed, but Node ≥ 21 ships a **global `WebSocket`** (undici), so plain `.mjs` files can drive Chrome DevTools Protocol:

```bash
# 1. Launch the app detached with a debug port (use a unique port each run, e.g. 9229):
#    put this in launch_debug.ps1:
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9229"
Start-Process -FilePath "D:\Codex\Audio Manager Windows\dist\Audio Config Manager.exe"

# 2. In an .mjs probe:
const list = await (await fetch("http://127.0.0.1:9229/json")).json();
const page = list.find((t) => t.type === "page" && t.webSocketDebuggerUrl);
const ws = new WebSocket(page.webSocketDebuggerUrl);
// then send {"id":n,"method":"Runtime.evaluate","params":{expression, returnByValue:true, awaitPromise:true}}
```

Patterns that work well:
- Read DOM state (badge text, computed styles, class toggles) via `Runtime.evaluate`.
- Drive the UI by `.click()` / `dispatchEvent(new Event("change",{bubbles:true}))`.
- Confirm the auto-refresh by installing a `MutationObserver` that counts `addedNodes` on the list container and waiting ~10 s (HTML-length comparison can't distinguish a rebuild with identical content).
- To change a route during a test, **always restore it afterward** (set back to system default) so you don't leave the user's audio configured differently. Verify the restore with a follow-up read.

## Route CLI (subcommand of the main exe) — quick sanity checks

The CLI lives inside the main binary: `Audio Config Manager.exe route …`.
Being a GUI-subsystem exe, it attaches the parent console (or allocates one)
before printing, so output shows up in cmd/PowerShell. From a shell:

```bash
"./dist/Audio Config Manager.exe" route sessions          # ● = currently playing
"./dist/Audio Config Manager.exe" route devices output
"./dist/Audio Config Manager.exe" route set <pid|name> output "<device-name-substring|system>"
"./dist/Audio Config Manager.exe" route get <pid|name>
```

## E2E tests (Applications view)

Requires the fake audio process built first: `cargo build --release --bin fakeaudio`
(the binary lands in `src-tauri/target/release/fakeaudio.exe`, where the suite
expects it — it is NOT shipped in `dist/`).

```bash
# Backend E2E (set / clear / introuvable-guard) — hardware test:
cargo test --bin audio-config-manager -- --ignored --nocapture e2e_apps_view_set_clear_missing

# Frontend E2E (real UI set/clear + introuvable rendering via CDP):
node e2e/apps-view.e2e.mjs
```

Notes:
- `fakeaudio` opens a real WASAPI render session so it shows up as an active audio app.
- The E2E spawns its own app + fakeaudio and kills them in `finally`. If CDP says "unavailable", kill stale `Audio Config Manager`/`fakeaudio` processes first (they can hold the debug port).
- The introuvable sub-test SKIPs when the machine has no disabled device (Windows refuses routing to off-list ids with 0x80070057).

## Golden rules for editing

- Match existing conventions; verify a library is already used before adding one. Prefer raw FFI here — the project deliberately avoids pulling WASAPI crates.
- When touching `app_routing.rs`: every policy/COM op must run through `with_apartment`, and remember the double-`Result` flattening (`with_apartment(move || { ... Ok(..) })?`).
- Frontend commands: `invoke("name", { camelCaseArgs })`.
- After editing: `cargo test` + `node --check src/main.js`, then rebuild + redeploy + live-verify, then clean up scratch files.
- **Never** run `git commit`/`push` unless explicitly asked. Leave changes uncommitted otherwise.

## Useful reference files

- `docs/ARCHITECTURE.md` — current architecture incl. routing engine + CLI.
- `docs/SCHEMA.md` — profile JSON schema (incl. `applications` section).
- `memory.md` — the reverse-engineering facts (vtable slots, IIDs, device-id packing, COM init gotchas).