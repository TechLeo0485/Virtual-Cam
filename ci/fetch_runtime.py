"""Download the wheels pinned in runtime-requirements.txt (Windows x64) and extract the
DLLs vcam needs into OUT, in the layout vcam looks for next to vcam.exe:

    OUT/onnxruntime.dll, onnxruntime_providers_cuda.dll, onnxruntime_providers_shared.dll
    OUT/nvidia/<package>/bin/*.dll      (cuda_runtime, cublas, cudnn, ...)

usage: python ci/fetch_runtime.py OUT

The wheels go to a temporary folder and are deleted afterwards; CI caches OUT itself,
keyed on this file and runtime-requirements.txt.
"""
import pathlib
import shutil
import subprocess
import sys
import tempfile
import zipfile

# Not used by vcam: TensorRT provider (no TensorRT libraries are shipped), the BLAS and
# FFTW compatibility wrappers.
SKIP = {"onnxruntime_providers_tensorrt.dll", "nvblas64_12.dll", "cufftw64_11.dll"}


def main() -> None:
    out = pathlib.Path(sys.argv[1])
    req = pathlib.Path(__file__).with_name("runtime-requirements.txt")
    if out.exists():
        shutil.rmtree(out)
    count = 0
    with tempfile.TemporaryDirectory() as tmp:
        subprocess.check_call([
            sys.executable, "-m", "pip", "download", "--disable-pip-version-check",
            "--no-deps", "--only-binary=:all:", "--platform", "win_amd64",
            "--python-version", "3.12", "--implementation", "cp",
            "-r", str(req), "-d", tmp,
        ])
        for whl in sorted(pathlib.Path(tmp).glob("*.whl")):
            with zipfile.ZipFile(whl) as z:
                for name in z.namelist():
                    p = pathlib.PurePosixPath(name)
                    if p.suffix.lower() != ".dll" or p.name in SKIP:
                        continue
                    if name.startswith("onnxruntime/capi/"):
                        dest = out / p.name
                    elif p.parts[0] == "nvidia" and len(p.parts) == 4 and p.parts[2] == "bin":
                        dest = out.joinpath(*p.parts)  # nvidia/<package>/bin/<dll>
                    else:
                        continue
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    with z.open(name) as src, open(dest, "wb") as dst:
                        shutil.copyfileobj(src, dst)
                    count += 1
    needed = ["onnxruntime.dll", "onnxruntime_providers_cuda.dll", "onnxruntime_providers_shared.dll",
              "nvidia/cuda_runtime/bin/cudart64_12.dll", "nvidia/cudnn/bin/cudnn64_9.dll",
              "nvidia/cublas/bin/cublas64_12.dll"]
    missing = [n for n in needed if not (out / n).is_file()]
    if missing:
        sys.exit(f"missing after extraction: {missing}")
    print(f"extracted {count} DLLs into {out}")


if __name__ == "__main__":
    main()
