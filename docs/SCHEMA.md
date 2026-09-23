# Schéma des profils audio

Format produit par le script PowerShell embarqué
(`src-tauri/audio-config-manager.ps1`, fonction `Export-Profile`) et
lu par `preview`/`restore` — identique à l'application de référence
« Audio Config Manager » v3.1, **complété** par une section
`applications` (routage par application) ajoutée par le moteur Rust.

```jsonc
{
  "Metadata": {
    "ComputerName": "PC-STEPHANE",
    "Timestamp": "2026-09-07T08:00:00.0000000+02:00",   // ISO 8601
    "Version": "3.1"
  },
  "PlaybackDevices": [
    { "ID": "{0.0.0.00000000}.{aaaa-bbbb-cccc-dddd}", "Name": "Casque USB", "Volume": 42.0 }
  ],
  "RecordingDevices": [
    { "ID": "{0.0.1.00000000}.{eeee-ffff-0000-1111}", "Name": "Micro USB", "Volume": 80.0 }
  ],
  "DefaultPlayback": { "ID": "…", "Name": "Casque USB", "Volume": 42.0 },
  "DefaultRecording": { "ID": "…", "Name": "Micro USB", "Volume": 80.0 },
  "applications": [                       // ← ajouté par ce projet (section optionnelle)
    {
      "processName": "XboxApp.exe",
      "executablePath": "C:\\Program Files\\…\\XboxApp.exe",
      "output": { "deviceId": "{0.0.0.00000000}.{aaaa-bbbb-cccc-dddd}", "deviceName": "Carte son 2" },
      "input":  { "deviceId": "{0.0.1.00000000}.{eeee-ffff-0000-1111}", "deviceName": "Micro USB" }
    }
  ]
}
```

## Champs

- **`Metadata`** — informatifs (machine d'origine, horodatage, version
  du format). C'est la clé dont la présence valide un fichier à
  l'importation (« Ce fichier n'est pas un profil JSON valide » sinon).
- **`PlaybackDevices` / `RecordingDevices`** — tous les périphériques
  *actifs* au moment de l'export, avec leur volume maître (`0.0` à
  `100.0`). `Volume: null` signifie que le volume n'a pas pu être lu
  (périphérique désactivé entre-temps) — ce n'est pas une erreur.
- **`DefaultPlayback` / `DefaultRecording`** — les périphériques par
  défaut du système, avec leur volume.
- **`applications`** — section **optionnelle** ajoutée par ce projet :
  le routage
  persisté de chaque application détectée. Une entrée possède le nom du
  processus (`processName`), son chemin d'exécutable (`executablePath`),
  et ses routes `output` / `input` (chacune : `deviceId` + `deviceName`
  lisible). Une application sans route personnalisée (qui suit le
  périphérique par défaut) n'apparaît pas. Un profil sans cette section
  (exporté par la v3.1, par exemple) reste parfaitement valide et
  restaurable.

## Restauration

Lors d'une restauration, chaque périphérique enregistré est retrouvé par
**identifiant (`ID`) puis par nom exact** (`Find-Device` dans le script) ;
un périphérique absent de la machine courante est signalé dans la liste
`missing` (« N périphérique(s) absent(s) ») sans faire échouer le reste.
Les volumes sont appliqués via `Set-AudioDevice -Volume`, les défauts
via `Set-AudioDevice -DefaultOnly`.

Les routes par application sont restaurées par le moteur Rust
(`app_routing/`, API interne `AudioPolicyConfig`) : chaque application
est retrouvée parmi les **processus en cours** (chemin d'exécutable
d'abord, nom ensuite), puis ses routes persistées sont réécrites pour
chaque flux. Une application qui ne tourne pas, ou dont le périphérique
cible n'existe plus sur la machine, est signalée dans `missing` sans
annuler le reste.

## Compatibilité

Un profil exporté par l'application de référence v3.1 (ou par ce projet)
s'importe tel quel, et réciproquement — le script et le format sont
identiques. La section `applications` est ignorée par l'application de
référence si un tel profil lui est rechargé (le routage par application
y était géré séparément, au moment de la sauvegarde, par le module
`winappaudiorouter`).
