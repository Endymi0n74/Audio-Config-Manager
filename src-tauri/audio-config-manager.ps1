param([Parameter(Mandatory=$true)][string]$Action,[string]$ConfigPath)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
function Module-Ready { return [bool](Get-Module -ListAvailable -Name AudioDeviceCmdlets) }
function Get-DefaultDevice([string]$Kind) {
  Import-Module AudioDeviceCmdlets -ErrorAction SilentlyContinue
  try { if ($Kind -eq 'Playback') { return Get-AudioDevice -Playback } else { return Get-AudioDevice -Recording } }
  catch { return $null }
}
function Overview {
  if (-not (Module-Ready)) { @{moduleAvailable=$false;playbackCount=0;recordingCount=0;defaultPlayback=$null;defaultRecording=$null} | ConvertTo-Json -Depth 6 -Compress; return }
  Import-Module AudioDeviceCmdlets
  $all = @(Get-AudioDevice -List)
  $p = @($all | Where-Object Type -eq 'Playback')
  $r = @($all | Where-Object Type -eq 'Recording')
  $dp = Get-DefaultDevice 'Playback'
  $dr = Get-DefaultDevice 'Recording'
  $dpj = if ($dp) { @{ID=$dp.ID;Name=$dp.Name;Volume=$dp.Volume} } else { $null }
  $drj = if ($dr) { @{ID=$dr.ID;Name=$dr.Name;Volume=$dr.Volume} } else { $null }
  @{moduleAvailable=$true;playbackCount=$p.Count;recordingCount=$r.Count;defaultPlayback=$dpj;defaultRecording=$drj} | ConvertTo-Json -Depth 6 -Compress
}
function Export-Profile {
  if (-not (Module-Ready)) { throw "Le module AudioDeviceCmdlets n’est pas installé." }
  Import-Module AudioDeviceCmdlets
  $all = @(Get-AudioDevice -List)
  $playback = @($all | Where-Object Type -eq 'Playback' | ForEach-Object { $v = try {(Get-AudioDevice -ID $_.ID).Volume} catch {$null}; @{ID=$_.ID;Name=$_.Name;Volume=$v} })
  $recording = @($all | Where-Object Type -eq 'Recording' | ForEach-Object { $v = try {(Get-AudioDevice -ID $_.ID).Volume} catch {$null}; @{ID=$_.ID;Name=$_.Name;Volume=$v} })
  $dp = Get-DefaultDevice 'Playback'; $dr = Get-DefaultDevice 'Recording'
  $dpj = if ($dp) { @{ID=$dp.ID;Name=$dp.Name;Volume=$dp.Volume} } else { $null }
  $drj = if ($dr) { @{ID=$dr.ID;Name=$dr.Name;Volume=$dr.Volume} } else { $null }
  $config = @{Metadata=@{ComputerName=$env:COMPUTERNAME;Timestamp=(Get-Date).ToString('o');Version='3.1'};PlaybackDevices=$playback;RecordingDevices=$recording;DefaultPlayback=$dpj;DefaultRecording=$drj}
  $config | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $ConfigPath -Encoding UTF8
  @{ok=$true;message='Profil sauvegardé'} | ConvertTo-Json -Compress
}
function Find-Device($list,$saved){ return $list | Where-Object { $_.ID -eq $saved.ID -or $_.Name -eq $saved.Name } | Select-Object -First 1 }
function Devices {
  if (-not (Module-Ready)) { @{moduleAvailable=$false;playbackDevices=@();recordingDevices=@()} | ConvertTo-Json -Depth 5 -Compress; return }
  Import-Module AudioDeviceCmdlets
  $all = @(Get-AudioDevice -List)
  $p = @($all | Where-Object Type -eq 'Playback' | ForEach-Object { @{ID=$_.ID;Name=$_.Name} })
  $r = @($all | Where-Object Type -eq 'Recording' | ForEach-Object { @{ID=$_.ID;Name=$_.Name} })
  @{moduleAvailable=$true;playbackDevices=$p;recordingDevices=$r} | ConvertTo-Json -Depth 5 -Compress
}
function Preview-Profile {
  if (-not (Module-Ready)) { throw "Le module AudioDeviceCmdlets n’est pas installé." }
  Import-Module AudioDeviceCmdlets
  $config = Get-Content -LiteralPath $ConfigPath -Raw -Encoding UTF8 | ConvertFrom-Json
  $all = @(Get-AudioDevice -List)
  $dp = $config.DefaultPlayback; $dr = $config.DefaultRecording
  $fp = if($dp){[bool](Find-Device $all $dp)}else{$false}
  $fr = if($dr){[bool](Find-Device $all $dr)}else{$false}
  @{playbackName=if($dp){$dp.Name}else{$null};playbackVolume=if($dp){$dp.Volume}else{$null};playbackFound=$fp;recordingName=if($dr){$dr.Name}else{$null};recordingVolume=if($dr){$dr.Volume}else{$null};recordingFound=$fr} | ConvertTo-Json -Depth 5 -Compress
}
function Restore-Profile {
  if (-not (Module-Ready)) { throw "Le module AudioDeviceCmdlets n’est pas installé." }
  Import-Module AudioDeviceCmdlets
  $config = Get-Content -LiteralPath $ConfigPath -Raw -Encoding UTF8 | ConvertFrom-Json
  $all = @(Get-AudioDevice -List); $applied = 0; $missing = @()
  if ($config.DefaultPlayback) { $d=Find-Device $all $config.DefaultPlayback; if($d){Set-AudioDevice -ID $d.ID -DefaultOnly; if($null -ne $config.DefaultPlayback.Volume){Set-AudioDevice -ID $d.ID -Volume $config.DefaultPlayback.Volume};$applied++}else{$missing += $config.DefaultPlayback.Name} }
  if ($config.DefaultRecording) { $d=Find-Device $all $config.DefaultRecording; if($d){Set-AudioDevice -ID $d.ID -DefaultOnly; if($null -ne $config.DefaultRecording.Volume){Set-AudioDevice -ID $d.ID -Volume $config.DefaultRecording.Volume};$applied++}else{$missing += $config.DefaultRecording.Name} }
  foreach($saved in @($config.PlaybackDevices)+@($config.RecordingDevices)){ $d=Find-Device $all $saved; if($d -and $null -ne $saved.Volume){try{Set-AudioDevice -ID $d.ID -Volume $saved.Volume;$applied++}catch{}} }
  @{ok=$true;applied=$applied;missing=$missing;message="$applied réglage(s) appliqué(s)"} | ConvertTo-Json -Depth 5 -Compress
}
switch($Action){'overview'{Overview};'devices'{Devices};'export'{Export-Profile};'preview'{Preview-Profile};'restore'{Restore-Profile};default{throw 'Action inconnue'}}