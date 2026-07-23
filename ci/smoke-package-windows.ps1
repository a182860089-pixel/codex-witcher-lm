$ErrorActionPreference = 'Stop'

$installers = @(Get-ChildItem -LiteralPath 'target\release\bundle\nsis' -Filter '*-setup.exe')
if ($installers.Count -ne 1) {
  throw "Expected exactly one NSIS installer, found $($installers.Count)."
}

$installRoot = Join-Path $env:RUNNER_TEMP 'codex-provider-switcher-install'
if (Test-Path -LiteralPath $installRoot) {
  throw "Clean install target already exists: $installRoot"
}

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

$uninstaller = Start-Process -FilePath $uninstallerPath `
  -ArgumentList '/S' -PassThru -Wait
if ($uninstaller.ExitCode -ne 0) {
  throw "NSIS uninstaller exited with $($uninstaller.ExitCode)."
}
if (Test-Path -LiteralPath $app) {
  throw 'NSIS uninstall left the application executable behind.'
}
