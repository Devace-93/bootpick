# Copies bootpick onto an already FAT32-formatted USB stick so it can be tried
# from the firmware's one-time boot menu before touching the internal ESP.
#   tools\make-usb.ps1 E:
# Works from the release package and from a built source tree.
param([Parameter(Mandatory = $true)][string]$Drive)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$usb = $Drive.TrimEnd('\')
if ($usb -notmatch '^[A-Za-z]:$') { throw 'give the drive letter of the USB stick, for example E:' }
$vol = Get-Volume -DriveLetter $usb[0]
if ($vol.FileSystem -notlike 'FAT*') { throw "$usb is $($vol.FileSystem); the stick must be formatted as FAT32" }
if ($vol.DriveType -ne 'Removable') { throw "$usb is not a removable drive" }

$efi = @("$root\bootpick.efi", "$root\uefi\target\x86_64-unknown-uefi\release\bootpick.efi") | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $efi) { throw 'bootpick.efi not found: build first with tools\build.ps1' }
$assets = "$root\esp\EFI\bootpick"

$dest = "$usb\EFI\bootpick"
New-Item -ItemType Directory -Force "$usb\EFI\BOOT", "$dest\icons", "$dest\backgrounds", "$dest\drivers", "$dest\flags", "$dest\fonts" | Out-Null
Copy-Item $efi "$usb\EFI\BOOT\BOOTX64.EFI" -Force
Copy-Item "$assets\icons\*.bmp" "$dest\icons" -Force
Copy-Item "$assets\flags\*.bmp" "$dest\flags" -Force
Copy-Item "$assets\fonts\*.ttf" "$dest\fonts" -Force
if (Test-Path "$assets\backgrounds") { Copy-Item "$assets\backgrounds\*.bmp" "$dest\backgrounds" -Force }
if (Test-Path "$root\assets\drivers") { Copy-Item "$root\assets\drivers\*.efi" "$dest\drivers" -Force }
if (-not (Test-Path "$dest\bootpick.conf")) { Copy-Item "$root\bootpick.conf.example" "$dest\bootpick.conf" }
Write-VolumeCache -DriveLetter $usb[0]
Write-Host 'done: boot the stick from the firmware boot menu (settings are saved on the stick)'
