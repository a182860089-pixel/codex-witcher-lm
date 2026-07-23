#requires -Version 5.1

[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$SourcePath,

  [string]$ToolRoot = 'D:\CodexProviderSwitcher\tooling',

  [string]$SourceArchiveSha256 = '',

  [switch]$RunPackageSmoke
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)

$source = (Resolve-Path -LiteralPath $SourcePath).Path
$cargoHome = Join-Path $ToolRoot 'cargo'
$rustupHome = Join-Path $ToolRoot 'rustup'
$npmPrefix = Join-Path $ToolRoot 'npm-global'
$pnpmStore = Join-Path $ToolRoot 'pnpm-store'
$tempRoot = Join-Path $ToolRoot 'temp'
$pnpm = Join-Path $npmPrefix 'pnpm.cmd'
$cargo = Join-Path $cargoHome 'bin\cargo.exe'

foreach ($required in @($pnpm, $cargo)) {
  if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
    throw "required build tool is missing: $required"
  }
}

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vsPath = & $vswhere -latest -products * -requires `
  Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
  Microsoft.VisualStudio.Component.Windows11SDK.26100 `
  -property installationPath
if (-not $vsPath) {
  throw 'the required MSVC and Windows SDK installation is unavailable'
}
$vsDevCmd = Join-Path $vsPath 'Common7\Tools\VsDevCmd.bat'
$environmentCommand = "`"$vsDevCmd`" -no_logo -arch=x64 -host_arch=x64 >nul && set"
foreach ($line in @(& cmd.exe /d /s /c $environmentCommand)) {
  if ($line -match '^([^=]+)=(.*)$') {
    Set-Item -Path "Env:$($matches[1])" -Value $matches[2]
  }
}
if ($LASTEXITCODE -ne 0) {
  throw 'could not initialize the MSVC environment'
}

$env:TEMP = $tempRoot
$env:TMP = $tempRoot
$env:CARGO_HOME = $cargoHome
$env:RUSTUP_HOME = $rustupHome
$env:PATH = "$npmPrefix;$cargoHome\bin;$env:PATH"
$env:CARGO_INCREMENTAL = '0'

Push-Location $source
try {
  & $pnpm install --frozen-lockfile --store-dir $pnpmStore
  if ($LASTEXITCODE -ne 0) { throw 'pnpm install failed' }

  & $pnpm check
  if ($LASTEXITCODE -ne 0) { throw 'TypeScript check failed' }
  & $pnpm test
  if ($LASTEXITCODE -ne 0) { throw 'runtime tests failed' }
  & $pnpm web:build
  if ($LASTEXITCODE -ne 0) { throw 'WebView production build failed' }

  & $cargo fmt --all -- --check
  if ($LASTEXITCODE -ne 0) { throw 'cargo fmt check failed' }
  & $cargo test `
    -p codex-provider-switcher-core `
    -p codex-provider-switcher-credentials `
    -p codex-provider-switcher-launcher
  if ($LASTEXITCODE -ne 0) { throw 'Rust tests failed' }

  & $pnpm tauri build
  if ($LASTEXITCODE -ne 0) { throw 'native Tauri package build failed' }

  $installers = @(
    Get-ChildItem -LiteralPath 'target\release\bundle\nsis' -Filter '*-setup.exe'
  )
  if ($installers.Count -ne 1) {
    throw "expected exactly one NSIS installer; found $($installers.Count)"
  }

  if ($RunPackageSmoke) {
    $env:RUNNER_TEMP =
      Join-Path $tempRoot "cps-smoke-$([guid]::NewGuid().ToString('N'))"
    New-Item -ItemType Directory -Path $env:RUNNER_TEMP | Out-Null
    & powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass `
      -File '.\ci\smoke-package-windows.ps1'
    if ($LASTEXITCODE -ne 0) { throw 'NSIS package smoke test failed' }
  }

  $installer = $installers[0]
  $signature = Get-AuthenticodeSignature -FilePath $installer.FullName
  [pscustomobject]@{
    sourcePath = $source
    sourceArchiveSha256 = $SourceArchiveSha256
    installerPath = $installer.FullName
    installerSha256 = (
      Get-FileHash -Algorithm SHA256 -LiteralPath $installer.FullName
    ).Hash.ToLowerInvariant()
    installerBytes = $installer.Length
    authenticode = $signature.Status.ToString()
    packageSmoke = [bool]$RunPackageSmoke
  } | ConvertTo-Json -Compress
}
finally {
  Pop-Location
}
