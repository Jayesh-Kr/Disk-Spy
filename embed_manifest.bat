@echo off
rem Embed the UAC elevation manifest into the Windows release binary.
rem Run after `cargo build --release` so launching diskspy.exe from a
rem non-admin shell auto-prompts UAC instead of exiting with the
rem "needs admin" error.

setlocal
call "C:\Program Files (x86)\Microsoft Visual Studio\2019\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" x64 >nul
if errorlevel 1 exit /b 1

set MANIFEST=assets\diskspy.exe.manifest
set BIN=target\release\diskspy.exe

if not exist "%BIN%" (
    echo %BIN% not found. Run "build.bat build --release" first.
    exit /b 1
)

echo Embedding %MANIFEST% into %BIN%...
"C:\Program Files (x86)\Windows Kits\10\bin\10.0.19041.0\x64\mt.exe" -manifest "%MANIFEST%" -outputresource:"%BIN%";#1
if errorlevel 1 (
    echo mt.exe failed.
    exit /b 1
)
echo Done.
