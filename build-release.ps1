# ============================================================
# Audio Config Manager — build release (une seule commande)
#
#   .\build-release.ps1
#
# Étapes (l'ordre est important — voir « Piège » plus bas) :
#   0. Arrête l'application et fakeaudio.exe s'ils tournent (évite
#      l'échec de copie sur exe verrouillé et les processus zombies ;
#      avertissement dédié si un test E2E tournait).
#   1. `cargo build --release --bin fakeaudio` → compile le binaire
#      compagnon de test (sans toucher à l'exe principal).
#   2. `npm run tauri build` → compile le binaire PRINCIPAL en release
#      via le CLI Tauri. C'est lui qui active le feature
#      `custom-protocol` de Tauri, indispensable pour EMBARQUER le
#      frontend `src/` dans l'exécutable.
#   3. Copie l'exe principal livré dans `dist/` (seul binaire à
#      distribuer : route.exe et fakeaudio.exe restent des outils de dev
#      dans `target/release/`, où les tests E2E les attendent).
#   4. Vérifications : signature PE valide + subsystem = GUI (2) sur
#      l'exe final, et frontend embarqué via le test Rust d'intégration
#      `frontend_is_embedded_via_custom_protocol` (aucune fenêtre de
#      terminal, aucun écran ERR_CONNECTION_REFUSED).
#
# PIÈGE (historique) : ne PAS lancer `cargo build --release --bins`
# APRÈS `npm run tauri build` — `--bins` recompilait l'exe principal
# SANS `custom-protocol`, écrasait le bon binaire et donnait un exe qui
# essaie de charger le serveur de dev (localhost:1420) → écran
# « localhost a refusé de se connecter ». Depuis que `custom-protocol`
# est une feature PAR DÉFAUT (Cargo.toml), tout `cargo build --release`
# embarque le frontend ; l'ordre ci-dessus reste le plus sûr et le test
# Rust garde l'anti-régression.
#
# Résultat : `dist\Audio Config Manager.exe`, prêt à être copié/distribué
# (la CLI `route` est une sous-commande de l'exe principal ; fakeaudio.exe
# reste dans `target\release\` pour les tests).
# ============================================================
$ErrorActionPreference = 'Stop'

$root   = $PSScriptRoot
$target = Join-Path $root 'src-tauri\target\release'
$dist   = Join-Path $root 'dist'

Write-Host ''
Write-Host '==> 0/5 Arret des processus qui verrouilleraient les exes' -ForegroundColor Cyan
# Application principale (deux noms possibles : l'exe livré dans dist/
# s'appelle « Audio Config Manager », celui de target/ « audio-config-manager »).
$appNames = @('Audio Config Manager', 'audio-config-manager')
$appProcs = @(Get-Process -Name $appNames -ErrorAction SilentlyContinue)
if ($appProcs.Count -gt 0) {
    foreach ($p in $appProcs) {
        Write-Host ("   arret du PID {0} ({1})" -f $p.Id, $p.ProcessName) -ForegroundColor Yellow
    }
    Stop-Process -Name $appNames -Force -ErrorAction SilentlyContinue
    Wait-Process -Name $appNames -Timeout 15 -ErrorAction SilentlyContinue
    Write-Host '   application arretee' -ForegroundColor Green
} else {
    Write-Host '   aucune instance de l''application en cours' -ForegroundColor Green
}

# fakeaudio.exe (helper de test E2E) : s'il tourne, sa copie dans dist/
# echouerait (fichier verrouille). Arret avec avertissement dedie : un test
# E2E en cours serait interrompu.
$fakeProcs = @(Get-Process -Name 'fakeaudio' -ErrorAction SilentlyContinue)
if ($fakeProcs.Count -gt 0) {
    Write-Host '   AVERTISSEMENT : fakeaudio.exe tournait (test E2E eventuellement interrompu) - arret pour pouvoir remplacer l''exe' -ForegroundColor Yellow
    foreach ($p in $fakeProcs) {
        Write-Host ("      arret du PID {0} ({1})" -f $p.Id, $p.ProcessName) -ForegroundColor Yellow
    }
    Stop-Process -Name 'fakeaudio' -Force -ErrorAction SilentlyContinue
    Wait-Process -Name 'fakeaudio' -Timeout 15 -ErrorAction SilentlyContinue
    Write-Host '   fakeaudio arrete' -ForegroundColor Green
}

Write-Host ''
Write-Host '==> 1/5 Build du binaire compagnon (fakeaudio)' -ForegroundColor Cyan
Push-Location (Join-Path $root 'src-tauri')
try {
    cargo build --release --bin fakeaudio
    if ($LASTEXITCODE -ne 0) { throw "cargo build (fakeaudio) a echoue (code $LASTEXITCODE)" }
} finally {
    Pop-Location
}

Write-Host ''
Write-Host '==> 2/5 Build Tauri (release, frontend embarque)' -ForegroundColor Cyan
Push-Location $root
try {
    npm run tauri build
    if ($LASTEXITCODE -ne 0) { throw "npm run tauri build a echoue (code $LASTEXITCODE)" }
} finally {
    Pop-Location
}

