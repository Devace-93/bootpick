@echo off
rem Runs install.ps1 regardless of the PowerShell execution policy.
rem Options: -Keep -Reset -NoBootEntry -NoApp -Uninstall (see install.ps1).
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
