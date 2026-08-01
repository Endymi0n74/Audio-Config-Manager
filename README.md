# Audio Config Manager 1.0

![Audio Config Manager](assets/brand-logo.png)

Application Windows 11 pour sauvegarder et restaurer les périphériques audio,
leurs volumes et les choix de sortie/entrée propres à chaque application.

## Fonctionnalités

- sauvegarde JSON portable et versionnée ;
- historique automatique des sauvegardes ;
- restauration sélective avec aperçu ;
- volumes de lecture et d’enregistrement, y compris `0` ;
- périphériques par défaut standard et communications ;
- routage de sortie et d’entrée par application ;
- correspondance par identifiant puis nom exact ;
- vérification après restauration ;
- rapport détaillé exportable ;
- diagnostic et installation guidée d’`AudioDeviceCmdlets` ;
- recherche des mises à jour publiées sur GitHub.

## Prérequis

- Windows 11 ;
- Windows PowerShell 5.1 ;
- Python 3.10+ pour exécuter le source ;
- module PowerShell `AudioDeviceCmdlets`.

L’interface propose l’installation du module lorsqu’il manque. Installation
manuelle possible :

```powershell
Install-Module AudioDeviceCmdlets -Scope CurrentUser
```

## Développement

```powershell
python -m pip install -r requirements.txt
python -m unittest -v test_audio_gui.py
python audio_gui.py
```

## Construction de l’EXE

```powershell
./build.ps1
```

Le fichier final est créé dans `dist/Audio Config Manager.exe`.

## Limite Windows

Une application doit être lancée pendant la sauvegarde pour que sa préférence
audio soit détectée, et pendant la restauration pour que Windows puisse la
réassocier. Certaines applications doivent redémarrer leur lecture ou capture
avant d’utiliser le nouvel endpoint.

Le routage utilise l’interface de politique audio interne de Windows employée
par des outils comme EarTrumpet. Cette interface n’est pas officiellement
stabilisée par Microsoft.
