# vcam (Rust) — real-time webcam virtual background

A Rust port of the Python `vcam.py` / `vcam_ui.py` in the parent folder. It replaces
your real background in real time and exposes the result as the **OBS Virtual Camera**,
which Google Meet, Zoom, Teams, Discord and OBS can select like a normal webcam.

100% local. No cloud, no account.

> **Status: builds and passes its self-test; not yet checked by eye.** It compiles with
> Rust 1.100 (`x86_64-pc-windows-gnu`), the unit tests pass, and the CLI captures, mattes
> on the GPU and feeds the OBS Virtual Camera. Picture quality and the control panel have
> not been checked by a person. See "Verification status" at the end.

## What is in here

| Path | Purpose |
|------|---------|
| `src/bin/vcam.rs` | `vcam.exe`: the control panel (port of `vcam_ui.py`), plus `--selftest`. |
| `src/bin/vcam-cli.rs` | `vcam-cli.exe`: the command line (port of `python vcam.py ...`). |
| `src/worker.rs` | `VCamWorker`: the capture -> matte -> composite -> virtual-cam thread. |
| `src/pipeline.rs` | `OutputPipeline`: compositing + send on their own thread. |
| `src/engine/` | The matting engine: `rvm.rs` (RobustVideoMatting), `ort_env.rs` (loads ONNX Runtime). |
| `src/compositor.rs`, `src/color.rs`, `src/geometry.rs`, `src/background.rs` | Compositor, colour presets, crop / fit maths, background loading. |
| `src/imgproc.rs` | The image operations the Python version got from OpenCV (resize, blur, guided filter, morphology, connected components). |
| `src/camera.rs` | Webcam capture through Media Foundation, and the `FrameGrabber` thread. |
| `src/virtualcam.rs` | OBS Virtual Camera output (shared-memory queue), install / uninstall, status. |
| `src/models.rs` | Model registry, lookup and download. |
| `src/ui/` | The egui control panel (`app.rs`), its look (`theme.rs`, `widgets.rs`), settings file, preview rendering. |
| `build.cmd` | Release build + bundle into `release\vcam\`. |
## Build

1. Install Rust from <https://rustup.rs>. Either toolchain works:
   - `stable-x86_64-pc-windows-msvc` (the default; needs the Visual Studio C++ Build Tools), or
   - `stable-x86_64-pc-windows-gnu` (no Visual Studio, but needs MinGW-w64 on PATH for
     `as`, `dlltool` and `gcc`, for example
     `winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e --scope user`).
2. Build:

```bat
cargo build --release
```

   or run `build.cmd`, which also bundles everything into `release\vcam\`.

Run the unit tests (no camera or model needed) with `cargo test`.

## Run

```bat
target\release\vcam.exe                 :: control panel
target\release\vcam-cli.exe --blur      :: command line
target\release\vcam-cli.exe --background ..\vb\2.png
target\release\vcam.exe --selftest      :: checks ONNX Runtime + RVM, writes vcam_selftest.txt
```

`vcam-cli --help` lists all flags. They follow the Python CLI without the MediaPipe ones
(`--engine`, `--feather`, `--edge-radius`, `--erode`, `--temporal`, `--mask-scale`).
`--threshold` is kept but now sets the RVM matte threshold (0 = off). They add
`--list-cameras` and `--download-models DIR`.

### What must be next to the exe

- **`onnxruntime.dll`, version 1.20 or newer.** It is looked up in this order: the
  `VCAM_ORT_DLL` environment variable, next to the exe, `onnxruntime\`, and finally a
  Python virtualenv in the folders above the exe
  (`venv\Lib\site-packages\onnxruntime\capi\`). That last rule means a `cargo run` inside
  this repository picks up the original project's `onnxruntime-gpu 1.20.2` with no setup.
- **For NVIDIA GPUs:** `onnxruntime_providers_cuda.dll` and
  `onnxruntime_providers_shared.dll` beside `onnxruntime.dll`, and the CUDA 12 / cuDNN 9
  DLLs in `nvidia\<package>\bin\` (the layout of the pip `nvidia-*-cu12` packages).
  Keep the versions the Python project pins: CUDA 12.6, cuDNN 9.5. Do not use CUDA 13.
- **Models** in `models\`. Missing ones are downloaded to `%LOCALAPPDATA%\vcam\models`.
  The folders above the exe are searched too, so the parent project's `models\` is used
  for the two RVM files.
- **OBS Studio** installed once, for the virtual-camera driver. The control panel's
  **Install** button registers it.

## Differences from the Python version

- **RobustVideoMatting only.** The two RVM models (`rvm_mobilenetv3_fp32`, the default,
  and `rvm_resnet50_fp32`) run in ONNX Runtime. The Python app's MediaPipe selfie models
  are not included; a saved setting naming one falls back to the default model.
- **Zoom 1.0 is actual size.** A crop keeps the size it has in the camera frame instead
  of being enlarged to fill the output; Zoom (0.5 - 2.0) scales from there.
- **Sharper matte handling.** The matte is blended as RVM made it (the Python app's
  extra edge feather is gone). The **Threshold** slider (Options card; `--threshold` in
  the CLI, 0 = off) turns uncertain, see-through parts of the person solid, for example
  light clothes against a light wall.
- **The matte is made at the size it is shown.** With Zoom above 1 (or an output smaller
  than the camera) the frame is shrunk before matting, and the Quality ratio is scaled up
  to match, so the coarse pass keeps its resolution. On a GTX 1050 Ti at Balanced:
  about 25 fps at zoom 1.0, 27.5 at 1.5, 30 at 2.0.
- **Rayon uses at most 4 worker threads** (`RAYON_NUM_THREADS` overrides it). The default
  of one per logical CPU spun idle workers and cost about 4 CPU cores.
- **Camera capture uses Media Foundation**, not DirectShow. Camera index order can
  differ on machines with several cameras; `vcam-cli --list-cameras` shows it. DirectShow-
  only virtual cameras (including the OBS one) are not offered as inputs.
- **No OpenCV.** Image operations are reimplemented in `src/imgproc.rs`. They follow
  OpenCV's conventions, but results are not guaranteed bit-identical.
- **The virtual camera is written directly** (the OBS shared-memory queue), with no
  `pyvirtualcam`.
- **The window is drawn with egui**, in the same layout as the Tk one, with its own
  styling (`src/ui/theme.rs`, `src/ui/widgets.rs`). The **Theme** dropdown in the footer
  switches between Dark, Light and Follow Windows; the choice is saved as `theme` in the
  settings file. Sliders reset on double-click like the Tk ones. Small additions: the
  camera is picked by name from a dropdown, and the colour card has a "Reset all" button.
- The CLI runs on `VCamWorker` instead of a second copy of the loop.
- Settings use the same file and keys (`%LOCALAPPDATA%\vcam\ui_settings.json`), so they
  carry over. Logs go to `%LOCALAPPDATA%\vcam\vcam.log`.
- Not ported: the historical patch scripts (`update*.cmd`, `zoom*_update.py`) and
  `vcam.spec`.

## Verification status

- **Builds** with Rust 1.100 on `x86_64-pc-windows-gnu`; `cargo test` passes.
- **`vcam.exe --selftest` passes** with the CUDA provider on ONNX Runtime 1.20.2.
- **`vcam-cli --blur --no-preview` ran for 20 seconds**: 1280x720 capture, RVM on the GPU,
  virtual camera live, about 19 fps. Nobody looked at the picture.
- Checked by reading the pinned crates' source: the `ort` 2.0.0-rc.15, `windows` 0.58,
  `eframe`/`egui` 0.31.1, `rfd` 0.15 and `image` 0.25 calls used here.
- Checked against the OBS source: the shared-memory queue layout and write sequence.
- Untested: the control panel, the picture itself (matte quality, colours, orientation),
  image / colour backgrounds, zoom, crop and
  rotation, and install / uninstall of the virtual camera.
