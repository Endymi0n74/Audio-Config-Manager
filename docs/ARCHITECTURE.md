# Architecture

## Vue d'ensemble

```
┌─────────────────────────────────────────────────────────────┐
│  Frontend (src/)  — HTML/CSS/JS, webview Tauri                │
│  index.html · style.css · main.js                             │
│  → invoke("overview" | "save_profile" | "restore_profile" |   │
│           "preview_profile" | "delete_profile" |              │
│           "import_profile" | "profiles_folder" |              │
│           "settings" | "update_settings" | ...)               │
└───────────────────────────┬─────────────────────────────────┘
                            │ IPC Tauri (JSON)
┌───────────────────────────▼─────────────────────────────────┐
│  Backend Rust (src-tauri/src/)                                 │
│                                                                 │
│  main.rs         → point d'entrée, commandes, veille périph.    │
│  commands.rs     → surface exposée au frontend (#[tauri::command])│
│  settings.rs     → paramètres (profilesFolder, backupBefore… )  │
│  profiles.rs     → liste/noms uniques/rétention des profils     │
│  ps.rs           → moteur PowerShell (script embarqué)          │
│  app_routing.rs  → routage audio PAR APPLICATION (Rust pur,     │
│                    API interne Windows AudioPolicyConfig)       │
│  appearance.rs   → Mica/arrière-plan + couleur d'accent système │
│                                                                 │
│  audio-config-manager.ps1  → script PS embarqué dans la binaire│
│    (Get-AudioDevice / Set-AudioDevice du module                │
│     AudioDeviceCmdlets) — overview, export, preview, restore     │
└─────────────────────────────────────────────────────────────┘
```

L'application est **Windows uniquement** : le moteur audio *système*
(défauts, volumes, listes) est PowerShell + le module AudioDeviceCmdlets
de Microsoft. Le **routage par application** — que le module ne couvre
pas — est en Rust pur, via l'API interne de Windows (`AudioPolicyConfig`),
le même mécanisme qu'EarTrumpet ou SoundVolumeView.

## Le moteur PowerShell (`ps.rs` + `audio-config-manager.ps1`)

Le script est embarqué dans l'exécutable (`include_str!`) et écrit dans
`%APPDATA%\Audio Config Manager\audio-config-manager.ps1` au premier
lancement. Chaque opération est une invocation :

```
powershell.exe -NoProfile -ExecutionPolicy Bypass -File <script> \
    -Action <overview|export|preview|restore> -ConfigPath <profil.json>
```

- `overview` — périphériques par défaut (ID, nom, volume) + compteurs
  lecture/enregistrement + disponibilité du module (`moduleAvailable`).
  (La liste des périphériques actifs est 100 % Rust : `current_devices`
  → `app_routing::active_devices`, COM direct, aucun appel PowerShell.)
- `export` — écrit un profil JSON (Metadata + PlaybackDevices +
  RecordingDevices + DefaultPlayback + DefaultRecording), schéma v3.1.
- `preview` — lit un profil et indique si ses périphériques par défaut
  existent sur la machine courante.
- `restore` — applique les périphériques par défaut puis les volumes de
  tous les périphériques du profil ; renvoie `applied` et `missing`.

`ps.rs` gère le délai d'exécution (60 s, 10 min pour l'installation du
module), la capture stdout/stderr sans deadlock (lecture en threads) et
l'analyse de la réponse JSON. Les messages d'erreur reprennent ceux de
l'original : « PowerShell indisponible », « Réponse audio invalide »,
« Le module AudioDeviceCmdlets n'est pas installé », etc.

## Routage par application (`app_routing.rs`)

Le routage « Xbox → une autre carte son », implanté en **Rust pur** —
aucun crate Windows externe : FFI direct sur
`combase`/`ole32`/`kernel32`, comme `appearance.rs`.

L'API utilisée est l'interface interne **`Windows.Media.Internal.
AudioPolicyConfig`** (implémentée dans `AudioSes.dll`) — le même chemin
que EarTrumpet / SoundVolumeView / winappaudiorouter :

- activation de la fabrique (`RoGetActivationFactory`) avec l'IID
  `ab3d4648-…` (Win11 ≥ 21H2 ; `2a59116d-…` avant) sur un **thread STA**
  dédié (« audio-policy-config »), la classe étant déclarée STA ;
- emplacements de vtable **25 / 26** = `Set`/`Get`
  `PersistedDefaultAudioEndpoint(pid, dataFlow, role, deviceId)` —
  l'identifiant est « emballé » (`\\?\SWD#MMDEVAPI#…`) et la route est
  écrite pour les rôles console **et** multimédia, comme le réglage de
  Windows ; `Get` sans route persistée renvoie `ERROR_NOT_FOUND`
  (0x80070490) — l'application suit alors le périphérique par défaut ;
