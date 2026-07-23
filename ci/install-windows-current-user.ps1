#requires -Version 5.1

[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$InstallerPath,

  [Parameter(Mandatory = $true)]
  [ValidatePattern('^[0-9a-fA-F]{64}$')]
  [string]$InstallerSha256,

  [string]$ExpectedUser = 'Admin'
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
if (Test-Path -LiteralPath $desiredInstall) {
  throw "refusing to overwrite an existing installation path: $desiredInstall"
}

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
if ($existing.Count -ne 0) {
  throw 'Codex Provider Switcher is already registered for this user'
}

$process = Start-Process -FilePath $installer `
  -ArgumentList @('/S', "/D=$desiredInstall") -Wait -PassThru
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

$installLocation = $registration[0].InstallLocation.Trim([char]34)
if (-not $installLocation) {
  $uninstallString = $registration[0].UninstallString
  if ($uninstallString -match '^"([^"]+)"') {
    $uninstallPath = $matches[1]
  }
  else {
    $uninstallPath = ($uninstallString -split '\s+')[0]
  }
  $installLocation = Split-Path -Parent $uninstallPath
}
$resolvedInstall = (Resolve-Path -LiteralPath $installLocation).Path
$resolvedLocalAppData = (Resolve-Path -LiteralPath $env:LOCALAPPDATA).Path
if (-not $resolvedInstall.StartsWith(
    "$resolvedLocalAppData\",
    [StringComparison]::OrdinalIgnoreCase
  )) {
  throw "installer wrote outside LOCALAPPDATA: $resolvedInstall"
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

$appSignature = Get-AuthenticodeSignature -FilePath $app
[pscustomobject]@{
  account = $account
  installLocation = $resolvedInstall
  appPath = $app
  appSha256 = (
    Get-FileHash -Algorithm SHA256 -LiteralPath $app
  ).Hash.ToLowerInvariant()
  authenticode = $appSignature.Status.ToString()
  uninstallRegistered = $true
  appRunning = $false
} | ConvertTo-Json -Compress
