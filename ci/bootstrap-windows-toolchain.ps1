#requires -Version 5.1

[CmdletBinding()]
param(
  [string]$ToolRoot = 'D:\CodexProviderSwitcher\tooling'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)

$cargoHome = Join-Path $ToolRoot 'cargo'
$rustupHome = Join-Path $ToolRoot 'rustup'
$npmPrefix = Join-Path $ToolRoot 'npm-global'
$npmCache = Join-Path $ToolRoot 'npm-cache'
$pnpmStore = Join-Path $ToolRoot 'pnpm-store'
$tempRoot = Join-Path $ToolRoot 'temp'

New-Item -ItemType Directory -Force -Path @(
  $cargoHome,
  $rustupHome,
  $npmPrefix,
  $npmCache,
  $pnpmStore,
  $tempRoot
) | Out-Null

$env:TEMP = $tempRoot
$env:TMP = $tempRoot
$env:CARGO_HOME = $cargoHome
$env:RUSTUP_HOME = $rustupHome
[Environment]::SetEnvironmentVariable('CARGO_HOME', $cargoHome, 'User')
[Environment]::SetEnvironmentVariable('RUSTUP_HOME', $rustupHome, 'User')

$nodeVersion = (& node.exe --version).Trim()
$nodeMajor = [int](($nodeVersion -replace '^v', '').Split('.')[0])
if ($nodeMajor -lt 22) {
  throw "Node.js 22 or newer is required; found $nodeVersion"
}

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
  throw 'vswhere is unavailable; install the required Visual Studio Build Tools first'
}
$vsPath = & $vswhere -latest -products * -requires `
  Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
  Microsoft.VisualStudio.Component.Windows11SDK.26100 `
  -property installationPath
if (-not $vsPath) {
  throw 'the required MSVC and Windows 11 SDK components were not detected'
}

$rustupInit = Join-Path $ToolRoot 'rustup-init.exe'
$rustupUrl =
  'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe'
$rustupChecksumUrl = "$rustupUrl.sha256"
$rustupChecksumFile = Join-Path $tempRoot 'rustup-init.exe.sha256'
if (-not (Test-Path -LiteralPath $rustupInit -PathType Leaf)) {
  Invoke-WebRequest -UseBasicParsing $rustupUrl -OutFile $rustupInit
}
Invoke-WebRequest -UseBasicParsing $rustupChecksumUrl -OutFile $rustupChecksumFile
$checksumText = Get-Content -LiteralPath $rustupChecksumFile -Raw
$checksumMatch = [regex]::Match($checksumText, '(?i)[0-9a-f]{64}')
if (-not $checksumMatch.Success) {
  throw 'the official rustup SHA-256 manifest is unavailable'
}
$expectedRustupSha256 = $checksumMatch.Value.ToLowerInvariant()
$actualRustupSha256 =
  (Get-FileHash -Algorithm SHA256 -LiteralPath $rustupInit).Hash.ToLowerInvariant()
if ($actualRustupSha256 -ne $expectedRustupSha256) {
  throw 'rustup-init failed its official SHA-256 verification'
}

$rustup = Join-Path $cargoHome 'bin\rustup.exe'
if (-not (Test-Path -LiteralPath $rustup -PathType Leaf)) {
  $rustupProcess = Start-Process -FilePath $rustupInit -ArgumentList @(
    '-y',
    '--profile',
    'minimal',
    '--default-host',
    'x86_64-pc-windows-msvc',
    '--default-toolchain',
    '1.97.1',
    '--no-modify-path'
  ) -Wait -PassThru
  if ($rustupProcess.ExitCode -ne 0) {
    throw "rustup-init failed with exit code $($rustupProcess.ExitCode)"
  }
}

& $rustup component add rustfmt --toolchain 1.97.1-x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) {
  throw 'rustfmt installation failed'
}
$rustc = Join-Path $cargoHome 'bin\rustc.exe'
$rustInfo = (& $rustc -vV) -join "`n"
if ($rustInfo -notmatch 'host: x86_64-pc-windows-msvc') {
  throw 'the installed Rust toolchain is not the required Windows MSVC host'
}

& npm.cmd install --global pnpm@10.32.0 --prefix $npmPrefix --cache $npmCache `
  --no-audit --no-fund
if ($LASTEXITCODE -ne 0) {
  throw 'pnpm installation failed'
}
$pnpm = Join-Path $npmPrefix 'pnpm.cmd'
$pnpmVersion = (& $pnpm --version).Trim()
if ($pnpmVersion -ne '10.32.0') {
  throw "unexpected pnpm version $pnpmVersion"
}

$vsDevCmd = Join-Path $vsPath 'Common7\Tools\VsDevCmd.bat'
$devCommand =
  "`"$vsDevCmd`" -no_logo -arch=x64 -host_arch=x64 && where cl && where link && where rc"
$msvcTools = @(& cmd.exe /d /s /c $devCommand)
if ($LASTEXITCODE -ne 0) {
  throw 'MSVC command environment validation failed'
}

[pscustomobject]@{
  node = $nodeVersion
  pnpm = $pnpmVersion
  rustupSha256 = $actualRustupSha256
  rustToolchain = (
    ($rustInfo -split "`n" | Where-Object { $_ -match '^(release|host):' }) -join '; '
  )
  visualStudio = $vsPath
  msvcTools = $msvcTools
  pnpmStore = $pnpmStore
} | ConvertTo-Json -Depth 3 -Compress