- les **sessions audio actives** sont énumérées via l'API publique
  WASAPI en FFI direct (`IMMDeviceEnumerator → IAudioSessionManager2`,
  comme pycaw), pour retrouver les processus qui jouent de l'audio et
  lire/écrire leur route persistée.

Rôles dans le flux de profil :

- **export** — après l'écriture du JSON par PowerShell,
  `attach_applications_to_profile` ajoute la section `applications`
  (processus actifs ayant une route personnalisée). Le nombre
  d'applications enregistrées est ajouté au message (« N application(s)
  routée(s) »). Non bloquant : sans cette section, le profil reste
  valide.
- **aperçu** — `preview_applications` indique pour chaque application si
  elle tourne sur la machine et si ses périphériques cibles y existent.
- **restauration** — `restore_applications` retrouve chaque application
  parmi les processus en cours (chemin d'exécutable puis nom, comme
  l'original) et réécrit ses routes persistées. Applications inactives ou
  périphériques absents → liste `missing`, sans annuler le reste.

Limitations connues : la persistance est refusée par le système en
**session distante** (RDP / audio redirigé) avec `ERROR_NOT_SUPPORTED`
(0x80070032) — le code le détecte et le signale proprement ; en session
locale, la route s'applique comme dans le panneau « Périphériques » de
Windows.

## Paramètres (`settings.rs`)

Persistés dans `%APPDATA%\Audio Config Manager\settings.json` — mêmes
clés que l'original :

| Clé                    | Type    | Défaut | Rôle                                        |
|------------------------|---------|--------|---------------------------------------------|
| `profilesFolder`       | string  | Documents\Audio Profiles | dossier des profils        |
| `backupBeforeRestore`  | bool    | true   | sauvegarde « Avant restauration … » avant restore |
| `autoSaveOnStart`      | bool    | false  | export automatique au démarrage             |
| `keepVersions`         | number  | 10     | versions horodatées conservées (0 = toutes) |
| `watchDevices`         | bool    | false  | sauvegarde à chaque changement de périphérique par défaut |

## Dossier de profils (`profiles.rs`)

- `list_profiles` — profils `.json` triés par date de modification
  (champs `name`, `path`, `modified`, `size`).
- `unique_path` — noms sans collision (« nom (2).json », …).
- `prune_versions` — rétention : ne garde que les `keepVersions`
  sauvegardes horodatées les plus récentes (`2026-09-07 08-00-00.json`,
  `Avant restauration …`).

## Flux des opérations

### Sauvegarde (`save_profile`)

```
choix du fichier (dialogue natif, filtre « Profil audio JSON »)
  → script -Action export -ConfigPath <fichier>
  → attach_applications_to_profile (moteur Rust : sessions actives +
    routes persistées → section « applications » du JSON)
  → message « Profil sauvegardé » (+ « · N application(s) routée(s) »)
  → événement "profiles-changed" (la liste se rafraîchit)
```

### Aperçu (`preview_profile`)

```
vérifie que le fichier existe (« Le profil n'existe plus »)
  → script -Action preview → { playbackName, playbackVolume,
    playbackFound, recordingName, recordingVolume, recordingFound }
  → si le profil a une section « applications » : preview_applications
    (application en cours d'exécution ? périphérique cible présent ?)
  → l'interface affiche « Introuvable sur cette machine » si besoin
```

### Restauration (`restore_profile`)

```
si backupBeforeRestore :
  export courant → « Avant restauration <date>.json » (dossier de profils)
  rétention keepVersions
script -Action restore (défauts + volumes)
  → si le profil a une section « applications » : restore_applications
    (moteur Rust, API interne Windows — routes persistées par processus)
  → message « Configuration restaurée » (+ « · N élément(s) absent(s) »)
```

### Import (`import_profile`)

```
choix du fichier (dialogue natif)
  → validation : JSON avec une clé « Metadata »
  → copie dans le dossier de profils (nom unique)
```

### Veille sur les périphériques (`watchDevices`, dans `main.rs`)

Un thread interroge `overview` toutes les 3 s ; si les périphériques par
défaut changent, une sauvegarde horodatée est créée et les événements
`profiles-changed` / `devices-changed` sont émis vers l'interface.

## Correspondance avec l'application de référence (v3.1)

| Référence (exe)                        | Ce projet                          |
|-----------------------------------------|------------------------------------|
| script `audio-config-manager-v3.ps1`    | `src-tauri/audio-config-manager.ps1` |
| `powershell.exe -Action overview/export/preview/restore` | `ps.rs` |
| commandes `save_profile`, `restore_profile`, … | `commands.rs` (mêmes noms) |
| `settings.json` (%APPDATA%\Audio Config Manager) | `settings.rs` |
| dossier « Audio Profiles » + profils JSON | `profiles.rs` |
| `%Y-%m-%d %H-%M-%S` pour les sauvegardes | `timestamp()` dans `commands.rs` |
| module AudioDeviceCmdlets + installation | `install_audio_module` |
| routage par application | `app_routing.rs` (Rust pur, `AudioPolicyConfig`) |
| interface graphique | `src/index.html` + `style.css` + `main.js` |

## État du routage par application

Implanté et testé (suite `cargo test`, dont un test matériel `--ignored`
qui fait l'aller-retour complet activation → session → set → get →
clear). La **persistance de la route** peut être refusée par le système
en session distante (0x80070032) — le comportement vient de l'OS, pas
d'un défaut d'ABI. Le test matériel le considère comme un SKIP dans ce
cas et s'exécute pleinement en session locale.

La section « applications » est exportée, prévisualisée et restaurée
avec les profils, et une **vue de gestion dédiée** (navigation
« Applications ») liste les processus audio actifs avec un sélecteur
de périphérique (sortie/entrée) par application : le changement écrit
directement la route persistée via `set_app_route` (PID + flux +
périphérique, ou `null` pour revenir au périphérique système), sans
passer par un profil.

## CLI de débogage (sous-commande `route` du binaire principal)

La CLI de routage par application est une **sous-commande de l'exécutable
principal** (`src/route_cli.rs`, autrefois un binaire séparé
`src/bin/route.rs` → `route.exe`) : `"Audio Config Manager.exe" route
sessions`, `route devices [output|input]`, `route get <process>` et
`route set <process> <output|input> <device|system>` (« system » efface
la route). Le processus se donne par PID ou nom d'exécutable ; le
périphérique par identifiant, nom exact ou sous-chaîne unique. Les noms
de périphériques sont lus sans PowerShell via
`PKEY_Device_FriendlyName` (`list_active_devices`, FFI IPropertyStore) —
une fonction réutilisable par l'application plus tard.

Comme l'exe est lié en subsystem GUI, `route_cli::attach_console`
rattache la console du parent (`AttachConsole(ATTACH_PARENT_PROCESS)`)
ou en alloue une (`AllocConsole`) avant toute sortie — aucune fenêtre de
terminal n'apparaît au lancement normal de l'application. Le binaire
n'est pas copié dans `dist/` en tant que tel : c'est l'exe principal qui
emporte la CLI, donc rien à distribuer en plus.

## Tests de bout en bout (vue Applications)

Deux niveaux, tous deux basés sur un **processus audio factice**
(`src/bin/fakeaudio.rs` → `src-tauri/target/release/fakeaudio.exe`) : un
petit binaire Rust qui ouvre une vraie session WASAPI de rendu (léger
bourdonnement inaudible) et apparaît donc comme une application audio
active. Il n'est pas copié dans `dist/` (outil de test, non distribué).

- **Test Rust** `e2e_apps_view_set_clear_missing` (dans `app_routing.rs`,
  marqué `#[ignore]`, « test matériel ») : lance fakeaudio, vérifie que sa
  session apparaît, puis **set** (route écrite et relue), **clear** (route
  effacée) et l'état **introuvable** — soit une route vers un périphérique
  inactif persistée sans nom résolu (`device_name=None`), soit, quand l'OS
  refuse (E_INVALIDARG 0x80070057 — cas constaté sur ce poste), la preuve du
  garde-fou. Lancement :
  `cargo test --bin audio-config-manager -- --ignored --nocapture e2e_apps_view_set_clear_missing`.
- **Test frontend** `e2e/apps-view.e2e.mjs` : pilote la vraie application
  via CDP (WebSocket natif Node) avec fakeaudio lancé — vérifie la ligne
  « fakeaudio.exe » avec l'indicateur « En lecture », un **set** réel à
  travers l'UI (persisté côté backend), un **clear** réel, puis le rendu
  **« (introuvable) »** quand la route pointe vers un périphérique absent
  (SKIP si la machine n'a aucun périphérique désactivé). Lancement :
  `node e2e/apps-view.e2e.mjs`.

Contrainte matérielle constatée : Windows **refuse** de router par
application vers un identifiant hors liste active (0x80070057, vérifié pour
un id d'entrée, un GUID inexistant et un périphérique désactivé) — une route
ne devient donc « introuvable » que lorsque son périphérique disparaît après
coup, ce que la vue gère en affichant l'option « (introuvable) ».