Write-Host ''
Write-Host '==> 3/5 Copie de l''exe principal vers dist/' -ForegroundColor Cyan
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$src = Join-Path $target 'audio-config-manager.exe'
$dst = Join-Path $dist 'Audio Config Manager.exe'
if (-not (Test-Path $src)) { throw "Binaire introuvable : $src (build incomplet ?)" }
try {
    Copy-Item -Force $src $dst -ErrorAction Stop
    Write-Host "   dist\Audio Config Manager.exe" -ForegroundColor Green
} catch {
    throw "Copie de $dst impossible : $($_.Exception.Message) - fermez l'application (l'exe est verrouille s'il est en cours d'execution)."
}

# Les builds antérieurs copiaient aussi route.exe et fakeaudio.exe dans
# dist/ : ce sont des outils de developpement (CLI de debug et faux
# processus audio pour les tests E2E), pas des binaires a distribuer. Ils
# restent dans target/release/ (ou les tests les cherchent) ; on supprime
# les copies obsolete de dist/ pour garder un livrable mono-exe.
foreach ($legacy in @('route.exe', 'fakeaudio.exe')) {
    $stale = Join-Path $dist $legacy
    if (Test-Path $stale) {
        Remove-Item -Force $stale
        Write-Host "   suppression de l'obsolete dist\$legacy" -ForegroundColor Yellow
    }
}

Write-Host ''
Write-Host '==> 4/5 Verifications (signature PE + GUI + frontend embarque)' -ForegroundColor Cyan
$exe = Join-Path $dist 'Audio Config Manager.exe'
$bytes = [System.IO.File]::ReadAllBytes($exe)

# 4a. Signature PE valide (MZ, PE\0\0, magic PE32/PE32+) + subsystem = 2 (GUI) :
#     executable authentique, aucune fenetre de terminal au lancement.
function Assert-ValidPeGui {
    param([byte[]]$Bytes)
    # En-tete DOS : 'MZ' (0x4D 0x5A).
    if ($Bytes.Length -lt 0x40 -or $Bytes[0] -ne 0x4D -or $Bytes[1] -ne 0x5A) {
        throw 'Signature DOS (MZ) absente : fichier non PE'
    }
    $peOff = [BitConverter]::ToInt32($Bytes, 0x3c)
    if ($peOff -lt 0x40 -or ($peOff + 0x60) -gt $Bytes.Length) {
        throw ("Offset header PE invalide (0x{0:X})" -f $peOff)
    }
    # Signature 'PE\0\0' (0x50 0x45 0x00 0x00).
    if (-not ($Bytes[$peOff] -eq 0x50 -and $Bytes[$peOff + 1] -eq 0x45 -and $Bytes[$peOff + 2] -eq 0 -and $Bytes[$peOff + 3] -eq 0)) {
        throw 'Signature PE\0\0 absente : fichier non PE'
    }
    # Magic de l'optional header : 0x10B (PE32) ou 0x20B (PE32+).
    $magic = [BitConverter]::ToUInt16($Bytes, $peOff + 0x18)
    if ($magic -ne 0x10B -and $magic -ne 0x20B) {
        throw ("Magic optional header invalide (0x{0:X}) : PE32 (0x10B) ou PE32+ (0x20B) attendu" -f $magic)
    }
    # Subsystem : offset 0x5C dans l'optional header (identique PE32/PE32+) ; 2 = GUI.
    $subsystem = [BitConverter]::ToUInt16($Bytes, $peOff + 0x5C)
    if ($subsystem -ne 2) {
        throw "Subsystem = $subsystem (attendu 2 = GUI) : une console s'ouvrirait au lancement !"
    }
    return $subsystem
}
$null = Assert-ValidPeGui -Bytes $bytes
Write-Host '   signature PE valide + subsystem = 2 (GUI) - aucun terminal au lancement' -ForegroundColor Green

# 4b. Frontend embarque : test Rust d'integration (anti-regression
#     ERR_CONNECTION_REFUSED). Il echoue si le feature `custom-protocol`
#     disparait de Cargo.toml ou si le frontend (src/) est absent/vide.
Write-Host '   test Rust : frontend_is_embedded_via_custom_protocol' -ForegroundColor Cyan
Push-Location (Join-Path $root 'src-tauri')
try {
    cargo test --bin audio-config-manager frontend_is_embedded_via_custom_protocol
    if ($LASTEXITCODE -ne 0) {
        throw "Test frontend_is_embedded_via_custom_protocol ECHOUE (code $LASTEXITCODE) : custom-protocol absent ou frontend non embarque."
    }
} finally {
    Pop-Location
}
Write-Host '   frontend embarque (custom-protocol) - pas de ERR_CONNECTION_REFUSED' -ForegroundColor Green

Write-Host ''
Write-Host 'OK : release prete dans dist/' -ForegroundColor Green