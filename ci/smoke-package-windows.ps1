$ErrorActionPreference = 'Stop'

$installers = @(Get-ChildItem -LiteralPath 'target\release\bundle\nsis' -Filter '*-setup.exe')
if ($installers.Count -ne 1) {
  throw "Expected exactly one NSIS installer, found $($installers.Count)."
}

$uninstallRoot = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall'
function Get-SwitcherRegistrations {
  return @(
    Get-ChildItem -LiteralPath $uninstallRoot -ErrorAction SilentlyContinue |
      Where-Object {
        $_.GetValue('DisplayName') -eq 'Codex Provider Switcher'
      }
  )
}

$existingRegistrations = @(Get-SwitcherRegistrations)
if ($existingRegistrations.Count -gt 1) {
  throw "Expected at most one existing registration, found $($existingRegistrations.Count)."
}

$registrationBackup = $null
$registrationKeyName = $null
$registrationBackupSha256 = $null
if ($existingRegistrations.Count -eq 1) {
  $registrationKeyName = $existingRegistrations[0].PSChildName
  $registrationNativePath =
    "HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\$registrationKeyName"
  $registrationBackup = Join-Path $env:RUNNER_TEMP 'existing-registration.reg'
  & reg.exe export $registrationNativePath $registrationBackup /y | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw 'Could not back up the existing uninstall registration.'
  }
  $registrationBackupSha256 =
    (Get-FileHash -Algorithm SHA256 -LiteralPath $registrationBackup).Hash
}

$installerStatePath =
  'HKCU:\Software\codex-provider-switcher\Codex Provider Switcher'
$installerStateNativePath =
  'HKEY_CURRENT_USER\Software\codex-provider-switcher\Codex Provider Switcher'
$installerStateBackup = $null
$installerStateBackupSha256 = $null
if (Test-Path -LiteralPath $installerStatePath -PathType Container) {
  $installerStateBackup =
    Join-Path $env:RUNNER_TEMP 'existing-installer-state.reg'
  & reg.exe export $installerStateNativePath $installerStateBackup /y | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw 'Could not back up the existing installer state.'
  }
  $installerStateBackupSha256 =
    (Get-FileHash -Algorithm SHA256 -LiteralPath $installerStateBackup).Hash
}

$shortcutPaths = @(
  (Join-Path $env:APPDATA `
    'Microsoft\Windows\Start Menu\Programs\Codex Provider Switcher.lnk'),
  (Join-Path ([Environment]::GetFolderPath('Desktop')) `
    'Codex Provider Switcher.lnk')
)
$shortcutSnapshots = @()
for ($index = 0; $index -lt $shortcutPaths.Count; $index++) {
  $path = $shortcutPaths[$index]
  $backup = $null
  $sha256 = $null
  if (Test-Path -LiteralPath $path -PathType Leaf) {
    $backup = Join-Path $env:RUNNER_TEMP "existing-shortcut-$index.lnk"
    Copy-Item -LiteralPath $path -Destination $backup
    $sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash
  }
  $shortcutSnapshots += [pscustomobject]@{
    path = $path
    backup = $backup
    sha256 = $sha256
  }
}

$installRoot = Join-Path $env:RUNNER_TEMP 'codex-provider-switcher-install'
if (Test-Path -LiteralPath $installRoot) {
  throw "Clean install target already exists: $installRoot"
}

