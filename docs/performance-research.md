# vcam-rs: edge sharpness, fps and CPU research

Nov 19, 2026

## Summary

The frame rate is capped by RVM inference on the GTX 1050 Ti (49 ms per frame at Balanced, so about 20 fps). The CPU load comes from Rayon's 20 spinning threads. The soft edge comes mostly from a feather blur that the compositor adds on top of RVM's already soft matte.

| Change | Edge sharpness | fps | CPU | Effort |
| --- | --- | --- | --- | --- |
| Cap the Rayon pool at 4 threads | none | 18.8 to 20.7 | 4.26 to 0.38 cores (measured) | a few lines |
| Remove the compositor's edge feather | sharper | none | -3.6 ms CPU per frame | a few lines |
| Add an alpha edge curve ("Edge sharpness" slider) | sharper, removes halo | none | negligible | small |
| Run the matte at the size it is shown (zoom > 1) | sharper (no matte resampling) | 20 to 30+ at zoom 1.5 | less | medium |
| Move keep-largest-person off the inference thread | none | -3 to -4 ms per frame | none | small |
| cuDNN exhaustive algorithm search | none | -1.5 to -3 ms per frame | none | one line |

All timings were measured on this PC (i7-12700KF, GTX 1050 Ti 4 GB, driver 560.94, 1280x720 webcam at 30 fps) with a temporary benchmark that is no longer in the project. Nothing in the app has been changed yet.

## Baseline

Three threads run in a chain: the camera thread, the inference thread (crop, mirror, colour, RVM, keep-largest-person) and the output thread (composite, NV12, virtual camera). Only the inference thread is serial, because RVM's recurrent state feeds each frame into the next, so its time per frame sets the fps.

| Stage | Thread | Time per 1280x720 frame |
| --- | --- | --- |
| Camera capture (MJPG decoded by Media Foundation) | camera | 30.3 fps delivered, about 0 CPU |
| RVM MobileNetV3 on CUDA, Balanced (dsr 0.375) | inference | 48-50 ms |
| Keep only the largest person | inference | 2.6-4.1 ms |
| Mirror / crop copy | inference | 0.7-1.0 ms |
| Composite, blur background | output | 8-19 ms |
| Composite, image or colour background | output | 4.6-5.3 ms |
| of which: edge feather (5x5 Gaussian on the matte) | output | 3.6-3.8 ms |
| BGR to NV12 for the virtual camera | output | 0.8-1.0 ms |
| Preview conversion (control panel) | UI | 1.2 ms |

The app as a whole measured 18.8 fps and 4.26 CPU cores with the default Rayon pool, against 10.1 fps and 0.37 cores for the Python version.

## Edge sharpness

The edge is softened three times after RVM: by a feather blur, by a wide alpha transition band, and by resampling the matte when zoom is not 1. Fixing the first two is cheap and needs no model change.

1. **Edge feather (main cause).** `Compositor::render` blurs the matte with a Gaussian of radius min(w, h) / 300, which is 5x5 at 720p (`src/compositor.rs`). It was written for hard MediaPipe masks, to avoid a stencil look. RVM already returns a soft, anti-aliased matte, so the edge is blurred twice. Fix: remove it for RVM, or expose it as an "Edge softness" slider that defaults to 0. It also saves 3.6 ms of CPU per frame.
2. **Wide alpha transition.** RVM spreads the edge over several pixels and leaves a faint semi-transparent halo of the old background. Fix: remap alpha through a contrast curve before blending, for example values under 0.05 become 0, over 0.95 become 1, and the band between is stretched. It is a 256-entry lookup on the u8 matte, so the cost is negligible. Exposed as an "Edge sharpness" slider, the user picks between crisp and hair-friendly.
3. **Matte resampling.** With zoom other than 1, the matte is converted to u8 and then resized with the foreground (area or bilinear). That adds about one pixel of blur. Running the matte at the displayed size removes this step (see Speed).
4. **Coarse-pass resolution (Quality).** dsr sets the coarse network's resolution. Higher dsr gives more detail but costs a lot on this GPU: 0.375 is 49 ms and 0.5 is 72 ms at 1280x720. It only becomes affordable with a smaller input.
5. **Model.** RVM ResNet-50 matches hair and fine edges better than MobileNetV3, but it is 1.8x slower here (90 ms against 49 ms at 1280x720, dsr 0.375). At 854x480 and dsr 0.25 it takes 29 ms, so it only fits 30 fps together with the displayed-size matte.

Not useful on this hardware: FP16 models, because the GTX 1050 Ti (Pascal) has a very low FP16 rate.

## Speed (fps)

