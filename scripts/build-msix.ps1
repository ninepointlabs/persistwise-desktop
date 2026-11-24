<#
  Builds dist\persistwise-desktop-<version>-x64.msix for the Microsoft Store.

  Run on Windows with the Windows 10/11 SDK installed (makeappx.exe), after:
      npm ci
      npx tauri build --no-bundle        # builds target\release\persistwise-desktop.exe

  The Store re-signs packages, so an unsigned .msix is what you upload.
  For local side-loading, pass -CertPath/-CertPassword to sign it with a
  test certificate whose subject matches -Publisher.

  Nothing here talks to Partner Center or the Store.
#>
param(
  [string]$IdentityName = "NinePointLabs.Persistwise",       # replace with Partner Center Package/Identity/Name
  [string]$Publisher = "CN=Nine Point Labs (placeholder)",  # replace with Partner Center Package/Identity/Publisher
  [string]$PublisherName = "Nine Point Labs",
  [string]$CertPath = "",
  [string]$CertPassword = ""
)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

$conf = Get-Content "src-tauri\tauri.conf.json" -Raw | ConvertFrom-Json
$version = $conf.version
$msixVersion = "$version.0"
$exe = "target\release\persistwise-desktop.exe"
if (-not (Test-Path $exe)) { throw "Missing $exe. Run: npx tauri build --no-bundle" }

$stage = "build\msix\stage"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path "$stage\Assets", "dist" | Out-Null
Copy-Item $exe $stage
Copy-Item "packaging\windows\msix\Assets\*.png" "$stage\Assets"
(Get-Content "packaging\windows\msix\AppxManifest.xml" -Raw) `
  -replace "@IDENTITY_NAME@", $IdentityName `
  -replace "@PUBLISHER@", $Publisher `
  -replace "@PUBLISHER_NAME@", $PublisherName `
  -replace "@VERSION@", $msixVersion | Set-Content "$stage\AppxManifest.xml" -Encoding UTF8

$sdk = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\makeappx.exe" | Sort-Object FullName -Descending | Select-Object -First 1
if (-not $sdk) { throw "makeappx.exe not found; install the Windows 10/11 SDK" }
$out = "dist\persistwise-desktop-$version-x64.msix"
& $sdk.FullName pack /o /d $stage /p $out
if ($LASTEXITCODE -ne 0) { throw "makeappx failed" }

if ($CertPath) {
  $signtool = Join-Path $sdk.DirectoryName "signtool.exe"
  & $signtool sign /fd SHA256 /f $CertPath /p $CertPassword $out
  if ($LASTEXITCODE -ne 0) { throw "signtool failed" }
}
Write-Host "Built $out"
