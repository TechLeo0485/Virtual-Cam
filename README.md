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
| `build.cmd` | Local release build + bundle into `release\vcam\` (needs Rust). |
| `.github/workflows/build.yml` | Cloud build on GitHub Actions (no Rust needed locally). |
| `.github/workflows/release.yml` | Builds the installers and publishes a GitHub Release when a `vX.Y.Z` tag is pushed. |
| `installer/vcam.iss` | Inno Setup script: the full setup and the small update package. |
| `ci/` | Pinned ONNX Runtime / CUDA wheels, runtime revision, and the script that extracts their DLLs. |
| `src/update.rs` | Update check and in-app update against the GitHub Releases. |

## Build

### In the cloud (no Rust on your PC)

`.github/workflows/build.yml` compiles the exes on GitHub's Windows servers.

1. Push this repository to GitHub. Every push to `main` starts a build; you can also
   start one from the **Actions** tab -> **build** -> **Run workflow**.
2. When the run is green, open it and download **vcam-windows** under *Artifacts*.
3. Unzip it and copy `vcam.exe` and `vcam-cli.exe` into `release\vcam\`, replacing the
   old ones. The models and the ONNX Runtime / CUDA DLLs already there are reused.

### Releases, installers and updates

The version lives in `Cargo.toml` (`version = "0.9.0"`). To publish a release:

1. Set the new version in `Cargo.toml` and commit it.
2. Tag and push the tag: `git tag v0.9.1` then `git push origin v0.9.1`.
3. `.github/workflows/release.yml` checks that the tag matches `Cargo.toml`, builds, runs
   the tests, builds two installers with Inno Setup (`installer/vcam.iss`) and publishes
   them on the repository's Releases page:
   - **vcam-setup-0.9.1.exe**: everything (program, both models, ONNX Runtime, the
     CUDA 12.6 / cuDNN 9.5 DLLs, the Visual C++ runtime). This is what people download
     for a first install; nothing is downloaded when vcam runs. About 1 GB.
   - **vcam-update-0.9.1-rt1.exe**: the program files only (about 10 MB), for
     installations already on runtime revision 1.

To try the installers before tagging: Actions tab -> **release** -> **Run workflow**. The
run builds both installers and attaches them to itself for 3 days, publishing nothing.

**Installing.** The setup installs for the current user into
`%LOCALAPPDATA%\Programs\vcam` with no admin prompt (its first page offers an all-users
install instead), adds a Start-menu shortcut, and optionally a desktop shortcut and
"Start vcam with Windows" (in the tray, see above). OBS Studio is still needed once for
the "OBS Virtual Camera" driver; the setup says so when it is missing. Uninstall from
Windows Settings -> Apps.

**Updating.** The control panel checks GitHub for a newer release at start and every 12
hours; the footer shows `vcam 0.9.0 · up to date` or **Install update v0.9.1 (10 MB)**.
That button downloads the right installer (the small update package when the runtime is
unchanged, else the full setup), checks its size and SHA-256, and runs it silently: it
closes vcam, installs, and starts vcam again. A copy not installed by the setup (no
`runtime-rev.txt` next to the exe) gets a button that opens the release page instead.
`vcam-cli --check-update` checks from the command line.

The repository to check is baked in at build time from `GITHUB_REPOSITORY`, which GitHub
Actions sets (set `VCAM_UPDATE_REPO=owner/name` for a local build). A build without it
never checks. GitHub answers only for a public repository; with a private one the check
reports "failed".

**Runtime revision.** `ci/runtime-requirements.txt` pins the ONNX Runtime and CUDA
wheels the DLLs come from; `ci/runtime-rev.txt` numbers that set. When you change the
pins (or the models), bump the number in the same commit: installed copies then take the
full setup for that update, because their small update package would lack the new DLLs.

**CI caches.** The DLLs (`ci-cache/runtime`, key: the pins) and the models
(`ci-cache/models`, key: `src/models.rs`) are kept in the GitHub Actions cache, so a
release only downloads them again after one of those files changes. The Rust build is
cached too.

### Locally (needs Rust)

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

### Tray and on-demand mode

- **Tray.** Closing the control panel hides it to the system tray; vcam keeps serving the
  virtual camera. Left-click the tray icon (or **Open vcam** in its menu) to bring the
  window back; **Quit vcam** turns everything off and exits. `vcam.exe --tray` starts
  hidden in the tray, for example from a shortcut in the Startup folder
  (`shell:startup`).
- **Only run while an app uses the camera** (card 2, on by default). vcam starts armed:
  "OBS Virtual Camera" is published with a black frame, and the webcam and the model stay
  off (standby, about 0 CPU). When Meet, Zoom, Teams... turn the camera on, vcam starts
  the webcam and the model (the app sees black for the few seconds this takes); 5 seconds
  after the last app turns it off, it goes back to standby. The command line has the
  same mode: `vcam-cli --on-demand`.
- **Preview now** (card 2, in this mode): turns the webcam on without a meeting, so the
  picture can be checked first; the output preview tab opens. **Stop preview**, turning
  vcam off, or hiding it to the tray ends it (an app using the camera keeps it running).
- How "an app is using it" is detected: OBS's virtual-camera filter opens vcam's
  shared-memory queue only while an app streams from it, so vcam counts the handles
  open on that queue. An app that merely lists cameras doesn't count.

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
- **No CPU spin while the GPU works.** With CUDA, vcam sets `cudaDeviceScheduleBlockingSync`
  before the first session, so the thread waiting for the GPU sleeps. CUDA's default spun
  it: 27-38 ms of CPU per frame with rvm_resnet50 (about half a core at 10-30 fps), now
  under 0.5 ms, at the same frame rate.
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
