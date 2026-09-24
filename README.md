# Audio Config Manager

[![Build status](https://github.com/Endymi0n74/Audio-Config-Manager/actions/workflows/build.yml/badge.svg)](https://github.com/Endymi0n74/Audio-Config-Manager/actions/workflows/build.yml)
[![Télécharger](https://img.shields.io/badge/T%C3%A9l%C3%A9charger-Windows-2ea44f?style=flat-square)](https://github.com/Endymi0n74/Audio-Config-Manager/releases/latest)

**🇫🇷 Français** · [🇬🇧 English](README.en.md)

Gestion de la configuration audio Windows : périphériques par défaut
(sortie/entrée), volumes et **routage audio par application**, sauvegardés
dans des **profils JSON**.

![Vue d'ensemble](docs/screens/overview.png)

## Fonctionnalités

- **Profils** : sauvegarde et restauration des périphériques par défaut
  (sortie/entrée) et des volumes ; aperçu avant restauration ; import/export
  JSON ; sauvegardes automatiques (avant restauration, au démarrage, sur
  changement de périphérique) avec rétention. La détection des changements
  est **event-native** (`IMMNotificationClient`, abonnement COM) : zéro
  processus d'arrière-plan, détection instantanée.
- **Routage par application** : choisis le périphérique de sortie et
  d'entrée de chaque application (ex. *Xbox → une autre carte son*), via
  l'API interne de Windows (`AudioPolicyConfig`).
- **Vue d'ensemble** : défauts, volumes et compteurs de périphériques lus
  **directement en COM** (sans PowerShell) en ~10 ms ; installation du
  module AudioDeviceCmdlets en un clic (détection par check fichiers).
- **Interface** : Mica, thème clair/sombre automatique, couleur d'accent
  système, version suivie automatiquement (aucun numéro codé en dur).

![Applications](docs/screens/apps.png)

## Télécharger

Exécutable portable (aucune installation) sur la page
[Releases](https://github.com/Endymi0n74/Audio-Config-Manager/releases) —
seule condition : le runtime WebView2 (présent par défaut sur Windows 10/11).
Chaque release est buildée et publiée automatiquement par GitHub Actions.

## Compiler

Windows uniquement :

```powershell
npm install
npm run tauri dev      # développement
.\build-release.ps1    # build release — une seule commande
```

La CLI de débogage du routage est une sous-commande du binaire principal :
`"Audio Config Manager.exe" route sessions|devices|get|set`.

## Documentation

- [Architecture](docs/ARCHITECTURE.md) · [Format des profils](docs/SCHEMA.md) · [Build](docs/BUILD.md)
