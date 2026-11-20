"""aftercast OCR helper (specs/11-post-pipeline.md, stage 9), installed by aftercast setup.

Two engines:
- GLM-OCR (default): a 0.9B vision model through llama.cpp's llama-server on the GPU, several
  frames in flight at once.
- Tesseract (fallback): the tesserocr wheel.

ocr.py --tessdata DIR --download                  fetch eng.traineddata into DIR (once)
ocr.py --setup-glm --llama DIR --models DIR       fetch llama.cpp (Windows, CUDA 12.4) and the
                                                  GLM-OCR model (once)
ocr.py --check-glm --llama DIR --models DIR       print what would run
ocr.py --engine glm --llama DIR --models DIR --parallel N --list F --out J
ocr.py --engine tesseract --tessdata DIR --list F --out J
    OCR each image path listed in F (one per line); write J: {"<path>": "<text>"}.
    Prints "progress <0..1>".
"""

import argparse
import base64
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import zipfile
from concurrent.futures import ThreadPoolExecutor

DATA_URL = "https://github.com/tesseract-ocr/tessdata_fast/raw/main/{}.traineddata"

# Pinned and tested (2026-11-20): llama.cpp build, GLM-OCR weights and vision projector.
LLAMA_RELEASE = "b12559"
LLAMA_ZIPS = [
    f"llama-{LLAMA_RELEASE}-bin-win-cuda-12.4-x64.zip",
    "cudart-llama-bin-win-cuda-12.4-x64.zip",
]
GLM_REPO = "ggml-org/GLM-OCR-GGUF"
GLM_MODEL = "GLM-OCR-Q8_0.gguf"
GLM_MMPROJ = "mmproj-GLM-OCR-Q8_0.gguf"
GLM_PROMPT = "Text Recognition:"
# The model can loop on long runs ("hoeeeeee"); DRY stops that. Repeats up to 12 tokens stay
# allowed, or legitimate ones (dates in a file list) get changed.
SAMPLING = {
    "temperature": 0,
    "max_tokens": 1500,
    "dry_multiplier": 0.8,
    "dry_base": 1.75,
    "dry_allowed_length": 12,
}
SLOT_CONTEXT = 8192


def download(tessdata, lang):
    os.makedirs(tessdata, exist_ok=True)
    path = os.path.join(tessdata, f"{lang}.traineddata")
    if os.path.exists(path):
        print(path)
        return
    with urllib.request.urlopen(DATA_URL.format(lang), timeout=60) as r:
        data = r.read()
    with open(path + ".part", "wb") as f:
        f.write(data)
    os.replace(path + ".part", path)
    print(path)


def server_exe(llama):
    return os.path.join(llama, "llama-server.exe" if sys.platform == "win32" else "llama-server")


def glm_files(models, download):
    from huggingface_hub import hf_hub_download

    return [
        hf_hub_download(GLM_REPO, f, cache_dir=models, local_files_only=not download)
        for f in (GLM_MODEL, GLM_MMPROJ)
    ]


