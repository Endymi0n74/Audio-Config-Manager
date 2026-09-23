// API Tauri injectée globalement (withGlobalTauri) — ce projet n'utilise
// aucun bundler, les imports ES de @tauri-apps/api ne seraient pas résolus.
// Accès aux éléments statiques : échec explicite plutôt que TypeError obscur
// si un id est renommé dans index.html.
function must(id) {
  const el = document.getElementById(id);
  if (!el) throw new Error(`Élément introuvable : #${id}`);
  return el;
}

// Rend les erreurs JavaScript visibles au lieu d'échouer en silence.
window.addEventListener("error", (event) => {
  toast(`Erreur interne : ${event.message}`, "error");
});
window.addEventListener("unhandledrejection", (event) => {
  toast(`Erreur interne : ${event.reason}`, "error");
});

const __TAURI__ = window.__TAURI__;
if (!__TAURI__) {
  document.addEventListener("DOMContentLoaded", () => {
    must("ready-badge").textContent = "● API Tauri indisponible";
    must("ready-badge").style.color = "var(--red)";
    toast("API Tauri non injectée — application hors Tauri ?", "error");
  });
  throw new Error("API Tauri indisponible");
}
const invoke = __TAURI__.core.invoke;
const listen = __TAURI__.event.listen;

const views = {
  overview: must("view-overview"),
  profiles: must("view-profiles"),
  apps: must("view-apps"),
  settings: must("view-settings"),
};

const navItems = document.querySelectorAll(".nav-item");
const statusText = must("status-text");
const readyBadge = must("ready-badge");

let currentSettings = null;
let previewPath = null;
let confirmPath = null;
let appsTimer = null;
// Dernière réponse `app_sessions` affichée (sérialisée) : tant qu'elle est
// identique, la liste n'est pas reconstruite (voir loadAppSessions).
let appsSignature = null;
// Même chose, sans les drapeaux « playing » : permet de distinguer un simple
// changement d'activité audio (badges mis à jour en place) d'un vrai
// changement de structure (reconstruction complète).
let appsStructureSignature = null;

function withTimeout(promise, milliseconds, message) {
  let timeoutId;
  const timeout = new Promise((_, reject) => {
    timeoutId = setTimeout(() => reject(new Error(message)), milliseconds);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timeoutId));
}

function setStatus(text, tone = "") {
  statusText.textContent = text;
  const bar = must("status-bar");
  bar.classList.toggle("hidden", !text.trim());
  bar.classList.toggle("error", tone === "error");
  bar.classList.toggle("success", tone === "success");
}

function setBusy(isBusy) {
  document.querySelectorAll("button").forEach((button) => {
    if (button.id !== "install-module") {
      button.disabled = isBusy;
    }
  });
}

function toast(message, tone = "") {
  const container = must("toasts");
  const item = document.createElement("div");
  item.className = `toast ${tone}`.trim();
  item.textContent = message;
  container.appendChild(item);
  setTimeout(() => item.classList.add("show"), 10);
  setTimeout(() => {
    item.classList.remove("show");
    setTimeout(() => item.remove(), 300);
  }, 4200);
}

// Erreur utilisateur standard : toast + barre de statut, même message.
function reportError(err) {
  toast(String(err), "error");
  setStatus(String(err), "error");
}

// Exécute une action en désactivant les boutons pendant sa durée.
async function runBusy(action) {
  setBusy(true);
  try {
    await action();
  } finally {
    setBusy(false);
  }
}

function switchView(view) {
  Object.entries(views).forEach(([key, element]) => {
    element.classList.toggle("hidden", key !== view);
  });
  navItems.forEach((item) => item.classList.toggle("active", item.dataset.view === view));
  if (appsTimer) {
    clearInterval(appsTimer);
    appsTimer = null;
  }
  if (view === "profiles") loadProfiles().catch(reportError);
  if (view === "apps") {
    loadAppSessions().catch(reportError);
    // Rafraîchissement en temps réel : les indicateurs « En lecture »
    // suivent les sessions audio toutes les 15 secondes ; la liste n'est
    // reconstruite que si l'état a réellement changé (voir loadAppSessions).
    appsTimer = setInterval(() => loadAppSessions().catch(reportError), 15000);
  }
  if (view === "settings") loadSettingsForm().catch(reportError);
}

