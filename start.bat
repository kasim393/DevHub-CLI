@echo off
rem DevHub CLI launcher
rem -ExecutionPolicy Bypass avoids "not digitally signed" errors on fresh clones.
rem Falls back to Windows PowerShell 5.1 if PowerShell 7 (pwsh) isn't installed.

where pwsh >nul 2>nul
if %errorlevel%==0 (
    pwsh -NoExit -ExecutionPolicy Bypass -File "%~dp0dev-manager.ps1"
) else (
    powershell -NoExit -ExecutionPolicy Bypass -File "%~dp0dev-manager.ps1"
)
