// Test de bout en bout de la vue « Applications » — pilote la VRAIE
// application via CDP avec un processus audio factice (fakeaudio.exe) :
//
//   1. SET   — route fakeaudio vers un périphérique de sortie actif (UI réelle)
//   2. CLEAR — retour au périphérique système (UI réelle)
//   3. INTROUVABLE — un route vers un périphérique absent (le moteur refuse
//      d'en créer une : Windows renvoie E_INVALIDARG), donc l'état est
//      vérifié côté rendu : on injecte une réponse `app_sessions` dont la
//      route pointe hors-liste, et on vérifie que la vue affiche l'option
//      « (introuvable) ».
//
// Lancement : node e2e/apps-view.e2e.mjs
// (nécessite fakeaudio.exe compilé : cargo build --release --bin fakeaudio)

import { spawn } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const PORT = 9230;
// Chemins relatifs au dépôt (le script vit dans e2e/) : aucune dépendance
// à l'emplacement du clone.
const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const APP = resolve(ROOT, "dist", "Audio Config Manager.exe");
const FAKE = resolve(ROOT, "src-tauri", "target", "release", "fakeaudio.exe");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
let fake = null;
let fakePid = null;
let app = null;
let ws = null;

function record(name, ok, detail) {
  results.push({ name, ok, detail });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? ` — ${detail}` : ""}`);
}

// Un sous-test dépendant de l'environnement (ex. présence d'un périphérique
// désactivé) est signalé SKIP au lieu d'échouer la suite.
function recordSkip(name, detail) {
  results.push({ name, ok: true, skip: true, detail });
  console.log(`SKIP  ${name} — ${detail}`);
}

async function cleanup() {
  try { if (fake) fake.kill(); } catch {}
  try { if (app) app.kill(); } catch {}
  try { ws?.close(); } catch {}
  // Filet de sécurité : tue les processus ENCORE vivants lancés par ce test,
  // par PID — jamais par nom d'image, pour ne pas tuer l'instance de
  // l'utilisateur qui tournerait en parallèle.
  await sleep(500);
  try {
    const { execSync } = await import("node:child_process");
    for (const pid of [app?.pid, fakePid].filter((p) => Number.isInteger(p))) {
      try { execSync(`taskkill /F /PID ${pid} 2>nul`); } catch {}
    }
  } catch {}
}

let id = 0;
const pending = new Map();
function send(method, params = {}) {
  return new Promise((resolve, reject) => {
    if (!ws || ws.readyState !== 1) return reject(new Error("WebSocket fermé"));
    const mid = ++id;
    pending.set(mid, (msg) => (msg.result ? resolve(msg.result) : reject(new Error(msg.error?.message || "CDP error"))));
    ws.send(JSON.stringify({ id: mid, method, params }));
  });
}
async function evalJs(expression) {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
  if (r.exceptionDetails) {
    throw new Error("EXC: " + (r.exceptionDetails.exception?.description || "evaluation error"));
  }
  return r.result?.value;
}

try {
  // ---- 1. Processus audio factice ---------------------------------------
  fake = spawn(FAKE, [], { stdio: ["ignore", "pipe", "ignore"] });
  fakePid = await new Promise((resolve, reject) => {
    let buf = "";
    const to = setTimeout(() => reject(new Error("fakeaudio : PID introuvable (10 s)")), 10000);
    fake.stdout.on("data", (d) => {
      buf += d.toString();
      const m = buf.match(/FAKEAUDIO_PID=(\d+)/);
      if (m) { clearTimeout(to); resolve(Number(m[1])); }
    });
    fake.on("exit", (code) => reject(new Error(`fakeaudio a quitté (code ${code})`)));
  });
  console.log(`INFO  fakeaudio PID=${fakePid}`);

  // ---- 2. Lancement de l'application (CDP) -------------------------------
  app = spawn(APP, [], {
    env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT}` },
    detached: true,
    stdio: "ignore",
  });
  app.unref();

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
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
  };
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  await send("Runtime.enable");

  // ---- 3. Vue Applications : la ligne fakeaudio apparaît -----------------
  await evalJs(`[...document.querySelectorAll(".nav-item")].find((el) => el.dataset.view === "apps")?.click()`);
  let row = null;
  for (let i = 0; i < 15 && !row; i++) {
    await sleep(1000);
    row = await evalJs(`(() => {
      const fake = [...document.querySelectorAll(".app-item")].find((r) => r.querySelector(".app-name")?.textContent === "fakeaudio.exe");
      return fake ? {
        pidShown: fake.querySelector(".app-detail")?.textContent ?? "",
        selectCount: fake.querySelectorAll(".app-select").length,
        playing: !!fake.querySelector(".app-playing"),
      } : null;
    })()`);
  }
  record("fakeaudio visible dans la vue", !!row, row ? `selects=${row.selectCount}, playing=${row.playing}` : "ligne absente");
  if (!row) throw new Error("fakeaudio n'apparaît pas dans la vue Applications");

  // ---- 4. SET : router fakeaudio vers un périphérique (UI réelle) --------
  const chosen = await evalJs(`(() => {
    const fake = [...document.querySelectorAll(".app-item")].find((r) => r.querySelector(".app-name")?.textContent === "fakeaudio.exe");
    const sel = fake.querySelector(".app-select");
    const target = [...sel.options].find((o) => o.value !== "");
    if (!target) return null;
    sel.value = target.value;
    sel.dataset.previous = "";
    sel.dispatchEvent(new Event("change", { bubbles: true }));
    return { value: target.value, label: target.textContent };
  })()`);
  if (!chosen) throw new Error("aucun périphérique de sortie à sélectionner");
  await sleep(2500);
  const persisted = await evalJs(
    `__TAURI__.core.invoke("app_sessions").then((r) => { const s = r.sessions.find((x) => x.processName === "fakeaudio.exe"); return s?.output?.deviceId ?? null; })`,
  );
  record("SET : route écrite et persistée", persisted === chosen.value, `${chosen.label} → ${persisted}`);

  // ---- 5. CLEAR : retour au périphérique système (UI réelle) -------------
  await evalJs(`(() => {
    const fake = [...document.querySelectorAll(".app-item")].find((r) => r.querySelector(".app-name")?.textContent === "fakeaudio.exe");
    const sel = fake.querySelector(".app-select");
    sel.value = "";
    sel.dataset.previous = "";
    sel.dispatchEvent(new Event("change", { bubbles: true }));
  })()`);
  await sleep(2500);
  const cleared = await evalJs(
    `__TAURI__.core.invoke("app_sessions").then((r) => { const s = r.sessions.find((x) => x.processName === "fakeaudio.exe"); return s?.output ?? null; })`,
  );
  record("CLEAR : route effacée", cleared === null || cleared === undefined, `output=${JSON.stringify(cleared)}`);

  // ---- 6. INTROUVABLE : rendu d'une route vers un périphérique absent ----
  // L'UI ne propose que des périphériques actifs et le moteur refuse un id
  // bogus (E_INVALIDARG) ; une route devient « introuvable » quand son
  // périphérique est ensuite débranché/désactivé. On crée donc une VRAIE route
  // vers un périphérique de lecture présent mais NON actif (retrouvé en
  // comparant la liste complète à la liste active), puis on vérifie que la vue
  // affiche l'option « (introuvable) ».
  let missing = null;
  let introuvableStatus = "non évalué";
  let introuvableSkip = false;
  try {
    const { execSync } = await import("node:child_process");
    const activeIds = await evalJs(
      `__TAURI__.core.invoke("app_sessions").then((r) => r.playbackDevices.map((d) => d.id))`,
    );
    const raw = execSync(
      'powershell -NoProfile -Command "Import-Module AudioDeviceCmdlets -EA SilentlyContinue; (Get-AudioDevice -List | Where-Object Type -eq \'Playback\' | Select-Object -ExpandProperty ID) -join \'|\'"',
      { encoding: "utf8" },
    ).trim();
    const allIds = raw.split("|").map((s) => s.trim()).filter(Boolean);
    const absentId = allIds.find((id) => !activeIds.includes(id));
    if (!absentId) {
      introuvableSkip = true;
      introuvableStatus = "aucun périphérique inactif trouvé";
    } else {
      const setResult = await evalJs(
        `__TAURI__.core.invoke("set_app_route", { pid: ${fakePid}, flow: "output", deviceId: ${JSON.stringify(absentId)} }).then(() => "ok").catch((e) => String(e))`,
      );
      if (setResult !== "ok") {
        introuvableStatus = `refus de la route vers ${absentId} : ${setResult}`;
      } else {
        // Rafraîchissement → la route pointe vers un id absent de la liste active.
        await evalJs(`document.querySelector("#apps-refresh")?.click()`);
        await sleep(1500);
        missing = await evalJs(`(() => {
          const fake = [...document.querySelectorAll(".app-item")].find((r) => r.querySelector(".app-name")?.textContent === "fakeaudio.exe");
          if (!fake) return null;
          const sel = fake.querySelector(".app-select");
          const opts = [...sel.options].map((o) => o.textContent);
          const missingOpt = opts.find((t) => t.includes("(introuvable)"));
          return { value: sel.value, hasMissingOption: !!missingOpt, missingText: missingOpt ?? null };
        })()`);
        introuvableStatus = `route → ${absentId}`;
        // Nettoyage de la route.
        await evalJs(
          `__TAURI__.core.invoke("set_app_route", { pid: ${fakePid}, flow: "output", deviceId: null }).catch(() => {})`,
        );
      }
    }
  } catch (err) {
    introuvableStatus = `erreur : ${err.message}`;
  }
  if (introuvableSkip) {
    recordSkip(
      "INTROUVABLE : option « (introuvable) » rendue",
      "aucun périphérique inactif sur cette machine — le scénario nécessite un périphérique désactivé (voir le test Rust pour le garde-fou)",
    );
  } else {
    record(
      "INTROUVABLE : option « (introuvable) » rendue",
      !!missing?.hasMissingOption && !!missing.missingText,
      `${introuvableStatus}${missing ? ` | ${JSON.stringify(missing)}` : ""}`,
    );
  }

  const failed = results.filter((r) => !r.ok);
  console.log(failed.length === 0 ? "ALL_E2E_PASS" : `E2E_FAILURES=${failed.length}`);
  process.exitCode = failed.length === 0 ? 0 : 1;
} catch (err) {
  console.error("E2E_ERROR", err.message);
  process.exitCode = 1;
} finally {
  await cleanup();
}