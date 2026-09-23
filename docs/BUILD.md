# Compiler et développer

**Windows uniquement** : le moteur audio repose sur PowerShell et le
module AudioDeviceCmdlets (Windows PowerShell 5.1, présent par défaut
sur Windows 10/11).

## Prérequis

- [Rust](https://rustup.rs/) (édition 2021, `rust-version = "1.75"` minimum)
- [Node.js](https://nodejs.org/) 18+ (pour l'outillage frontend Tauri)
- Les [outils de build Visual Studio](https://tauri.app/start/prerequisites/)
  (workload « Desktop development with C++ »)

## Développement

```powershell
npm install
npm run tauri dev
```

Ouvre la fenêtre avec rechargement à chaud du frontend (`src/`) ; les
changements Rust nécessitent un redémarrage (`Ctrl+C` puis relancer).

## Build release (exécutable portable)

**Une seule commande** :

```powershell
.\build-release.ps1
```

Ce script enchaîne toutes les étapes du package :

1. `cargo build --release --bin fakeaudio` — compile le binaire de test
   (sans toucher à l'exe principal). C'est un outil de développement :
   il n'est pas copié dans `dist/`.
2. `npm run tauri build` — compile le binaire principal en release via
   le CLI Tauri, qui **embarque le frontend `src/`** dans l'exécutable.
   Le feature **`custom-protocol`** de Tauri (indispensable pour
   embarquer) est désormais activé **par défaut** dans `Cargo.toml` :
   même un simple `cargo build --release` produit un exe complet. Sans
   ce feature, l'exe essaierait de charger le serveur de dev
   (`localhost:1420`) → écran `ERR_CONNECTION_REFUSED` au lancement.
3. Copie l'unique exécutable livré dans `dist/` :
   `Audio Config Manager.exe`. Les copies obsolètes de `route.exe` /
   `fakeaudio.exe` laissées par d'anciens builds y sont supprimées.
4. Vérifie l'exe final : **signature PE valide** (en-tête DOS `MZ`,
   signature `PE\0\0`, magic PE32/PE32+) et subsystem = **GUI (2)**
   (aucune fenêtre de terminal), puis lance le **test Rust
   d'intégration** `frontend_is_embedded_via_custom_protocol` qui
   garantit que le frontend est bien **embarqué** (`custom-protocol`
   actif) — anti-régression `ERR_CONNECTION_REFUSED`.

Le projet est volontairement configuré **sans installeur**
(`bundle.targets` vide dans `tauri.conf.json`) : le build produit
uniquement des exécutables portables qui se lancent sans installation.

Produits :
- `dist/Audio Config Manager.exe` — l'exécutable à distribuer, prêt à
  l'emploi, à la racine du projet ;
- `src-tauri/target/release/fakeaudio.exe` — processus audio de test pour
  les E2E (développement uniquement, non livré).

Les tests E2E (Rust et Node) cherchent `fakeaudio.exe` directement dans
`src-tauri/target/release/` : il n'a donc pas besoin d'être dans `dist/`,
et `dist/` ne contient que le binaire à distribuer.

## CLI de débogage `route`

La CLI de routage par application est une **sous-commande du binaire
principal** (plus de `route.exe` séparé) :

```powershell
"dist\Audio Config Manager.exe" route sessions
"dist\Audio Config Manager.exe" route devices [output|input]
"dist\Audio Config Manager.exe" route get <process>
"dist\Audio Config Manager.exe" route set <process> <output|input> <device|system>
```

L'exécutable étant lié en subsystem GUI, il **rattache la console** du
terminal appelant (ou en alloue une au double-clic) avant d'écrire — les
sorties `sessions`/`devices`/`get`/`set` s'affichent normalement dans
cmd/PowerShell.

> **À retenir** : `npm run tauri build` à lui seul **ne copie pas** dans
> `dist/` — il ne produit que `target/release/audio-config-manager.exe`.
> Utilisez `build-release.ps1` pour obtenir le package complet.

L'exécutable embarque le frontend (`src/`) **et** le script PowerShell
(`src-tauri/audio-config-manager.ps1`, écrit dans le dossier de
configuration au premier lancement). Seul le runtime WebView2 de
Microsoft est requis (présent par défaut sur Windows 10/11).

## Tests

```powershell
npm run check     # syntaxe du frontend (node --check)
npm run test      # cargo test : 32 tests, 2 ignorés (matériel, --ignored)
npm run clippy    # clippy --all-targets -- -D warnings (requis par la CI)
npm run e2e       # vue Applications via CDP (nécessite fakeaudio compilé)
```

Les tests unitaires couvrent la logique sans matériel audio :
chargement/enregistrement des paramètres, listage et rétention des
profils, noms de fichiers uniques, validation des profils importés et
parsing des réponses du script. Deux tests matériels (`--ignored`,
`live_policy_round_trip` et `e2e_apps_view_set_clear_missing`) exigent
un vrai périphérique audio + `fakeaudio.exe` compilé
(`cargo build --release --bin fakeaudio`) ; le scénario « introuvable »
de la vue Applications est couvert côté frontend par `e2e/apps-view.e2e.mjs`.
Le comportement audio réel (module AudioDeviceCmdlets) se vérifie en
lançant l'application.

## CI

`.github/workflows/build.yml` tourne sur `windows-latest` : installe
Rust + Node (cache `Swatinem/rust-cache`), lance `node --check`,
`cargo test`, `clippy -D warnings`, puis `build-release.ps1`, publie
`dist/` en artefact et attache l'exécutable à une GitHub Release sur
les tags `v*`.

## Dépannage

- **« Le module AudioDeviceCmdlets n'est pas installé. »** — cliquer sur
  « Installer le module » dans l'application (installe NuGet puis le
  module depuis PSGallery). Vérifiable en PowerShell :
  `Get-Module -ListAvailable AudioDeviceCmdlets`.
- **« PowerShell indisponible »** — `powershell.exe` introuvable ou
  bloqué par une politique (penser à `-ExecutionPolicy Bypass`, déjà
  passé par l'application).
- **« Dossier de configuration introuvable »** — variable `APPDATA`
  absente ; l'application ne peut pas créer
  `%APPDATA%\Audio Config Manager\settings.json`.