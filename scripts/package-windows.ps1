# Packages the release build for Windows into target\dist:
# kubach-<version>-windows-<arch>.zip (portable) and .msi.
#
#   cargo build --release -p kubach-app
#   scripts\package-windows.ps1 -Arch x64|arm64
#
# Needs the WiX v5 tool in PATH: dotnet tool install --global wix --version 5.0.2
param(
    [Parameter(Mandatory = $true)][ValidateSet('x64', 'arm64')][string]$Arch
)
$ErrorActionPreference = 'Stop'

$root = Resolve-Path (Join-Path $PSScriptRoot '..')
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.*)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$exe = Join-Path $target 'release\kubach.exe'
if (-not (Test-Path $exe)) { throw "no release build at $exe" }
$dist = Join-Path $target 'dist'
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$name = "kubach-$version-windows-$Arch"

$stage = Join-Path ([IO.Path]::GetTempPath()) $name
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $stage | Out-Null
Copy-Item $exe $stage
Copy-Item (Join-Path $root 'LICENSE') (Join-Path $stage 'LICENSE.txt')
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath (Join-Path $dist "$name.zip") -Force

# YEAR.MONTH.INC -> (YEAR - 2000).MONTH.INC: MSI majors stop at 255.
$parts = $version.Split('.')
$msiVersion = '{0}.{1}.{2}' -f ([int]$parts[0] - 2000), $parts[1], $parts[2]
wix build (Join-Path $root 'packaging\windows\kubach.wxs') -arch $Arch `
    -d "MsiVersion=$msiVersion" `
    -d "ExeFile=$exe" `
    -d "LicenseFile=$(Join-Path $stage 'LICENSE.txt')" `
    -d "IconFile=$(Join-Path $root 'crates\kubach-app\assets\icon\kubach.ico')" `
    -o (Join-Path $dist "$name.msi")
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }

Remove-Item -Recurse -Force $stage
Get-ChildItem $dist