try {
  $installer = Start-Process -FilePath $installers[0].FullName `
    -ArgumentList @('/S', "/D=$installRoot") -PassThru -Wait
  if ($installer.ExitCode -ne 0) {
    throw "NSIS installer exited with $($installer.ExitCode)."
  }

  $app = Join-Path $installRoot 'codex-provider-switcher.exe'
  $uninstallerPath = Join-Path $installRoot 'uninstall.exe'
  if (-not (Test-Path -LiteralPath $app -PathType Leaf) -or
      -not (Test-Path -LiteralPath $uninstallerPath -PathType Leaf)) {
    throw 'NSIS did not install the application and uninstaller.'
  }
  $installedAppSha256 =
    (Get-FileHash -Algorithm SHA256 -LiteralPath $app).Hash.ToLowerInvariant()

  $uninstaller = Start-Process -FilePath $uninstallerPath `
    -ArgumentList '/S' -PassThru -Wait
  if ($uninstaller.ExitCode -ne 0) {
    throw "NSIS uninstaller exited with $($uninstaller.ExitCode)."
  }
  if (Test-Path -LiteralPath $app) {
    throw 'NSIS uninstall left the application executable behind.'
  }
}
finally {
  foreach ($registration in @(Get-SwitcherRegistrations)) {
    $nativePath =
      "HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\$($registration.PSChildName)"
    & reg.exe delete $nativePath /f | Out-Null
    if ($LASTEXITCODE -ne 0) {
      throw 'Could not remove the package-smoke uninstall registration.'
    }
  }
  if ($registrationBackup) {
    & reg.exe import $registrationBackup | Out-Null
    if ($LASTEXITCODE -ne 0) {
      throw 'Could not restore the existing uninstall registration.'
    }
  }
  if (Test-Path -LiteralPath $installerStatePath -PathType Container) {
    & reg.exe delete $installerStateNativePath /f | Out-Null
    if ($LASTEXITCODE -ne 0) {
      throw 'Could not remove the package-smoke installer state.'
    }
  }
  if ($installerStateBackup) {
    & reg.exe import $installerStateBackup | Out-Null
    if ($LASTEXITCODE -ne 0) {
      throw 'Could not restore the existing installer state.'
    }
  }

  foreach ($snapshot in $shortcutSnapshots) {
    if (Test-Path -LiteralPath $snapshot.path -PathType Leaf) {
      Remove-Item -LiteralPath $snapshot.path -Force
    }
    if ($snapshot.backup) {
      Copy-Item -LiteralPath $snapshot.backup -Destination $snapshot.path
    }
  }
}

$restoredRegistrations = @(Get-SwitcherRegistrations)
if ($registrationBackup) {
  if ($restoredRegistrations.Count -ne 1 -or
      $restoredRegistrations[0].PSChildName -ne $registrationKeyName) {
    throw 'Package smoke did not restore the original uninstall registration.'
  }
  $registrationVerification =
    Join-Path $env:RUNNER_TEMP 'restored-registration.reg'
  & reg.exe export $registrationNativePath $registrationVerification /y | Out-Null
  if ($LASTEXITCODE -ne 0 -or
      (Get-FileHash -Algorithm SHA256 -LiteralPath $registrationVerification).Hash -ne
        $registrationBackupSha256) {
    throw 'Restored uninstall registration does not match its backup.'
  }
}
elseif ($restoredRegistrations.Count -ne 0) {
  throw 'Package smoke left an unexpected uninstall registration.'
}

if ($installerStateBackup) {
  if (-not (Test-Path -LiteralPath $installerStatePath -PathType Container)) {
    throw 'Package smoke did not restore the original installer state.'
  }
  $installerStateVerification =
    Join-Path $env:RUNNER_TEMP 'restored-installer-state.reg'
  & reg.exe export `
    $installerStateNativePath $installerStateVerification /y | Out-Null
  if ($LASTEXITCODE -ne 0 -or
      (Get-FileHash -Algorithm SHA256 `
        -LiteralPath $installerStateVerification).Hash -ne
        $installerStateBackupSha256) {
    throw 'Restored installer state does not match its backup.'
  }
}
elseif (Test-Path -LiteralPath $installerStatePath -PathType Container) {
  throw 'Package smoke left unexpected installer state.'
}

foreach ($snapshot in $shortcutSnapshots) {
  $exists = Test-Path -LiteralPath $snapshot.path -PathType Leaf
  if ($snapshot.backup) {
    if (-not $exists -or
        (Get-FileHash -Algorithm SHA256 -LiteralPath $snapshot.path).Hash -ne
          $snapshot.sha256) {
      throw "Package smoke did not restore shortcut $($snapshot.path)."
    }
  }
  elseif ($exists) {
    throw "Package smoke left an unexpected shortcut $($snapshot.path)."
  }
}

[pscustomobject]@{
  packageSmoke = $true
  existingRegistrationPreserved = [bool]$registrationBackup
  existingInstallerStatePreserved = [bool]$installerStateBackup
  existingShortcutsPreserved = @(
    $shortcutSnapshots | Where-Object { $_.backup }
  ).Count
  installedAppSha256 = $installedAppSha256
} | ConvertTo-Json -Compress
