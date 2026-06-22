@echo off
call "C:\Program Files (x86)\Microsoft Visual Studio\2019\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" x64 >nul
if errorlevel 1 exit /b 1
cd /d "D:\Disk Spy"
cargo %*

rem If this was a release build, also embed the UAC manifest so launching
rem diskspy.exe from a non-admin shell auto-prompts UAC.
if /I "%~1"=="build" goto :maybe_embed
if /I "%~1"=="b"     goto :maybe_embed
goto :eof

:maybe_embed
rem Only embed if the second arg is "--release" (cargo syntax).
if /I "%~2"=="--release" goto :embed
goto :eof

:embed
if not exist "target\release\diskspy.exe" goto :eof
echo Embedding UAC manifest...
"C:\Program Files (x86)\Windows Kits\10\bin\10.0.19041.0\x64\mt.exe" -manifest "assets\diskspy.exe.manifest" -outputresource:"target\release\diskspy.exe";#1
if errorlevel 1 echo (manifest embed step failed - non-fatal)
