@echo off
setlocal
cd /d "%~dp0"

rem To run the server on your own Java, write the path to its java.exe in java.txt.
rem Add JVM arguments, like -Xmx for memory, to user_jvm_args.txt.
set "GRAALVM={{GRAALVM}}"
set "JAVA="
if exist java.txt set /p JAVA=<java.txt
if defined JAVA set "JAVA=%JAVA:"=%"
if defined JAVA goto install
if "%GRAALVM%"=="" (
    set "JAVA=java"
    goto install
)

set "GRAALVM_DIR=java\graalvm-%GRAALVM%"
set "STAGING=%GRAALVM_DIR%.new"
set "URL={{URL_PREFIX}}windows-x64_bin.zip"
set "CURL=%SystemRoot%\System32\curl.exe"

rem Oracle replaces its latest GraalVM build in place, so we compare the checksum Oracle publishes
rem with the one we saved when we installed. Offline, we keep using what's installed.
set "LATEST="
set "INSTALLED="
"%CURL%" -fsL --max-time 20 --create-dirs -o "%GRAALVM_DIR%.sha256" "%URL%.sha256" 2>nul
if exist "%GRAALVM_DIR%.sha256" (
    for /f "usebackq tokens=1" %%h in ("%GRAALVM_DIR%.sha256") do set "LATEST=%%h"
    del "%GRAALVM_DIR%.sha256"
)
if exist "%GRAALVM_DIR%\download.sha256" set /p INSTALLED=<"%GRAALVM_DIR%\download.sha256"

if not exist "%GRAALVM_DIR%\bin\java.exe" goto download
if not defined LATEST goto graalvm
if "%LATEST%"=="%INSTALLED%" goto graalvm
echo Updating GraalVM %GRAALVM% in %GRAALVM_DIR%.
goto fetch
:download
echo Downloading GraalVM %GRAALVM% into %GRAALVM_DIR%. To use your own Java instead, write its path in java.txt.
:fetch
if exist "%STAGING%" rmdir /s /q "%STAGING%"
mkdir "%STAGING%"
"%CURL%" -fL -o "%STAGING%.zip" "%URL%" || goto failed
"%SystemRoot%\System32\tar.exe" -xf "%STAGING%.zip" -C "%STAGING%" --strip-components=1 || goto failed
del "%STAGING%.zip"
if not exist "%STAGING%\bin\java.exe" goto failed
if defined LATEST >"%STAGING%\download.sha256" echo %LATEST%
if exist "%GRAALVM_DIR%" rmdir /s /q "%GRAALVM_DIR%"
move "%STAGING%" "%GRAALVM_DIR%" >nul || goto failed
:graalvm
set "JAVA=%GRAALVM_DIR%\bin\java.exe"

:install
{{DOWNLOAD}}
{{INSTALL}}
:run
echo Starting the server on %JAVA%
"%JAVA%" @user_jvm_args.txt {{LAUNCH}} nogui{{ARGS}} %*
pause
exit /b

:failed
if exist "%STAGING%" rmdir /s /q "%STAGING%"
if exist "%STAGING%.zip" del "%STAGING%.zip"
if exist "%GRAALVM_DIR%\bin\java.exe" (
    echo Couldn't update GraalVM %GRAALVM%, so the server will start on the version it already has.
    goto graalvm
)
echo Couldn't download GraalVM %GRAALVM%. Install Java %GRAALVM% yourself, then write the path to its java.exe in java.txt.
pause
exit /b 1
