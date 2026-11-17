# Installs bootpick from Windows: the boot menu onto the EFI System Partition
# (made the first firmware boot entry) and the "bootpick settings" app, then
# opens the app. The Windows boot manager, GRUB and systemd-boot are left
# untouched, so you can always pick them from the firmware boot menu.
#
# Easiest: double-click install.cmd. From a terminal:
#
#   .\install.cmd                  # asks for administrator rights
#   .\install.cmd -Keep            # already installed: keep its settings (default)
#   .\install.cmd -Reset           # already installed: factory settings
#   .\install.cmd -NoBootEntry     # copy files only
#   .\install.cmd -NoApp           # boot menu only, no settings app
#   .\install.cmd -NoOpen          # do not open the settings app afterwards
#   .\install.cmd -Uninstall       # remove everything
#
# When bootpick is already installed on any EFI partition (for example from a
# Linux on this computer), that copy is updated in place and you are asked
# whether to keep its settings (backgrounds, logos, choices) or reset them.
#
# From a source tree, build first: tools\build.ps1
param(
    [switch]$Keep,
    [switch]$Reset,
    [switch]$NoBootEntry,
    [switch]$NoApp,
    [switch]$NoOpen,
    [switch]$Uninstall,
    # Set when this script restarted itself elevated in its own window.
    [switch]$Elevated
)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot

$exe = @("$root\bootpick-config.exe", "$root\target\release\bootpick-config.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $exe) {
    Write-Host 'bootpick-config.exe not found: build first with tools\build.ps1' -ForegroundColor Red
    exit 1
}

$flags = @()
if ($Keep) { $flags += '-Keep' }
if ($Reset) { $flags += '-Reset' }
if ($NoBootEntry) { $flags += '-NoBootEntry' }
if ($NoApp) { $flags += '-NoApp' }
if ($NoOpen) { $flags += '-NoOpen' }
if ($Uninstall) { $flags += '-Uninstall' }

$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    # Start again elevated (one UAC prompt), in a window that stays open.
    $psArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Elevated') + $flags
    try {
        Start-Process -FilePath 'powershell.exe' -Verb RunAs -ArgumentList $psArgs | Out-Null
    } catch {
        Write-Host 'administrator rights were not granted; nothing was changed' -ForegroundColor Red
        exit 1
    }
    exit 0
}

$code = 1
try {
    if (-not $Uninstall) {
        # With BitLocker, a new first boot entry changes what the TPM measures:
        # Windows then asks for the recovery key at the next start.
        $protected = $false
        try { $protected = (Get-BitLockerVolume -MountPoint $env:SystemDrive -ErrorAction Stop).ProtectionStatus -eq 'On' } catch { }
        if ($protected) {
            Write-Host "BitLocker protects $env:SystemDrive. After installing, Windows will ask for the" -ForegroundColor Yellow
            Write-Host 'BitLocker recovery key when it starts through bootpick. Have the key at hand,' -ForegroundColor Yellow
            Write-Host 'or suspend BitLocker first (Suspend-BitLocker).' -ForegroundColor Yellow
            if ((Read-Host 'Continue? [y/N]') -notmatch '^[yY]') { exit 1 }
        }
    }
    $exeArgs = @('--install', '--source', "`"$root`"")
    if ($Keep) { $exeArgs += '--keep' }
    if ($Reset) { $exeArgs += '--reset' }
    if ($NoBootEntry) { $exeArgs += '--no-boot-entry' }
    if ($NoApp) { $exeArgs += '--no-app' }
    if ($NoOpen) { $exeArgs += '--no-open' }
    if ($Uninstall) { $exeArgs += '--uninstall' }
    # The program is a GUI application: wait for it, sharing this console.
    $p = Start-Process -FilePath $exe -ArgumentList $exeArgs -NoNewWindow -Wait -PassThru
    $code = $p.ExitCode
} finally {
    if ($Elevated) { Read-Host 'Press Enter to close' | Out-Null }
}
exit $code
