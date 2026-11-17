# Builds the Windows release files, so users can install without a Rust
# toolchain:
#   tools\package.ps1 v0.2.0-beta
#     -> dist\bootpick-v0.2.0-beta-x86_64-windows-setup.exe   the installer: one file, opens a window
#     -> dist\bootpick-v0.2.0-beta-x86_64-windows.zip         the same files with install.cmd and make-usb.ps1
param([Parameter(Mandatory = $true)][string]$Version)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
& "$PSScriptRoot\build.ps1"

$name = "bootpick-$Version-x86_64-windows"
$stage = Join-Path $env:TEMP "bootpick-package-$PID\$name"
$b = "$stage\esp\EFI\bootpick"
New-Item -ItemType Directory -Force $b, "$stage\tools", "$stage\assets" | Out-Null
foreach ($d in 'icons', 'flags', 'fonts', 'backgrounds') {
    Copy-Item "esp\EFI\bootpick\$d" $b -Recurse
}
Copy-Item 'uefi\target\x86_64-unknown-uefi\release\bootpick.efi' $stage
Copy-Item 'target\release\bootpick-config.exe' $stage
Copy-Item 'assets\drivers' "$stage\assets" -Recurse
Copy-Item 'install.ps1', 'install.cmd', 'bootpick.conf.example', 'README.md', 'LICENSE' $stage
Copy-Item 'tools\make-usb.ps1' "$stage\tools"

New-Item -ItemType Directory -Force 'dist' | Out-Null
$zip = "dist\$name.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path $stage -DestinationPath $zip
Remove-Item (Split-Path -Parent $stage) -Recurse -Force
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
Set-Content "$zip.sha256" "$hash  $name.zip" -Encoding ascii
Write-Host $zip

# The single-file installer: the settings app with the boot menu files packed
# at its end.
$setup = "dist\$name-setup.exe"
$p = Start-Process -FilePath "$root\target\release\bootpick-config.exe" -ArgumentList '--make-setup', "`"$root\$setup`"", '--source', "`"$root`"" -NoNewWindow -Wait -PassThru
if ($p.ExitCode -ne 0) { throw 'making the installer failed' }
$hash = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLower()
Set-Content "$setup.sha256" "$hash  $name-setup.exe" -Encoding ascii
Write-Host $setup
