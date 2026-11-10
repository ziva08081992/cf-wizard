@echo off
setlocal
set "wizardPython=python.exe"
if exist "%~dp0runtime\python.exe" set "wizardPython=%~dp0runtime\python.exe"
pushd "%~dp0" || exit /b 1
"%wizardPython%" -B -X utf8 -m cf_wizard %*
set "wizardExit=%ERRORLEVEL%"
popd
if not "%wizardExit%"=="0" pause
exit /b %wizardExit%
