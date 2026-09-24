// Test de bout en bout de la veille watchDevices (event-driven, IMMNotificationClient)
// — bascule RÉELLE du périphérique d'ENTRÉE par défaut, puis restauration :
//
//   1. ÉVÉNEMENT — la bascule déclenche le callback COM → create_auto_backup
//      → émission `devices-changed` vers l'interface (chaîne complète).
//   2. FICHIER   — le fichier horodaté annoncé dans le payload existe réellement.
//   3. RETOUR    — restauration du défaut → SECOND événement (aller-retour)…
//   4. ÉTAT      — défaut d'entrée relu = identique au départ ; option
//      watchDevices et fichiers de test restaurés/nettoyés (finally).
//
// INTRUSIF : bascule le micro par défaut ~3-5 s (inaudible, entrée seule).
// Aucun son n'est dévié ; la cible est un périphérique VIRTUEL si possible.
//
// Lancement : node e2e/watch-devices.e2e.mjs

import { spawn, execFileSync, execSync } from "node:child_process";
import { readdirSync, existsSync, unlinkSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const PORT = 9234; // port CDP unique (9230/9231/9232 déjà utilisés)
const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const APP = resolve(ROOT, "dist", "Audio Config Manager.exe");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const record = (name, ok, detail) => {
  results.push(ok);
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? ` — ${detail}` : ""}`);
};
const recordSkip = (name, detail) => {
  results.push(true);
  console.log(`SKIP  ${name} — ${detail}`);
};

// PowerShell sans fenêtre, argv passé en tableau (pas d'interpolation shell) ;
// chaque appel importe le module (~1-2 s) — timeout 30 s.
function ps(cmd) {
  return execFileSync("powershell", ["-NoProfile", "-Command", cmd], {
    encoding: "utf8",
    timeout: 30000,
  }).trim();
}

let app = null;
let ws = null;
let id = 0;
const pending = new Map();
function send(method, params = {}) {
  return new Promise((res, rej) => {
    if (!ws || ws.readyState !== 1) return rej(new Error("WebSocket fermé"));
    const mid = ++id;
    pending.set(mid, (m) => (m.result ? res(m.result) : rej(new Error(m.error?.message || "CDP error"))));
    ws.send(JSON.stringify({ id: mid, method, params }));
  });
}
async function evalJs(expression) {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
  if (r.exceptionDetails) throw new Error("EXC: " + (r.exceptionDetails.exception?.description || "eval error"));
  return r.result?.value;
}

// ---- État à restaurer impérativement (finally) ---------------------------
let origInputId = null; // défaut d'entrée d'origine
let flipAttempted = false;
let restored = false;
let settingsSnapshot = null; // settings() complet
let watchEnabledByTest = false;
let profilesDir = null;
let baselineFiles = new Set();

// Le `return` du SKIP exige une fonction : tout le scénario vit dans main().
async function main() {
try {
  // ---- 1. Snapshots (AVANT toute modification) ---------------------------
  origInputId = ps("(Get-AudioDevice -Recording).ID");
  if (!origInputId) throw new Error("défaut d'entrée introuvable");
  console.log(`INFO  défaut d'entrée d'origine = ${origInputId}`);

  app = spawn(APP, [], {
    env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT}` },
    detached: true,
    stdio: "ignore",
  });
  const appPid = app.pid;
  app.unref();
  console.log(`INFO  app PID=${appPid}, CDP=${PORT}`);

  let page = null;
  for (let i = 0; i < 30 && !page; i++) {
    await sleep(1000);
    try {
      const list = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
      page = list.find((t) => t.type === "page" && t.webSocketDebuggerUrl);
    } catch {}
  }
  if (!page) throw new Error("CDP indisponible après 30 s");
  ws = new WebSocket(page.webSocketDebuggerUrl);
  ws.onmessage = (ev) => {
    const m = JSON.parse(ev.data);
    if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); }
  };
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  await send("Runtime.enable");

  settingsSnapshot = await evalJs(`__TAURI__.core.invoke("settings")`);
  profilesDir = settingsSnapshot.profilesFolder;
  baselineFiles = new Set(readdirSync(profilesDir));
  console.log(`INFO  dossier de profils = ${profilesDir} (${baselineFiles.size} fichier(s))`);

  // ---- 2. Choisir une cible : virtuel de préférence, ACTIF, ≠ défaut -----
  const sessions = await evalJs(`__TAURI__.core.invoke("app_sessions")`);
  const candidates = (sessions.recordingDevices || []).filter((d) => d.id !== origInputId);
  const target =
    candidates.find((d) => /voicemeeter|cable|virtual/i.test(d.name)) || candidates[0];
  if (!target) {
    recordSkip("Bascule du défaut d'entrée", "aucun autre périphérique d'entrée actif");
    console.log(results.every(Boolean) ? "WATCH_SKIP" : "WATCH_FAILURES");
    process.exitCode = results.every(Boolean) ? 0 : 1;
    return;
  }
  console.log(`INFO  cible de bascule = ${target.name}`);

  // ---- 3. Activer watchDevices si désactivé (restauré en fin de test) ----
  if (!settingsSnapshot.watchDevices) {
    await evalJs(
      `__TAURI__.core.invoke("update_settings", { newSettings: ${JSON.stringify({
        ...settingsSnapshot,
        watchDevices: true,
      })} }).then(() => true)`,
    );
    watchEnabledByTest = true;
    console.log("INFO  option watchDevices activée pour le test (restaurée ensuite)");
  }

  // ---- 4. Écoute de devices-changed --------------------------------------
  const listening = await evalJs(
    `__TAURI__.event.listen("devices-changed", (e) => { window.__wdEvt = e.payload; }).then(() => { window.__wdEvt = null; return true; })`,
  );
  if (!listening) throw new Error("abonnement devices-changed impossible");

  // ---- 5. Bascule RÉELLE du défaut d'entrée -------------------------------
  flipAttempted = true;
  ps(`Set-AudioDevice -ID '${target.id}'`);

  let evt = null;
  for (let i = 0; i < 15 && !evt; i++) {
    await sleep(1000);
    evt = await evalJs(`window.__wdEvt ?? null`);
  }
  record(
    "Bascule → callback COM → devices-changed émis",
    !!evt?.backup,
    evt?.backup ? `backup=${evt.backup}` : "aucun événement en 15 s",
  );

  const backupPath = evt?.backup;
  record(
    "Fichier de sauvegarde horodaté réellement créé",
    !!backupPath && existsSync(backupPath),
    backupPath ? (existsSync(backupPath) ? "existe" : `ABSENT : ${backupPath}`) : "pas de payload",
  );

  // ---- 6. Restauration du défaut → SECOND événement -----------------------
  await evalJs(`window.__wdEvt = null; true`);
  ps(`Set-AudioDevice -ID '${origInputId}'`);
  restored = true;
  let evt2 = null;
  for (let i = 0; i < 15 && !evt2; i++) {
    await sleep(1000);
    evt2 = await evalJs(`window.__wdEvt ?? null`);
  }
  record(
    "Restauration → second devices-changed (aller-retour)",
    !!evt2?.backup,
    evt2?.backup ? `backup=${evt2.backup}` : "aucun événement en 15 s",
  );

  // ---- 7. État restauré (lecture de retour indépendante) ------------------
  const readBack = ps("(Get-AudioDevice -Recording).ID");
  record(
    "Défaut d'entrée restauré à l'identique",
    readBack === origInputId,
    `lu=${readBack}`,
  );

  if (watchEnabledByTest) {
    await evalJs(
      `__TAURI__.core.invoke("update_settings", { newSettings: ${JSON.stringify(settingsSnapshot)} }).then(() => true)`,
    );
    watchEnabledByTest = false;
    const after = await evalJs(`__TAURI__.core.invoke("settings")`);
    record(
      "Option watchDevices restaurée",
      after.watchDevices === settingsSnapshot.watchDevices,
      `watchDevices=${after.watchDevices}`,
    );
  } else {
    record("Option watchDevices (déjà active) inchangée", true, "aucune modification requise");
  }

  const failed = results.filter((ok) => !ok).length;
  console.log(failed === 0 ? "WATCH_E2E_ALL_PASS" : `WATCH_E2E_FAILURES=${failed}`);
  process.exitCode = failed === 0 ? 0 : 1;
} catch (err) {
  console.error("WATCH_E2E_ERROR", err.message);
  process.exitCode = 1;
} finally {
  // ---- Restaurations de sécurité (même en cas d'échec intermédiaire) -----
  if (flipAttempted && !restored && origInputId) {
    try { ps(`Set-AudioDevice -ID '${origInputId}'`); } catch (e) { console.error("RESTORE_INPUT", e.message); }
  }
  // Laisser finir une sauvegarde en vol avant de tuer l'app (~2 s d'export).
  await sleep(4000);
  if (ws && watchEnabledByTest && settingsSnapshot) {
    try {
      await evalJs(
        `__TAURI__.core.invoke("update_settings", { newSettings: ${JSON.stringify(settingsSnapshot)} }).then(() => true)`,
      );
    } catch {}
  }
  try { ws?.close(); } catch {}
  if (app?.pid) {
    try { execSync(`taskkill /F /PID ${app.pid} 2>nul`); } catch {}
  }
  // Nettoyage : supprimer UNIQUEMENT les fichiers apparus pendant le test.
  if (profilesDir && baselineFiles.size) {
    try {
      for (const f of readdirSync(profilesDir)) {
        if (!baselineFiles.has(f) && f.endsWith(".json")) {
          unlinkSync(resolve(profilesDir, f));
          console.log(`INFO  fichier de test supprimé : ${f}`);
        }
      }
    } catch (e) { console.error("CLEANUP", e.message); }
  }
}
}

await main();
