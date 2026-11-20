"""aftercast transcription helper (specs/11-post-pipeline.md, stage 5).

Writes words.json: {"language", "model", "segments": [{"start", "end", "text",
"words": [{"w", "start", "end", "p"}]}]}, times in seconds. Prints "progress <0..1>" lines.

Needs:  aftercast setup   (or: pip install faster-whisper / whisperx)
GPU:    CUDA 12 + cuDNN 9; use --compute int8_float16 if VRAM runs out.
"""

import argparse
import glob
import json
import os
import sys


def cuda_dlls():
    """Puts the DLLs of the nvidia-* wheels (installed by aftercast setup) on the search path."""
    if sys.platform != "win32":
        return
    for d in glob.glob(os.path.join(sys.prefix, "Lib", "site-packages", "nvidia", "*", "bin")):
        os.add_dll_directory(d)
        os.environ["PATH"] = d + os.pathsep + os.environ["PATH"]


def faster_whisper(args):
    from faster_whisper import WhisperModel

    model = WhisperModel(
        args.model, device=args.device, compute_type=args.compute, download_root=args.models
    )
    segments, info = model.transcribe(
        args.input,
        language=None if args.language == "auto" else args.language,
        vad_filter=True,
        word_timestamps=True,
        initial_prompt=args.prompt or None,
        hotwords=args.hotwords or None,
    )
    out = []
    for seg in segments:
        out.append(
            {
                "start": round(seg.start, 3),
                "end": round(seg.end, 3),
                "text": seg.text.strip(),
                "words": [
                    {
                        "w": w.word.strip(),
                        "start": round(w.start, 3),
                        "end": round(w.end, 3),
                        "p": round(w.probability, 3),
                    }
                    for w in (seg.words or [])
                ],
            }
        )
        if info.duration:
            print(f"progress {min(seg.end / info.duration, 1.0):.4f}", flush=True)
    return info.language, out


def whisperx(args):
    import whisperx as wx

    audio = wx.load_audio(args.input)
    model = wx.load_model(
        args.model,
        args.device,
        compute_type=args.compute,
        download_root=args.models,
        asr_options={"initial_prompt": args.prompt or None},
    )
    language = None if args.language == "auto" else args.language
    result = model.transcribe(audio, language=language)
    print("progress 0.6", flush=True)
    align, meta = wx.load_align_model(language_code=result["language"], device=args.device)
    aligned = wx.align(result["segments"], align, meta, audio, args.device)
    out = []
    for seg in aligned["segments"]:
        out.append(
            {
                "start": round(seg["start"], 3),
                "end": round(seg["end"], 3),
                "text": seg["text"].strip(),
                "words": [
                    {
                        "w": w["word"].strip(),
                        "start": round(w["start"], 3),
                        "end": round(w["end"], 3),
                        "p": round(w.get("score", 0.0), 3),
                    }
                    for w in seg.get("words", [])
                    if "start" in w and "end" in w
                ],
            }
        )
    return result["language"], out


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--backend", choices=["faster-whisper", "whisperx"], default="faster-whisper")
    p.add_argument("--model", default="large-v3")
    p.add_argument("--compute", default="float16")
    p.add_argument("--device", default="cuda")
    p.add_argument("--language", default="auto")
    p.add_argument("--models", help="model download folder (default: the Hugging Face cache)")
    p.add_argument("--prompt", default="", help="context Whisper reads first (series, topics, terms)")
    p.add_argument("--hotwords", default="", help="words to prefer in every window")
    p.add_argument("--download", action="store_true", help="only download the model")
    p.add_argument("--in", dest="input")
    p.add_argument("--out")
    args = p.parse_args()
    cuda_dlls()
    if args.download:
        from faster_whisper import download_model

        print(download_model(args.model, cache_dir=args.models), flush=True)
        return
    if not (args.input and args.out):
        p.error("--in and --out are required")
    run = faster_whisper if args.backend == "faster-whisper" else whisperx
    try:
        language, segments = run(args)
    except ImportError as e:
        sys.exit(f"{e}: run aftercast setup (or pip install {args.backend})")
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump({"language": language, "model": args.model, "segments": segments}, f, ensure_ascii=False)
    print("progress 1.0", flush=True)


if __name__ == "__main__":
    main()
