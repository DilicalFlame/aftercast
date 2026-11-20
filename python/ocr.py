"""aftercast OCR helper (specs/11-post-pipeline.md, stage 9), installed by aftercast setup.

ocr.py --tessdata DIR --download        fetch eng.traineddata into DIR (once)
ocr.py --tessdata DIR --list F --out J  OCR each image path listed in F (one per line) and
                                        write J: {"<path>": "<text>"}. Prints "progress <0..1>".
"""

import argparse
import json
import os
import urllib.request

DATA_URL = "https://github.com/tesseract-ocr/tessdata_fast/raw/main/{}.traineddata"


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


def ocr(args):
    from tesserocr import PSM, PyTessBaseAPI

    with open(args.list, encoding="utf-8") as f:
        images = [line.strip() for line in f if line.strip()]
    out = {}
    with PyTessBaseAPI(path=args.tessdata, lang=args.lang, psm=PSM.SINGLE_BLOCK) as api:
        for i, image in enumerate(images):
            api.SetImageFile(image)
            out[image] = api.GetUTF8Text()
            print(f"progress {(i + 1) / len(images):.4f}", flush=True)
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--tessdata", required=True)
    p.add_argument("--lang", default="eng")
    p.add_argument("--download", action="store_true")
    p.add_argument("--list")
    p.add_argument("--out")
    args = p.parse_args()
    if args.download:
        download(args.tessdata, args.lang)
    elif args.list and args.out:
        ocr(args)
    else:
        p.error("--download, or --list and --out")


if __name__ == "__main__":
    main()