navItems.forEach((item) => {
  item.addEventListener("click", () => switchView(item.dataset.view));
});

function formatSize(bytes) {
  if (bytes < 1024) return `${bytes} o`;
  return `${(bytes / 1024).toFixed(1)} Ko`;
}

function formatDate(rfc3339) {
  const date = new Date(rfc3339);
  if (Number.isNaN(date.getTime())) return "?";
  return date.toLocaleString("fr-FR");
}

function deviceRow(label, info, found) {
  const row = document.createElement("div");
  row.className = "preview-row";
  const name = document.createElement("span");
  name.className = "preview-row-name";
  const volume = document.createElement("span");
  volume.className = "preview-row-volume";
  if (info) {
    name.textContent = info.name;
    volume.textContent =
      typeof info.volume === "number" ? `Volume : ${Math.round(info.volume)} %` : "";
  } else {
    name.textContent = "Non enregistré";
    volume.textContent = "";
  }
  row.append(
    Object.assign(document.createElement("b"), { textContent: label }),
    name,
    volume,
  );
  if (info && found === false) {
    const missing = document.createElement("span");
    missing.className = "preview-row-missing";
    missing.textContent = "Introuvable sur cette machine";
    row.appendChild(missing);
  }
  return row;
}

function chip(text, kind) {
  const span = document.createElement("span");
  span.className = `chip chip-${kind}`;
  span.textContent = text;
  return span;
}

// Sélecteur de périphérique d'un flux pour une application (vue « Applications »).
function routeSelect(flow, devices, current, pid, name) {
  const group = document.createElement("label");
  group.className = "app-route";
  const label = document.createElement("span");
  label.className = "app-route-label";
  label.textContent = flow === "output" ? "Sortie" : "Entrée";
  const select = document.createElement("select");
  select.className = "app-select";
  const system = document.createElement("option");
  system.value = "";
  system.textContent = "Périphérique système (par défaut)";
  select.appendChild(system);
  let matched = false;
  for (const device of devices) {
    const option = document.createElement("option");
    option.value = device.id;
    option.textContent = device.name;
    if (current && device.id === current.deviceId) {
      option.selected = true;
      matched = true;
    }
    select.appendChild(option);
  }
  if (current && !matched) {
    // Périphérique cible absent de la liste active : option grisée dédiée.
    const missing = document.createElement("option");
    missing.value = current.deviceId;
    missing.textContent = `${current.deviceName || current.deviceId} (introuvable)`;
    missing.selected = true;
    select.appendChild(missing);
  }
  select.dataset.previous = select.value;
  select.addEventListener("change", async () => {
    const previous = select.dataset.previous ?? "";
    select.dataset.previous = select.value;
    select.disabled = true;
    try {
      await invoke("set_app_route", {
        pid,
        flow,
        deviceId: select.value ? select.value : null,
      });
      toast(
        select.value
          ? `${name} → ${select.selectedOptions[0].textContent}`
          : `${name} → périphérique système`,
        "success",
      );
    } catch (err) {
      select.value = previous;
      select.dataset.previous = previous;
      toast(String(err), "error");
    } finally {
      select.disabled = false;
    }
  });
  group.append(label, select);
  return group;
}

// Seule l'activité audio a changé : met à jour les badges « En lecture » des
// lignes existantes, sans reconstruire la liste (sélecteurs et scroll
// intacts). Chaque ligne porte son PID dans `data-pid`.
function updatePlayingBadges(list, sessions) {
  const byPid = new Map(sessions.map((s) => [String(s.pid), s]));
  list.querySelectorAll(".app-item").forEach((item) => {
    const session = byPid.get(item.dataset.pid);
    const badge = item.querySelector(".app-playing");
    const nameRow = item.querySelector(".app-name-row");
    if (session?.playing && !badge) {
      const playing = document.createElement("span");
      playing.className = "app-playing";
      playing.textContent = "En lecture";
      playing.title = "Cette application joue (ou enregistre) du son actuellement.";
      nameRow.appendChild(playing);
    } else if (!session?.playing && badge) {
      badge.remove();
    }
  });
}