Running RVM on the image at the size it is actually shown is the one change that reaches 30 fps without lowering quality. Inference time falls much faster than the pixel count on this GPU, because the full-resolution refine pass and the CPU-GPU copies dominate.

RVM MobileNetV3 on CUDA, ms per frame (the camera delivers one frame every 33 ms):

| Matting input | dsr 0.20 | dsr 0.25 | dsr 0.375 (Balanced) | dsr 0.50 | dsr 0.75 |
| --- | --- | --- | --- | --- | --- |
| 1280x720 (zoom 1.0, today) | 28-30 | 34-36 | 48-50 | 69-74 | 126-138 |
| 1024x576 (zoom 1.25) | 18 | 21 | 30 | 42 | 80 |
| 854x480 (zoom 1.5) | 14 | 16 | 24 | 30 | 59 |
| 640x360 (zoom 2.0) | 13 | 14 | 15 | 20 | 34 |

1. **Matte at the displayed size.** Today the full crop goes through RVM and the compositor then shrinks the result by 1/zoom. Shrinking the crop first gives the same output pixels for a fraction of the work. To keep the coarse pass as detailed as today, scale dsr by zoom (zoom 1.5: dsr 0.56 on 854x480, about 32-35 ms between the measured 30 and 59). Caveats: it changes the image the model receives, which you asked to keep as before in the earlier Quality change, and each zoom change resets RVM's recurrent state for one frame. At zoom 1.0 nothing changes.
2. **Keep-largest-person off the serial path.** It runs on the inference thread after RVM (2.6-4.1 ms). Moving it to the output thread, or running it at quarter resolution, takes that off every frame.
3. **cuDNN exhaustive search.** Switching `ConvAlgorithmSearch::Heuristic` to `Exhaustive` measured 1.5-3 ms faster at dsr 0.25-0.5, with a slower first frame.
4. **Quality preset.** Without change 1, only dsr 0.25 or lower reaches about 30 fps at 1280x720, at the cost of coarser hair.

Tested with no gain: ONNX Runtime intra-op thread count of 1 and disabled spinning (within noise). Not tested: TensorRT, which needs about 1 GB of extra DLLs, and IO binding to keep tensors on the GPU (estimated 2-3 ms, the size of the copies).

## CPU usage

Capping Rayon's thread pool at 4 removes about 90% of the CPU load: 4.26 cores fell to 0.38 cores, and fps rose from 18.8 to 20.7 (CLI, blur, CUDA, no preview). Rayon starts one worker per logical CPU (20 here), and the idle workers spin between the roughly 36 small parallel loops each frame runs. Nothing in the code sets the pool size today.

1. **Rayon pool of 4.** Call `rayon::ThreadPoolBuilder::new().num_threads(4).build_global()` at the start of both `main` functions. The per-frame loops are 1-5 ms of memory-bound work, so 4 threads lose nothing measurable.
2. **Remove the edge feather.** -3.6 ms of CPU per frame, as in Edge sharpness.
3. **Cheaper live blur.** Blur mode costs 8-19 ms per frame against 5 ms for an image background. Each frame it area-resizes the full frame to a quarter, blurs it, and bilinear-resizes it back to full size. A fixed 4x box downscale and an upscale folded into the blend would roughly halve it (estimate, not measured).
4. **Smaller matte input.** The displayed-size matte from Speed also shrinks every per-pixel step on the inference thread (input normalising, foreground conversion, keep-largest).
5. **Preview.** The control panel converts and uploads a preview texture every 33 ms (1.2 ms each). Skipping it while the window is minimised saves that.

Camera capture is not a factor: Media Foundation decodes the MJPG stream at about 0 cores.

## Recommended plan

Do the low-risk changes first; they leave the model input exactly as it is now. Then decide on the displayed-size matte, which gives the large fps gain but changes what the model sees.

1. Cap the Rayon pool at 4 threads.
2. Remove the edge feather, or add an "Edge softness" slider defaulting to 0.
3. Add the alpha edge curve with an "Edge sharpness" slider.
4. Move keep-largest-person to the output thread.
5. Switch cuDNN to exhaustive search.
6. Matte at the displayed size when zoom > 1, with dsr scaled by zoom.
7. Then optionally offer ResNet-50 for zoom 1.5 and above.
8. Optimise the live blur.

Risks:

- Steps 2 and 3 change the look. A crisper edge shows matting mistakes (a missed finger, a stray patch) more clearly, which is why both should be sliders.
- Step 6 makes fps depend on zoom: at zoom 1.0 it stays at about 20 fps at Balanced.
- The edge results are not measured: I cannot see the picture, so they need an A/B check by eye on the real camera.

After each step, rerun `build.cmd` so the release folder matches.