def setup_glm(llama, models):
    """llama.cpp from its GitHub release (unless already there) and the model files."""
    if sys.platform != "win32":
        sys.exit("GLM-OCR setup is automatic on Windows only; install llama.cpp and set post.ocr_engine")
    if not os.path.exists(server_exe(llama)):
        os.makedirs(llama, exist_ok=True)
        with tempfile.TemporaryDirectory() as tmp:
            for name in LLAMA_ZIPS:
                url = f"https://github.com/ggml-org/llama.cpp/releases/download/{LLAMA_RELEASE}/{name}"
                path = os.path.join(tmp, name)
                with urllib.request.urlopen(url, timeout=120) as r, open(path, "wb") as f:
                    shutil.copyfileobj(r, f)
                with zipfile.ZipFile(path) as z:
                    for member in z.infolist():
                        if member.is_dir():
                            continue
                        # Flat: llama-server.exe and its DLLs side by side.
                        target = os.path.join(llama, os.path.basename(member.filename))
                        with z.open(member) as src, open(target, "wb") as dst:
                            shutil.copyfileobj(src, dst)
    model, mmproj = glm_files(models, True)
    print(f"llama.cpp {LLAMA_RELEASE}, {os.path.basename(model)}", flush=True)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def ocr_glm(args, images):
    model, mmproj = glm_files(args.models, False)
    port = free_port()
    log = open(args.out + ".server.log", "w", encoding="utf-8")
    server = subprocess.Popen(
        [server_exe(args.llama), "-m", model, "--mmproj", mmproj, "-ngl", "99",
         "--host", "127.0.0.1", "--port", str(port), "--parallel", str(args.parallel),
         "-c", str(SLOT_CONTEXT * args.parallel), "--no-webui"],
        stdout=log, stderr=subprocess.STDOUT,
    )
    base = f"http://127.0.0.1:{port}"
    try:
        deadline = time.time() + 180
        while True:
            if server.poll() is not None:
                sys.exit(f"llama-server exited ({server.returncode}); see {args.out}.server.log")
            try:
                with urllib.request.urlopen(base + "/health", timeout=2) as r:
                    if r.status == 200:
                        break
            except (urllib.error.URLError, ConnectionError, TimeoutError):
                pass  # still starting or loading the model (503)
            if time.time() > deadline:
                sys.exit("llama-server did not start within 3 minutes")
            time.sleep(0.5)

        def one(image):
            with open(image, "rb") as f:
                data = base64.b64encode(f.read()).decode()
            body = {
                "messages": [{"role": "user", "content": [
                    {"type": "image_url", "image_url": {"url": f"data:image/jpeg;base64,{data}"}},
                    {"type": "text", "text": GLM_PROMPT},
                ]}],
                **SAMPLING,
            }
            req = urllib.request.Request(
                base + "/v1/chat/completions",
                data=json.dumps(body).encode(),
                headers={"Content-Type": "application/json"},
            )
            with urllib.request.urlopen(req, timeout=600) as r:
                return json.load(r)["choices"][0]["message"]["content"]

        out = {}
        with ThreadPoolExecutor(args.parallel) as pool:
            for i, (image, text) in enumerate(zip(images, pool.map(one, images))):
                out[image] = text
                print(f"progress {(i + 1) / len(images):.4f}", flush=True)
        return out
    finally:
        server.terminate()
        server.wait()
        log.close()
        try:
            os.remove(args.out + ".server.log")
        except OSError:
            pass


def ocr_tesseract(args, images):
    from tesserocr import PSM, PyTessBaseAPI

    out = {}
    with PyTessBaseAPI(path=args.tessdata, lang=args.lang, psm=PSM.SINGLE_BLOCK) as api:
        for i, image in enumerate(images):
            api.SetImageFile(image)
            out[image] = api.GetUTF8Text()
            print(f"progress {(i + 1) / len(images):.4f}", flush=True)
    return out


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--engine", choices=["glm", "tesseract"], default="tesseract")
    p.add_argument("--tessdata")
    p.add_argument("--lang", default="eng")
    p.add_argument("--llama", help="folder with llama-server")
    p.add_argument("--models", help="model download folder")
    p.add_argument("--parallel", type=int, default=4)
    p.add_argument("--download", action="store_true")
    p.add_argument("--setup-glm", action="store_true")
    p.add_argument("--check-glm", action="store_true")
    p.add_argument("--list")
    p.add_argument("--out")
    args = p.parse_args()
    if args.download:
        return download(args.tessdata, args.lang)
    if args.setup_glm:
        return setup_glm(args.llama, args.models)
    if args.check_glm:
        if not os.path.exists(server_exe(args.llama)):
            sys.exit(f"no {server_exe(args.llama)}")
        model, _ = glm_files(args.models, False)
        return print(f"GLM-OCR ({os.path.basename(model)}) on llama.cpp {LLAMA_RELEASE}")
    if not (args.list and args.out):
        p.error("--download, --setup-glm, --check-glm, or --list and --out")
    with open(args.list, encoding="utf-8") as f:
        images = [line.strip() for line in f if line.strip()]
    out = ocr_glm(args, images) if args.engine == "glm" else ocr_tesseract(args, images)
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False)


if __name__ == "__main__":
    main()
