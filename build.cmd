@echo off
setlocal
cd /d "%~dp0"

rem ======================================================================
rem  Builds release\vcam\ (a FOLDER you can ship):
rem      vcam.exe        the control panel
rem      vcam-cli.exe    the command line
rem      models\         all five matting models (no download at run time)
rem      assets\         icons
rem      onnxruntime*.dll and nvidia\*\bin\*.dll when a source is found
rem
rem  Needs the Rust toolchain (https://rustup.rs). ONNX Runtime and the
rem  CUDA 12 / cuDNN 9 DLLs are copied from a Python virtualenv when one
rem  is active (%VIRTUAL_ENV%) or sits in ..\venv; otherwise copy them in
rem  by hand (see README.md).
rem ======================================================================

set "OUT=%~dp0release\vcam"

where cargo >nul 2>&1
if errorlevel 1 (
  if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
)
where cargo >nul 2>&1
if errorlevel 1 (
  echo ERROR: cargo was not found. Install Rust from https://rustup.rs and run this again.
  goto :fail
)

echo Building vcam (release) ...
rem Try with the exe icon first (needs rc.exe or windres); fall back to a plain build.
cargo build --release --features exe-icon
if errorlevel 1 (
  echo.
  echo Retrying without the embedded exe icon ...
  cargo build --release
  if errorlevel 1 goto :fail
)

if not exist "%OUT%" mkdir "%OUT%"
copy /y "target\release\vcam.exe" "%OUT%\" >nul || goto :fail
copy /y "target\release\vcam-cli.exe" "%OUT%\" >nul || goto :fail
if exist "assets" xcopy /e /i /y /q "assets" "%OUT%\assets" >nul

rem ---- find (or download) ALL five model files ----
"%OUT%\vcam-cli.exe" --download-models "%OUT%\models"
if errorlevel 1 (
  echo ERROR: could not find or download every model file.
  goto :fail
)

rem ---- ONNX Runtime + CUDA runtime DLLs from a Python virtualenv, when there is one ----
set "SP="
if defined VIRTUAL_ENV if exist "%VIRTUAL_ENV%\Lib\site-packages\onnxruntime\capi\onnxruntime.dll" set "SP=%VIRTUAL_ENV%\Lib\site-packages"
if not defined SP if exist "%~dp0..\venv\Lib\site-packages\onnxruntime\capi\onnxruntime.dll" set "SP=%~dp0..\venv\Lib\site-packages"
if defined SP (
  echo Copying ONNX Runtime from %SP%
  copy /y "%SP%\onnxruntime\capi\onnxruntime*.dll" "%OUT%\" >nul
  rem nvjitlink: cufft64_11.dll needs nvJitLink_120_0.dll
  for %%P in (cuda_runtime cuda_nvrtc cublas cufft curand cudnn nvjitlink) do (
    if exist "%SP%\nvidia\%%P\bin" xcopy /i /y /q "%SP%\nvidia\%%P\bin\*.dll" "%OUT%\nvidia\%%P\bin\" >nul
  )
  rem onnxruntime*.dll need the Visual C++ 2015-2022 runtime, which the pip package does
  rem not include. Ship it next to the exe (app-local) so a fresh PC needs no installer.
  for %%D in (msvcp140.dll msvcp140_1.dll msvcp140_2.dll vcruntime140.dll vcruntime140_1.dll concrt140.dll) do (
    if exist "%SystemRoot%\System32\%%D" copy /y "%SystemRoot%\System32\%%D" "%OUT%\" >nul
  )
) else (
  echo.
  echo NOTE: onnxruntime.dll was not copied. Put ONNX Runtime 1.20 or newer next to
  echo       %OUT%\vcam.exe before running it ^(see README.md^).
)

echo.
echo Done: %OUT%\vcam.exe
echo Ship the whole "%OUT%" folder.
set "RC=0"
goto :end

:fail
echo.
echo Build failed.
set "RC=1"

:end
echo %cmdcmdline% | find /i "%~0" >nul && pause
exit /b %RC%
