@echo off
rem Puts Explorer back as your shell (undoes Rust UI shell mode) and starts it now.
"%USERPROFILE%\shellbits\shellbits.exe" --restore-explorer
taskkill /f /im shellbits.exe >nul 2>&1
start "" explorer.exe
timeout /t 4 >nul
schtasks /run /tn shellbits >nul
