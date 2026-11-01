"%JAVA%" -jar packraven-downloader.jar
if errorlevel 1 (
    pause
    exit /b 1
)
