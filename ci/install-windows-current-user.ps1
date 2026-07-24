#requires -Version 5.1

[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$InstallerPath,

  [Parameter(Mandatory = $true)]
  [ValidatePattern('^[0-9a-fA-F]{64}$')]
  [string]$InstallerSha256,

  [string]$ExpectedUser = 'Admin',

  [switch]$AllowUpgrade,

  [string]$ExpectedVersion = '',

  [ValidatePattern('^$|^[0-9a-fA-F]{64}$')]
  [string]$ExpectedAppSha256 = ''
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$account = $identity.Name
if ($account.Split('\')[-1] -ne $ExpectedUser) {
  throw "refusing to install as unexpected account $account"
}
if ($env:USERPROFILE -ne "C:\Users\$ExpectedUser") {
  throw "refusing unexpected user profile $env:USERPROFILE"
}
if ($env:LOCALAPPDATA -notlike "$env:USERPROFILE\*") {
  throw "LOCALAPPDATA is outside the expected user profile: $env:LOCALAPPDATA"
}
if ($env:CODEX_HOME) {
  throw 'CODEX_HOME must not be overridden during a per-user installation'
}
if (@(Get-AppxPackage -Name OpenAI.Codex -ErrorAction SilentlyContinue).Count -ne 1) {
  throw 'the current Windows user does not have exactly one OpenAI.Codex package'
}
if (Get-Process -Name codex-provider-switcher -ErrorAction SilentlyContinue) {
  throw 'Codex Provider Switcher is already running'
}
$desiredInstall = Join-Path $env:LOCALAPPDATA 'Codex Provider Switcher'

$installer = (Resolve-Path -LiteralPath $InstallerPath).Path
$actualSha256 =
  (Get-FileHash -Algorithm SHA256 -LiteralPath $installer).Hash.ToLowerInvariant()
if ($actualSha256 -ne $InstallerSha256.ToLowerInvariant()) {
  throw 'installer SHA-256 does not match the approved artifact'
}

$existing = @(
  Get-ItemProperty `
    'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*' `
    -ErrorAction SilentlyContinue |
    Where-Object { $_.DisplayName -eq 'Codex Provider Switcher' }
)
if ($existing.Count -gt 1) {
  throw "expected at most one existing registration; found $($existing.Count)"
}
if ($existing.Count -eq 1 -and -not $AllowUpgrade) {
  throw 'Codex Provider Switcher is already registered; use -AllowUpgrade after closing it'
}
if ($AllowUpgrade -and (-not $ExpectedVersion -or -not $ExpectedAppSha256)) {
  throw 'an upgrade requires both -ExpectedVersion and -ExpectedAppSha256'
}
if ($existing.Count -eq 0 -and (Test-Path -LiteralPath $desiredInstall)) {
  throw "refusing an unregistered existing installation path: $desiredInstall"
}

function Get-RegistrationInstallLocation($registration) {
  $location = ([string]$registration.InstallLocation).Trim([char]34)
  if ($location) {
    return $location
  }
  $uninstallString = $registration.UninstallString
  if ($uninstallString -match '^"([^"]+)"') {
    $uninstallPath = $matches[1]
  }
  else {
    $uninstallPath = ($uninstallString -split '\s+')[0]
  }
  return Split-Path -Parent $uninstallPath
}

function Get-StateFingerprint {
  $targets = @(
    (Join-Path $env:USERPROFILE '.codex\config.toml'),
    (Join-Path $env:USERPROFILE '.codex\provider-switcher')
  )
  $entries = @()
  foreach ($target in $targets) {
    if (Test-Path -LiteralPath $target -PathType Leaf) {
      $entries += "$target|$((Get-FileHash -Algorithm SHA256 -LiteralPath $target).Hash)"
    }
    elseif (Test-Path -LiteralPath $target -PathType Container) {
      foreach ($file in @(Get-ChildItem -LiteralPath $target -File -Force -Recurse |
          Sort-Object FullName)) {
        $relative = $file.FullName.Substring($target.Length)
        $entries += "$target$relative|$((Get-FileHash -Algorithm SHA256 -LiteralPath $file.FullName).Hash)"
      }
    }
  }
  return @($entries)
}

function Get-OptionalRegistryValue([string]$Path, [string]$Name) {
  if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
    return $null
  }
  $item = Get-ItemProperty -LiteralPath $Path -ErrorAction Stop
  $property = $item.PSObject.Properties[$Name]
  if ($null -eq $property) {
    return $null
  }
  if ($property.Value -is [byte[]]) {
    return [Convert]::ToBase64String($property.Value)
  }
  return $property.Value
}

function Get-CredentialMetadata {
  return [string]((& cmdkey.exe /list) -join "`n")
}

$installerStatePath =
  'HKCU:\Software\codex-provider-switcher\Codex Provider Switcher'
function Get-SavedInstallLocation {
  if (-not (Test-Path -LiteralPath $installerStatePath -PathType Container)) {
    return $null
  }
  return [string](Get-Item -LiteralPath $installerStatePath).GetValue('')
}

$beforeState = @(Get-StateFingerprint)
$beforeCredentialMetadata = Get-CredentialMetadata
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$startupApprovedKey =
  'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run'
$beforeAutostart = Get-OptionalRegistryValue `
  $runKey 'Codex Provider Switcher'
$beforeStartupApproved = Get-OptionalRegistryValue `
  $startupApprovedKey 'Codex Provider Switcher'

if ($existing.Count -eq 1) {
  $existingLocation = (Resolve-Path -LiteralPath (
    Get-RegistrationInstallLocation $existing[0]
  )).Path
  if (-not $existingLocation.Equals(
      $desiredInstall,
      [StringComparison]::OrdinalIgnoreCase
  )) {
    throw "refusing to upgrade an unexpected installation path: $existingLocation"
  }
  $savedInstallLocation = Get-SavedInstallLocation
  if (-not $savedInstallLocation) {
    throw 'refusing to upgrade without the installer saved path'
  }
  $resolvedSavedInstall = (Resolve-Path -LiteralPath $savedInstallLocation).Path
  if (-not $resolvedSavedInstall.Equals(
      $desiredInstall,
      [StringComparison]::OrdinalIgnoreCase
    )) {
    throw "refusing unexpected installer saved path: $resolvedSavedInstall"
  }
  $installerArguments = @('/S', '/UPDATE', "/D=$desiredInstall")
}
else {
  if (Get-SavedInstallLocation) {
    throw 'refusing an unregistered installation with saved installer state'
  }
  $installerArguments = @('/S', "/D=$desiredInstall")
}

if (Get-Process -Name codex-provider-switcher -ErrorAction SilentlyContinue) {
  throw 'Codex Provider Switcher was reopened before installation could start'
}
$process = Start-Process -FilePath $installer `
  -ArgumentList $installerArguments -Wait -PassThru
if ($process.ExitCode -ne 0) {
  throw "NSIS installer failed with exit code $($process.ExitCode)"
}

$registration = @(
  Get-ItemProperty `
    'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*' `
    -ErrorAction SilentlyContinue |
    Where-Object { $_.DisplayName -eq 'Codex Provider Switcher' }
)
if ($registration.Count -ne 1) {
  throw "expected one current-user uninstall registration; found $($registration.Count)"
}

$installLocation = Get-RegistrationInstallLocation $registration[0]
if ($ExpectedVersion -and $registration[0].DisplayVersion -ne $ExpectedVersion) {
  throw "installed version is $($registration[0].DisplayVersion), expected $ExpectedVersion"
}
$resolvedInstall = (Resolve-Path -LiteralPath $installLocation).Path
$resolvedLocalAppData = (Resolve-Path -LiteralPath $env:LOCALAPPDATA).Path
if (-not $resolvedInstall.StartsWith(
    "$resolvedLocalAppData\",
    [StringComparison]::OrdinalIgnoreCase
  )) {
  throw "installer wrote outside LOCALAPPDATA: $resolvedInstall"
}
$savedInstallLocation = Get-SavedInstallLocation
if (-not $savedInstallLocation) {
  throw 'installer did not record its current-user installation path'
}
$resolvedSavedInstall = (Resolve-Path -LiteralPath $savedInstallLocation).Path
if (-not $resolvedSavedInstall.Equals(
    $resolvedInstall,
    [StringComparison]::OrdinalIgnoreCase
  )) {
  throw "installer saved an unexpected installation path: $resolvedSavedInstall"
}

$app = Join-Path $resolvedInstall 'codex-provider-switcher.exe'
$uninstaller = Join-Path $resolvedInstall 'uninstall.exe'
foreach ($required in @($app, $uninstaller)) {
  if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
    throw "installed file is missing: $required"
  }
}
if (Get-Process -Name codex-provider-switcher -ErrorAction SilentlyContinue) {
  throw 'installer unexpectedly left Codex Provider Switcher running'
}
$afterState = @(Get-StateFingerprint)
if (($beforeState | ConvertTo-Json -Compress) -ne
    ($afterState | ConvertTo-Json -Compress)) {
  throw 'installation unexpectedly changed Codex or Provider Switcher user state'
}
$afterCredentialMetadata = Get-CredentialMetadata
if ($beforeCredentialMetadata -ne $afterCredentialMetadata) {
  throw 'installation unexpectedly changed Windows credential metadata'
}
$afterAutostart = Get-OptionalRegistryValue `
  $runKey 'Codex Provider Switcher'
$afterStartupApproved = Get-OptionalRegistryValue `
  $startupApprovedKey 'Codex Provider Switcher'
if ($beforeAutostart -ne $afterAutostart -or
    $beforeStartupApproved -ne $afterStartupApproved) {
  throw 'installation unexpectedly changed the Provider Switcher autostart entry'
}

$appSignature = Get-AuthenticodeSignature -FilePath $app
$appSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $app).Hash.ToLowerInvariant()
$versionInfo = (Get-Item -LiteralPath $app).VersionInfo
function Test-VersionEquals([string]$Actual, [string]$Expected) {
  try {
    $actualVersion = [Version]$Actual
    $expectedVersion = [Version]$Expected
    $actualParts = @(
      $actualVersion.Major,
      $actualVersion.Minor,
      [Math]::Max(0, $actualVersion.Build),
      [Math]::Max(0, $actualVersion.Revision)
    )
    $expectedParts = @(
      $expectedVersion.Major,
      $expectedVersion.Minor,
      [Math]::Max(0, $expectedVersion.Build),
      [Math]::Max(0, $expectedVersion.Revision)
    )
    return (($actualParts -join '.') -eq ($expectedParts -join '.'))
  }
  catch {
    return ($Actual -eq $Expected)
  }
}
if ($ExpectedVersion -and
    (-not (Test-VersionEquals $versionInfo.FileVersion $ExpectedVersion) -or
      -not (Test-VersionEquals $versionInfo.ProductVersion $ExpectedVersion))) {
  throw 'installed application version metadata does not match the expected version'
}
if ($ExpectedAppSha256 -and
    $appSha256 -ne $ExpectedAppSha256.ToLowerInvariant()) {
  throw 'installed application SHA-256 does not match the approved build'
}
[pscustomobject]@{
  account = $account
  installLocation = $resolvedInstall
  appPath = $app
  appSha256 = $appSha256
  displayVersion = $registration[0].DisplayVersion
  fileVersion = $versionInfo.FileVersion
  productVersion = $versionInfo.ProductVersion
  authenticode = $appSignature.Status.ToString()
  uninstallRegistered = $true
  credentialMetadataUnchanged = $true
  savedInstallLocation = $resolvedSavedInstall
  appRunning = $false
} | ConvertTo-Json -Compress
