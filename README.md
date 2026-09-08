# Audio Config Manager

Sauvegarde et restauration de la configuration audio Windows : périphériques
par défaut (sortie/entrée), volumes et **routage audio par application**,
sous forme de **profils JSON** gérés dans un dossier de profils.

Il s'agit de la réécriture Rust/Tauri de
[Audio_config_manager_Windows](https://github.com/Endymi0n74/Audio_config_manager_Windows)
(originellement Python/Tkinter), portée à l'identique de l'application
de référence **« Audio Config Manager » v3.1** : le moteur audio est le
même, un script PowerShell embarqué qui s'appuie sur le module
[AudioDeviceCmdlets](https://www.powershellgallery.com/packages/AudioDeviceCmdlets)
de Microsoft.

## Documentation

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — organisation du
  projet, moteur PowerShell, commandes, dossier de profils et paramètres.
- **[docs/SCHEMA.md](docs/SCHEMA.md)** — référence du format JSON des profils.
- **[docs/BUILD.md](docs/BUILD.md)** — compiler, développer, tester.

## En bref

- **Moteur PowerShell + AudioDeviceCmdlets** : le script
  `src-tauri/audio-config-manager.ps1` est embarqué dans l'exécutable et
  invoqué via `powershell.exe -NoProfile -ExecutionPolicy Bypass -File …
  -Action overview|export|preview|restore`. C'est exactement l'architecture
  de l'application originale qui fonctionne.
- **Dossier de profils** : les profils `.json` sont créés, importés,
  restaurés et supprimés dans un dossier dédié (par défaut
  `Documents\Audio Profiles`), ouvrable dans l'Explorateur.
- **Sauvegardes automatiques** : avant chaque restauration, au démarrage,
  et en veille sur les changements de périphériques (réglables dans
  Paramètres, avec rétention `keepVersions`).
- **Aucune autre dépendance externe** : la première utilisation propose
  d'installer le module `AudioDeviceCmdlets` (bouton dédié), comme
  l'original.

## Compiler

**Windows uniquement** :

```powershell
npm install
npm run tauri dev      # mode développement
.\build-release.ps1   # build release (portable) — une seule commande
```

`build-release.ps1` fait tout d'une traite : il lance `npm run tauri build`
(qui compile via le CLI Tauri et **embarque le frontend** `src/` dans
l'exécutable), compile le binaire de test `fakeaudio.exe`, puis copie
**l'unique exécutable livré** dans `dist/` — `Audio Config Manager.exe` —
et vérifie enfin que l'exe est lié en mode GUI (aucune fenêtre de
terminal au lancement).

Un seul exe, donc : la CLI de débogage du routage par application est
une **sous-commande du binaire principal**
(`"Audio Config Manager.exe" route sessions|devices|get|set` — la
console est rattachée automatiquement), et `fakeaudio.exe` (faux
processus audio pour les tests E2E) reste dans
`src-tauri/target/release/`, où les tests le recherchent — voir
[docs/BUILD.md](docs/BUILD.md).

Le build produit des **exécutables portables** (pas d'installeur), qui se
lancent directement sans installation — seule condition : le runtime
WebView2 de Microsoft (présent par défaut sur Windows 10/11). Détails
complets, prérequis et dépannage : voir [docs/BUILD.md](docs/BUILD.md).

## Fonctionnalités

- **Vue d'ensemble** : périphériques par défaut (sortie/entrée), volumes,
  nombre de périphériques détectés, état du module AudioDeviceCmdlets
  (avec bouton d'installation si absent).
- **Routage par application** : vue « Applications » listant les processus
  audio actifs, avec sélection du périphérique de sortie/entrée par
  application (moteur natif Rust via
  `Windows.Media.Internal.AudioPolicyConfig` — le même mécanisme
  qu'EarTrumpet), indicateur « En lecture » et rafraîchissement
  automatique sans scintillement.
- **Profils** : création (« Nouveau profil » → boîte de dialogue native),
  liste (nom, date, taille), **aperçu avant restauration** (périphériques
  par défaut enregistrés et leur présence sur la machine), restauration,
  importation d'un profil JSON, suppression, ouverture du dossier dans
  l'Explorateur, choix du dossier.
- **Restauration** : applique les périphériques par défaut et les volumes ;
  les périphériques introuvables sont signalés
  (« Configuration restaurée · N périphérique(s) absent(s) »).
- **Sauvegardes automatiques** : `backupBeforeRestore` (profil
  « Avant restauration … » avant chaque restauration), `autoSaveOnStart`
  (export au démarrage), `watchDevices` (export lors d'un changement de
  périphérique par défaut), avec rétention `keepVersions`.
- **Paramètres** persistés dans `%APPDATA%\Audio Config Manager\settings.json`.

## Commandes Tauri

`settings`, `update_settings`, `overview`, `save_profile`,
`preview_profile`, `restore_profile`, `delete_profile`, `import_profile`,
`profiles_folder`, `profile_path`, `choose_profiles_folder`,
`open_profiles_folder`, `install_audio_module`, `app_sessions`,
`set_app_route` — les deux derniers gèrent le routage par application ;
les autres reprennent les noms et comportements de l'application de
référence.

## Icônes

Le jeu complet d'icônes (Windows, macOS, tailles multiples, logos
Microsoft Store) est généré depuis `src/assets/app-icon.png` avec
`npx tauri icon src/assets/app-icon.png` — les fichiers produits sont
versionnés dans `src-tauri/icons/`.

## Ancienne version Python

Les fichiers `audio_gui.py`, `test_audio_gui.py`, `build.ps1` et
`requirements.txt` de la version Python d'origine sont conservés à la
racine à titre de référence ; ils ne font plus partie du build.