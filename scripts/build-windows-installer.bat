@echo off
setlocal EnableExtensions EnableDelayedExpansion

rem Builds the Foxy Windows installer using the same layout as the GitHub workflow.
rem Requirements:
rem   - Rust toolchain with x86_64-pc-windows-msvc target
rem   - Inno Setup compiler (iscc.exe) available in PATH

pushd "%~dp0\.." || exit /b 1

where cargo >nul 2>nul
if errorlevel 1 (
    echo Error: cargo was not found in PATH.
    popd
    exit /b 1
)

where iscc >nul 2>nul
if errorlevel 1 (
    echo Error: Inno Setup compiler ^(iscc^) was not found in PATH.
    echo Install Inno Setup and ensure iscc.exe is available in PATH.
    popd
    exit /b 1
)

for /f "tokens=2 delims== " %%A in ('findstr /b /c:"version" Cargo.toml') do (
    set "APP_VERSION=%%~A"
    goto :version_found
)

:version_found
if not defined APP_VERSION (
    echo Error: Could not read package version from Cargo.toml.
    popd
    exit /b 1
)

if not exist dist mkdir dist

echo [1/2] Building Windows release...
cargo build --release --target x86_64-pc-windows-msvc
if errorlevel 1 (
    echo Error: Windows release build failed.
    popd
    exit /b 1
)

echo [2/2] Building Windows installer...
iscc /DAppVersion="%APP_VERSION%" /DSourceDir="..\..\target\x86_64-pc-windows-msvc\release" installer\windows\foxy-setup.iss
if errorlevel 1 (
    echo Error: Windows installer build failed.
    popd
    exit /b 1
)

echo.
echo Built installer: dist\Foxy-%APP_VERSION%-setup.exe

popd
endlocal
