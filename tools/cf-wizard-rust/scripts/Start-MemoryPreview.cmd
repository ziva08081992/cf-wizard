@echo off
setlocal
set "CF_MEMORY_PS=%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe"
if exist "%SystemRoot%\Sysnative\WindowsPowerShell\v1.0\powershell.exe" set "CF_MEMORY_PS=%SystemRoot%\Sysnative\WindowsPowerShell\v1.0\powershell.exe"
"%CF_MEMORY_PS%" -NoProfile -ExecutionPolicy Bypass -File "%~dp0Start-MemoryPreview.ps1" %*
exit /b %ERRORLEVEL%
