"""aftercast live captions helper: microphone -> faster-whisper -> JSON lines on stdout.

The streaming loop follows WhisperLive (https://github.com/collabora/WhisperLive, MIT,
Copyright (c) 2023 Vineet Suryan, Collabora Ltd.): Whisper rereads the audio not yet
committed every pass; every segment but the last is final, the last one is shown as a
partial and becomes final once it stops changing.

Prints {"ready": true} once audio flows, then {"text", "final", "start", "end"} per update
(seconds since ready). Exits when stdin closes, so it never outlives aftercast.
"""

import argparse
import json
import os
import subprocess
import sys
import threading
import time

import numpy as np

from transcribe import cuda_dlls

RATE = 16000
# Passes the partial may stay the same before it is taken as final.
SAME_PASSES = 3
# Longest uncommitted audio; past this the partial is committed anyway.
MAX_BUFFER_S = 25.0


def norm(text):
    return " ".join("".join(c for c in text.lower() if c.isalnum() or c.isspace()).split())


def made_up(seg, prompt):
    """What Whisper invents on silence or music: punctuation only, its prompt read back, a
    low-confidence guess, or two words stretched over many seconds ("Thank you.")."""
    text = norm(seg.text)
    long = seg.end - seg.start > 8 and len(text.split()) / (seg.end - seg.start) < 0.3
    return not text or text in prompt or seg.avg_logprob < -1.0 or seg.no_speech_prob > 0.45 or long


def say(**msg):
    print(json.dumps(msg, ensure_ascii=False), flush=True)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--ffmpeg", default="ffmpeg")
    p.add_argument("--device", required=True, help="microphone name (ffmpeg -list_devices true -f dshow -i dummy)")
    p.add_argument("--model", default="large-v3-turbo")
    p.add_argument("--compute", default="float16")
    p.add_argument("--language", default="auto")
    p.add_argument("--models", help="model download folder")
    p.add_argument("--prompt", default="")
    args = p.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")  # Windows pipes default to the ANSI code page
    prompt = norm(args.prompt)

    # aftercast closed the pipe: stop (and ffmpeg follows when its stdout breaks).
    threading.Thread(target=lambda: (sys.stdin.read(), os._exit(0)), daemon=True).start()

    cuda_dlls()
    from faster_whisper import WhisperModel

    model = WhisperModel(args.model, device="cuda", compute_type=args.compute, download_root=args.models)
    # dshow buffers 500 ms of audio by default; 50 ms keeps the captions close to the voice.
    source = {
        "win32": ["-f", "dshow", "-audio_buffer_size", "50", "-i", f"audio={args.device}"],
        "darwin": ["-f", "avfoundation", "-i", f":{args.device}"],
    }.get(sys.platform, ["-f", "pulse", "-i", args.device])
    ff = subprocess.Popen(
        [args.ffmpeg, "-hide_banner", "-loglevel", "error", *source,
         "-ac", "1", "-ar", str(RATE), "-f", "s16le", "-"],
        stdout=subprocess.PIPE,
    )

    lock = threading.Lock()
    chunks = []

    def read_audio():
        while data := ff.stdout.read(RATE // 10 * 2):  # 0.1 s
            with lock:
                chunks.append(np.frombuffer(data, np.int16).astype(np.float32) / 32768.0)
        sys.stderr.write(f"ffmpeg stopped ({ff.wait()})\n")
        os._exit(1)

    threading.Thread(target=read_audio, daemon=True).start()
    buf = np.zeros(0, np.float32)
    offset = 0.0  # seconds of audio already committed and dropped from buf
    language = None if args.language == "auto" else args.language
    last, same = "", 0
    ready = False
    while True:
        with lock:
            new, chunks[:] = chunks[:], []
        # A pass only when new audio arrived (every 0.1 s while the mic runs): rereading the
        # same audio would only take GPU from OBS.
        if not new:
            time.sleep(0.01)
            continue
        buf = np.concatenate([buf, *new])
        if not ready:
            ready = True
            say(ready=True)
        if len(buf) < RATE // 2:
            continue
        segs, info = model.transcribe(
            buf, language=language, initial_prompt=args.prompt or None, vad_filter=True,
            condition_on_previous_text=False, beam_size=1,
        )
        segs = [s for s in segs if not made_up(s, prompt)]
        if language is None and info.language_probability > 0.5:
            language = info.language  # detected once, like WhisperLive
        dur = len(buf) / RATE
        if not segs:
            # Silence: drop all but the last second (speech may be starting).
            cut = max(0.0, dur - 1.0)
        else:
            for s in segs[:-1]:
                say(text=s.text.strip(), final=True, start=round(offset + s.start, 2), end=round(offset + s.end, 2))
            tail = segs[-1]
            text = tail.text.strip()
            same = same + 1 if text == last else 0
            last = text
            if same >= SAME_PASSES or dur > MAX_BUFFER_S:
                say(text=text, final=True, start=round(offset + tail.start, 2), end=round(offset + tail.end, 2))
                cut, last, same = min(dur, tail.end), "", 0
            else:
                say(text=text, final=False, start=round(offset + tail.start, 2), end=round(offset + tail.end, 2))
                cut = segs[-2].end if len(segs) > 1 else 0.0
        n = int(cut * RATE)
        buf, offset = buf[n:], offset + n / RATE


if __name__ == "__main__":
    main()