// Vue « Applications » : processus audio actifs + route persistée courante.
async function loadAppSessions() {
  const list = must("apps-list");
  const banner = must("apps-banner");
  const empty = must("apps-empty");
  // Ne pas interrompre une interaction en cours : liste déroulante ouverte
  // (le sélecteur a le focus) ou écriture de route en cours (sélecteur
  // désactivé) — le prochain cycle rafraîchira.
  if (document.querySelector(".app-select:focus, .app-select:disabled")) {
    return;
  }
  let result;
  try {
    result = await withTimeout(
      invoke("app_sessions"),
      12000,
      "La lecture des applications audio a expiré (12 s).",
    );
  } catch (err) {
    // Erreur transitoire : on force le prochain cycle à reconstruire la
    // liste dès que l'état redevient lisible.
    appsSignature = null;
    appsStructureSignature = null;
    list.innerHTML = "";
    banner.classList.add("hidden");
    empty.classList.remove("hidden");
    empty.textContent = String(err);
    return;
  }
  // Rafraîchissement sans churn : si rien n'a changé (processus, lecture en
  // cours, routes, périphériques), la liste n'est pas reconstruite — les
  // sélecteurs restent intacts et la vue ne « clignote » pas.
  const signature = JSON.stringify(result);
  if (appsSignature === signature) return;
  // Structure sans les drapeaux « playing » : si elle est inchangée, seule
  // l'activité audio a bougé → les badges « En lecture » sont mis à jour en
  // place, sans reconstruction (scroll et sélecteurs préservés).
  const structure = JSON.stringify({
    available: result.available,
    error: result.error,
    playbackDevices: result.playbackDevices,
    recordingDevices: result.recordingDevices,
    sessions: result.sessions.map(({ playing, ...rest }) => rest),
  });
  if (appsStructureSignature === structure) {
    appsSignature = signature;
    updatePlayingBadges(list, result.sessions);
    return;
  }
  appsSignature = signature;
  appsStructureSignature = structure;
  list.innerHTML = "";
  banner.classList.add("hidden");
  empty.classList.add("hidden");
  if (!result.available || result.error) {
    banner.classList.remove("hidden");
    must("apps-banner-text").textContent =
      result.error || "Routage par application indisponible sur ce système.";
    return;
  }
  if (!result.sessions.length) {
    empty.classList.remove("hidden");
    return;
  }
  for (const session of result.sessions) {
    const item = document.createElement("div");
    item.className = "app-item";
    item.dataset.pid = String(session.pid);
    const info = document.createElement("div");
    info.className = "app-info";
    const nameRow = document.createElement("span");
    nameRow.className = "app-name-row";
    const name = document.createElement("span");
    name.className = "app-name";
    name.title = session.executablePath ?? "";
    name.textContent = session.processName;
    nameRow.appendChild(name);
    if (session.playing) {
      const playing = document.createElement("span");
      playing.className = "app-playing";
      playing.textContent = "En lecture";
      playing.title = "Cette application joue (ou enregistre) du son actuellement.";
      nameRow.appendChild(playing);
    }
    const detail = document.createElement("span");
    detail.className = "app-detail";
    detail.textContent = session.executablePath
      ? `${session.executablePath} • PID ${session.pid}`
      : `PID ${session.pid}`;
    info.append(nameRow, detail);
    const routes = document.createElement("div");
    routes.className = "app-routes";
    routes.append(
      routeSelect("output", result.playbackDevices, session.output, session.pid, session.processName),
      routeSelect("input", result.recordingDevices, session.input, session.pid, session.processName),
    );
    item.append(info, routes);
    list.appendChild(item);
  }
}

must("apps-refresh").addEventListener("click", () => loadAppSessions().catch(reportError));

// Section « Routage par application » de l'aperçu d'un profil.
function previewAppsSection(apps) {
  const title = document.createElement("div");
  title.className = "preview-apps-title";
  title.textContent = "Routage par application";
  const rows = apps.map((app) => {
    const row = document.createElement("div");
    row.className = "preview-row preview-app";
    const head = document.createElement("div");
    head.className = "preview-app-head";
    const name = document.createElement("b");
    name.title = app.executablePath ?? "";
    name.textContent = app.processName;
    head.appendChild(name);
    head.appendChild(
      app.running
        ? chip("Active", "good")
        : chip("Application non active", "bad"),
    );
    row.appendChild(head);
    for (const flow of [
      ["output", "Sortie"],
      ["input", "Entrée"],
    ]) {
      const target = app[flow[0]];
      if (!target) continue;
      const line = document.createElement("span");
      line.className = "preview-app-flow";
      line.textContent = `${flow[1]} : ${target.deviceName || target.deviceId}`;
      row.appendChild(line);
      if (!target.present) {
        line.appendChild(document.createTextNode(" "));
        line.appendChild(chip("Périphérique absent", "bad"));
      }
    }
    return row;
  });
  return [title, ...rows];
}

