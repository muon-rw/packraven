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
if exist "%GRAALVM_DIR%\bin\java.exe" goto graalvm
echo Downloading GraalVM %GRAALVM% into %GRAALVM_DIR%. To use your own Java instead, write its path in java.txt.
if exist "%GRAALVM_DIR%" rmdir /s /q "%GRAALVM_DIR%"
mkdir "%GRAALVM_DIR%"
"%SystemRoot%\System32\curl.exe" -fL -o "%GRAALVM_DIR%.zip" "{{URL_PREFIX}}windows-x64_bin.zip" || goto failed
"%SystemRoot%\System32\tar.exe" -xf "%GRAALVM_DIR%.zip" -C "%GRAALVM_DIR%" --strip-components=1 || goto failed
del "%GRAALVM_DIR%.zip"
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
if exist "%GRAALVM_DIR%" rmdir /s /q "%GRAALVM_DIR%"
if exist "%GRAALVM_DIR%.zip" del "%GRAALVM_DIR%.zip"
echo Couldn't download GraalVM %GRAALVM%. Install Java %GRAALVM% yourself, then write the path to its java.exe in java.txt.
pause
exit /b 1
