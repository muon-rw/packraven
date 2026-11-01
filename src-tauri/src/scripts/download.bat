rem A PowerShell 7 terminal passes on its module path, which stops Windows PowerShell loading its own commands.
set "PSModulePath="
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File packraven\download.ps1
if errorlevel 1 (
    pause
    exit /b 1
)