async function loadOverview() {
  try {
    const overview = await withTimeout(
      invoke("overview"),
      10000,
      "La lecture de l'état audio a expiré (10 s).",
    );
    const playback = overview.defaultPlayback;
    const recording = overview.defaultRecording;
    const playbackEl = must("playback-summary");
    const inputEl = must("input-summary");
    playbackEl.textContent = playback?.name ?? "Aucune sortie détectée";
    playbackEl.title = playback?.name ?? "";
    playbackEl.classList.toggle("value-empty", !playback);
    inputEl.textContent = recording?.name ?? "Aucune entrée détectée";
    inputEl.title = recording?.name ?? "";
    inputEl.classList.toggle("value-empty", !recording);
    must("playback-detail").textContent = `${overview.playbackCount} sortie(s) détectée(s)`;
    must("input-detail").textContent = `${overview.recordingCount} entrée(s) détectée(s)`;

    const banner = must("module-banner");
    banner.classList.toggle("hidden", overview.moduleAvailable);
    readyBadge.textContent = overview.moduleAvailable
      ? "● Module prêt"
      : "● Module manquant";
    readyBadge.style.color = overview.moduleAvailable ? "var(--green)" : "var(--yellow)";
  } catch (err) {
    readyBadge.textContent = "● Indisponible";
    readyBadge.style.color = "var(--red)";
    setStatus(String(err), "error");
    must("module-banner").classList.remove("hidden");
    must("module-banner-text").textContent = String(err);
  }
}

async function loadProfiles() {
  const list = must("profiles-list");
  list.innerHTML = "";
  try {
    const result = await withTimeout(
      invoke("profiles_folder"),
      10000,
      "La lecture du dossier de profils a expiré (10 s).",
    );
    must("profiles-folder-label").textContent = result.path;
    const countEl = must("profiles-count");
    if (result.profiles.length) {
      countEl.textContent = String(result.profiles.length);
      countEl.classList.remove("value-empty");
    } else {
      countEl.textContent = "Aucun profil";
      countEl.classList.add("value-empty");
    }
    if (!result.profiles.length) {
      must("profiles-empty").classList.remove("hidden");
      return;
    }
    must("profiles-empty").classList.add("hidden");
    for (const profile of result.profiles) {
      const item = document.createElement("div");
      item.className = "profile-item";
      const info = document.createElement("div");
      info.className = "profile-info";
      const name = document.createElement("span");
      name.className = "profile-name";
      name.title = profile.path;
      name.textContent = profile.name;
      const detail = document.createElement("span");
      detail.className = "profile-detail";
      detail.textContent = `${formatDate(profile.modified)} • ${formatSize(profile.size)}`;
      info.append(name, detail);

      const actions = document.createElement("div");
      actions.className = "profile-actions";
      const restore = document.createElement("button");
      restore.type = "button";
      restore.className = "btn btn-primary";
      restore.textContent = "Restaurer";
      restore.addEventListener("click", () => openPreview(profile).catch(reportError));
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "btn btn-ghost btn-danger-ghost";
      remove.textContent = "Supprimer";
      remove.addEventListener("click", () => confirmDelete(profile));
      actions.append(restore, remove);
      item.append(info, actions);
      list.appendChild(item);
    }
  } catch (err) {
    must("profiles-empty").classList.remove("hidden");
    must("profiles-empty").textContent = String(err);
  }
}

// Modales : ouverture/fermeture générique (focus restauré à la fermeture).
const modalOpeners = new Map();

function openModal(modalId, cancelId) {
  modalOpeners.set(modalId, document.activeElement);
  must(modalId).classList.remove("hidden");
  must(cancelId).focus();
}

