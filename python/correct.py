"""aftercast correction helper (stage `correct`), installed by aftercast setup.

correct.py --model OWNER/REPO/FILE --models DIR --download   fetch the GGUF model (once)
correct.py --model ... --models DIR --check                  print versions if it can run
correct.py --model ... --models DIR --tasks T --out O        answer the questions in T

T: {"context", "questions": [{"q", "before", "text", "word", "options"}]}: for each, keep
the [bracketed] word (A) or take one of the look-alike terms (B, C, ...).
O: {"answers": [{"q", "choice"}]}; the JSON schema grammar only allows those letters, so the
model cannot invent words. Prints "progress <0..1>".
"""

import argparse
import glob
import json
import os
import sys

BATCH = 12

SYSTEM = (
    "You check a speech recognition transcript of a live coding stream (English, sometimes "
    "mixed with Hindi). Each question shows a sentence with one word in [brackets] and asks "
    "whether the speaker said that word (A) or one of the listed terms that sound alike "
    "(B, C, ...). Decide from the meaning of the sentence. Choose a term only when the "
    "sentence is clearly about it; ordinary English words are usually right, so when in "
    "doubt choose A."
)


def cuda_dlls():
    """Puts the DLLs of the nvidia-* wheels (installed by aftercast setup) on the search path."""
    if sys.platform != "win32":
        return
    for d in glob.glob(os.path.join(sys.prefix, "Lib", "site-packages", "nvidia", "*", "bin")):
        os.add_dll_directory(d)
        os.environ["PATH"] = d + os.pathsep + os.environ["PATH"]


def model_path(spec, models, download):
    from huggingface_hub import hf_hub_download

    owner, repo, file = spec.split("/", 2)
    return hf_hub_download(f"{owner}/{repo}", file, cache_dir=models, local_files_only=not download)


def question_text(q):
    options = [f"A) {q['word']}"] + [f"{chr(66 + i)}) {o}" for i, o in enumerate(q["options"])]
    before = f"{q['before']} " if q["before"] else ""
    return f"Q{q['q']}: {before}{q['text']}\n    {'   '.join(options)}"


def answer(llm, tasks):
    questions = tasks["questions"]
    letters = [chr(65 + i) for i in range(1 + max(len(q["options"]) for q in questions))]
    schema = {
        "type": "object",
        "properties": {
            "answers": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {"q": {"type": "integer"}, "choice": {"enum": letters}},
                    "required": ["q", "choice"],
                },
            }
        },
        "required": ["answers"],
    }
    out = []
    for start in range(0, len(questions), BATCH):
        batch = questions[start : start + BATCH]
        user = (
            f"Stream: {tasks['context']}\n\n"
            + "\n".join(question_text(q) for q in batch)
            + f"\n\nAnswer each of the {len(batch)} questions with its number and a letter."
        )
        r = llm.create_chat_completion(
            messages=[{"role": "system", "content": SYSTEM}, {"role": "user", "content": user}],
            response_format={"type": "json_object", "schema": schema},
            temperature=0,
            max_tokens=16 * len(batch) + 32,
        )
        try:
            out.extend(json.loads(r["choices"][0]["message"]["content"]).get("answers", []))
        except (json.JSONDecodeError, KeyError):
            pass  # a cut-off answer: these words stay as they are
        print(f"progress {min((start + BATCH) / len(questions), 1.0):.4f}", flush=True)
    return out


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--model", required=True, help="Hugging Face owner/repo/file.gguf")
    p.add_argument("--models", required=True)
    p.add_argument("--download", action="store_true")
    p.add_argument("--check", action="store_true")
    p.add_argument("--tasks")
    p.add_argument("--out")
    args = p.parse_args()
    cuda_dlls()
    if args.download:
        print(model_path(args.model, args.models, True), flush=True)
        return
    path = model_path(args.model, args.models, False)
    import llama_cpp

    if args.check:
        print(f"llama-cpp-python {llama_cpp.__version__}, {os.path.basename(path)}", flush=True)
        return
    if not (args.tasks and args.out):
        p.error("--tasks and --out are required")
    with open(args.tasks, encoding="utf-8") as f:
        tasks = json.load(f)
    llm = llama_cpp.Llama(model_path=path, n_gpu_layers=-1, n_ctx=8192, verbose=False)
    answers = answer(llm, tasks) if tasks["questions"] else []
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump({"answers": answers}, f, ensure_ascii=False)
    print("progress 1.0", flush=True)


if __name__ == "__main__":
    main()