function closeModal(modalId) {
  must(modalId).classList.add("hidden");
  const opener = modalOpeners.get(modalId);
  modalOpeners.delete(modalId);
  if (opener instanceof HTMLElement) opener.focus();
}

async function openPreview(profile) {
  previewPath = profile.path;
  must("preview-file").textContent = profile.name;
  const content = must("preview-content");
  content.innerHTML = "";
  try {
    const preview = await invoke("preview_profile", { path: profile.path });
    const rows = [
      deviceRow("Lecture par défaut", preview.playbackName ? {
        name: preview.playbackName,
        volume: preview.playbackVolume,
      } : null, preview.playbackFound),
      deviceRow("Enregistrement par défaut", preview.recordingName ? {
        name: preview.recordingName,
        volume: preview.recordingVolume,
      } : null, preview.recordingFound),
    ];
    if (Array.isArray(preview.applications) && preview.applications.length) {
      rows.push(...previewAppsSection(preview.applications));
    }
    content.append(...rows);
  } catch (err) {
    content.innerHTML = "";
    const error = document.createElement("p");
    error.className = "preview-error";
    error.textContent = String(err);
    content.appendChild(error);
  }
  openModal("preview-modal", "preview-cancel");
}

must("preview-cancel").addEventListener("click", () => closeModal("preview-modal"));

must("preview-confirm").addEventListener("click", async () => {
  if (!previewPath) return;
  const path = previewPath;
  closeModal("preview-modal");
  setStatus("Restauration en cours…");
  await runBusy(async () => {
    try {
      const result = await invoke("restore_profile", { path });
      toast(result.message, "success");
      setStatus(result.message, "success");
      loadProfiles();
      loadOverview();
    } catch (err) {
      reportError(err);
    }
  });
});

function confirmDelete(profile) {
  confirmPath = profile.path;
  must("confirm-file").textContent = profile.name;
  openModal("confirm-modal", "confirm-cancel");
}

must("confirm-cancel").addEventListener("click", () => closeModal("confirm-modal"));

must("confirm-ok").addEventListener("click", async () => {
  if (!confirmPath) return;
  const path = confirmPath;
  closeModal("confirm-modal");
  await runBusy(async () => {
    try {
      const result = await invoke("delete_profile", { path });
      toast(result.message, "success");
      loadProfiles();
      loadOverview();
    } catch (err) {
      reportError(err);
    }
  });
});

async function doSaveProfile(reloadList) {
  setStatus("Sauvegarde du profil en cours…");
  await runBusy(async () => {
    try {
      const result = await invoke("save_profile");
      if (result.saved) {
        toast(result.message, "success");
        setStatus(result.message, "success");
        if (reloadList) loadProfiles();
      } else {
        setStatus(result.message);
      }
    } catch (err) {
      reportError(err);
    }
  });
}

must("new-profile").addEventListener("click", () => doSaveProfile(true));

must("save-profile-card").addEventListener("click", () => doSaveProfile(false));

must("go-profiles-card").addEventListener("click", () => switchView("profiles"));

must("import-profile").addEventListener("click", async () => {
  await runBusy(async () => {
    try {
      const result = await invoke("import_profile");
      if (result.imported) {
        toast(result.message, "success");
        loadProfiles();
      } else if (result.message) {
        setStatus(result.message);
      }
    } catch (err) {
      reportError(err);
    }
  });
});

must("open-folder").addEventListener("click", async () => {
  try {
    await invoke("open_profiles_folder");
  } catch (err) {
    toast(String(err), "error");
  }
});

async function pickProfilesFolder(reloadList) {
  await runBusy(async () => {
    try {
      // `choose_profiles_folder` renvoie les réglages, mais loadSettingsForm
      // les recharge déjà : la valeur de retour est ignorée.
      await invoke("choose_profiles_folder");
      if (reloadList) loadProfiles();
      loadSettingsForm();
      toast("Dossier des profils modifié", "success");
    } catch (err) {
      toast(String(err), "error");
    }
  });
}

must("choose-folder").addEventListener("click", () => pickProfilesFolder(true));

async function loadSettingsForm() {
  try {
    currentSettings = await invoke("settings");
  } catch (err) {
    toast(String(err), "error");
    return;
  }
  must("profiles-folder-input").value = currentSettings.profilesFolder;
  must("backup-before-restore").checked = currentSettings.backupBeforeRestore;
  must("auto-save-on-start").checked = currentSettings.autoSaveOnStart;
  must("watch-devices").checked = currentSettings.watchDevices;
  must("keep-versions").value = String(currentSettings.keepVersions);
  must("settings-status").textContent = "";
}

must("pick-folder").addEventListener("click", () => pickProfilesFolder(false));

must("save-settings").addEventListener("click", async () => {
  if (!currentSettings) return;
  await runBusy(async () => {
    must("settings-status").textContent = "Enregistrement…";
    try {
      currentSettings = await invoke("update_settings", {
        newSettings: {
          profilesFolder: must("profiles-folder-input").value.trim(),
          backupBeforeRestore: must("backup-before-restore").checked,
          autoSaveOnStart: must("auto-save-on-start").checked,
          keepVersions: Number(must("keep-versions").value) || 0,
          watchDevices: must("watch-devices").checked,
        },
      });
      must("settings-status").textContent = "Paramètres enregistrés.";
      must("settings-status").style.color = "var(--green)";
      toast("Paramètres enregistrés", "success");
      loadOverview();
    } catch (err) {
      must("settings-status").textContent = String(err);
      must("settings-status").style.color = "var(--red)";
      toast(String(err), "error");
    }
  });
});

must("install-module").addEventListener("click", async () => {
  const button = must("install-module");
  button.disabled = true;
  const original = button.textContent;
  button.textContent = "Installation en cours…";
  setStatus("Installation du module AudioDeviceCmdlets… (peut prendre plusieurs minutes)");
  try {
    const result = await invoke("install_audio_module");
    toast(result.message, "success");
    setStatus(result.message, "success");
    loadOverview();
  } catch (err) {
    reportError(err);
  } finally {
    button.textContent = original;
    button.disabled = false;
  }
});

// Clavier : Échap ferme la modale ouverte ; clic sur le voile aussi.
document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;
  for (const id of ["preview-modal", "confirm-modal"]) {
    if (!must(id).classList.contains("hidden")) {
      closeModal(id);
      return;
    }
  }
});
for (const id of ["preview-modal", "confirm-modal"]) {
  must(id).addEventListener("click", (event) => {
    if (event.target !== must(id)) return;
    closeModal(id);
  });
}

// Événements émis par le backend (les chargeurs gèrent déjà leurs erreurs,
// le catch n'est qu'un filet anti-rejet non géré).
listen("profiles-changed", () => {
  loadProfiles().catch(reportError);
});

listen("devices-changed", (event) => {
  const backup = event.payload?.backup;
  toast(
    backup
      ? `Périphériques modifiés — sauvegarde créée : ${backup.split(/[\\/]/).pop()}`
      : "Périphériques modifiés",
    "info",
  );
  loadOverview().catch(reportError);
});

// Couleur d'accent système : remplace les variables CSS `--accent*`.
async function applySystemAccent() {
  try {
    const hex = await withTimeout(invoke("system_accent_color"), 4000, "Accent indisponible");
    if (!hex) return;
    const n = parseInt(hex.slice(1), 16);
    const r = (n >> 16) & 255;
    const g = (n >> 8) & 255;
    const b = n & 255;
    // Luminance relative approximative → texte lisible sur l'accent.
    const luminance = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
    const root = document.documentElement.style;
    root.setProperty("--accent", hex);
    root.setProperty("--on-accent", luminance > 0.45 ? "#101010" : "#ffffff");
  } catch {
    // Accent par défaut du thème conservé.
  }
}

// Mica : si le backend a rendu le WebView transparent, surfaces adaptées.
async function applyBackdrop() {
  try {
    const enabled = await withTimeout(invoke("backdrop_enabled"), 4000, "Backdrop indisponible");
    document.documentElement.classList.toggle("backdrop", enabled === true);
  } catch {
    document.documentElement.classList.remove("backdrop");
  }
}

// Démarrage
applySystemAccent();
applyBackdrop();
loadOverview().catch((err) => toast(`Vue d'ensemble indisponible : ${err}`, "error"));
loadProfiles().catch((err) => toast(`Profils indisponibles : ${err}`, "error"));