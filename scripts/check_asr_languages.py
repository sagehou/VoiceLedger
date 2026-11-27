"""Evaluate the packaged offline ASR adapters on the same synthetic speech."""
import argparse
from array import array
import ctypes
import hashlib
import io
import json
import os
import platform
import re
import unicodedata
from pathlib import Path
import subprocess
import sys
import tempfile
from time import perf_counter
import wave

from build_speech import SHERPA_RELEASE, fetch, unpack

RATE = 16000
ROOT = Path(__file__).resolve().parents[1]
SAMPLES = {
    "zh": ("cmn", "大家好，欢迎参加今天的会议。请把会议录音保存在本地。我们稍后一起核对发言记录。",
           "明天下午三点开始测试。请记下设备名称和录音时间。结束之后请把结果发给同事。"),
    "ja": ("ja", "みなさん、こんにちは。今日の会議を始めます。録音した音声はこの端末に保存してください。",
           "明日の午後に新しい録音をテストします。電池の残量を確かめてから、マイクの設定を確認してください。"),
    "en": ("en-us", "Good morning, everyone. Today's meeting will start in a minute. Please save the recording on this device.",
           "The microphone is ready for the next test. Check the battery before you start, and write down the time when you finish."),
}
CODES = {1: "zh", 2: "en", 3: "ja"}
WHISPER = "whisper-tiny-multilingual-q5-1"
SENSEVOICE = "sensevoice-small-int8"


def candidate(model_id):
    catalog = json.loads((ROOT / "models/catalog-v1.json").read_text(encoding="utf-8"))
    return next(model for model in catalog["models"] if model["id"] == model_id)


def download(folder, model):
    folder.mkdir(parents=True, exist_ok=True)
    for artifact in model["artifacts"]:
        path = fetch(folder, artifact["file"], artifact["url"], artifact["sha256"])
        if path.stat().st_size != artifact["size_bytes"]:
            raise RuntimeError("Unexpected model size")


def prepare(folder):
    folder = folder.resolve()
    for model_id in (WHISPER, SENSEVOICE):
        download(folder, candidate(model_id))
    cli = "sherpa-onnx-v1.12.20-linux-x64-shared"
    voice_package = "kokoro-int8-multi-lang-v1_0"
    unpack(fetch(folder, cli + ".tar.bz2", SHERPA_RELEASE + cli + ".tar.bz2",
                 "13a2c502f5ef373f514a5fc5c5ad03cc0798dcb564a6f8cbd54f8f6ce9e47462"), folder)
    unpack(fetch(folder, voice_package + ".tar.bz2",
                 "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/" + voice_package + ".tar.bz2",
                 "72c347230107ef8a31098ec4d468a605244beef5f1ed00db97efa13518800653"), folder)
    runtime = folder / cli
    voice_model = folder / voice_package
    weights, = voice_model.glob("*.onnx")
    sh_env = dict(os.environ, LD_LIBRARY_PATH=str(runtime / "lib"))
    paths = lambda package: subprocess.check_output(["dpkg", "-L", package], text=True).splitlines()
    dictionary, = (Path(path).parent for path in paths("open-jtalk-mecab-naist-jdic") if path.endswith("/sys.dic"))
    japanese, = (path for path in paths("hts-voice-nitech-jp-atr503-m001") if path.endswith(".htsvoice"))
    for language, (voice, *sentences) in SAMPLES.items():
        for index, sentence in enumerate(sentences, 1):
            output = folder / f"{language}-{index}.wav"
            if language == "zh":
                subprocess.run([
                    str(runtime / "bin/sherpa-onnx-offline-tts"), "--debug=0", "--num-threads=2",
                    f"--kokoro-model={weights}", f"--kokoro-voices={voice_model / 'voices.bin'}",
                    f"--kokoro-tokens={voice_model / 'tokens.txt'}", f"--kokoro-data-dir={voice_model / 'espeak-ng-data'}",
                    f"--kokoro-lexicon={voice_model / 'lexicon-us-en.txt'},{voice_model / 'lexicon-zh.txt'}",
                    "--sid=45", f"--output-filename={output}", sentence,
                ], check=True, env=sh_env)
            elif language == "ja":
                with tempfile.TemporaryDirectory(dir=folder) as temp:
                    text = Path(temp) / "text"
                    text.write_text(sentence + "\n", encoding="utf-8")
                    subprocess.run(["open_jtalk", "-x", str(dictionary), "-m", japanese, "-r", "0.95",
                                    "-ow", str(output), str(text)], check=True)
            else:
                audio = subprocess.check_output(["espeak-ng", "-v", voice, "-s", "145", "--stdout", sentence])
                output.write_bytes(audio)


def waveform(path):
    with wave.open(io.BytesIO(path.read_bytes())) as source:
        rate = source.getframerate()
        if source.getnchannels() != 1 or source.getsampwidth() != 2:
            raise RuntimeError("Expected mono 16-bit fixture")
        pcm = array("h", source.readframes(rate * 30))
    if sys.byteorder != "little":
        pcm.byteswap()
    return array("f", (
        (pcm[left] * (RATE - rest) + pcm[min(left + 1, len(pcm) - 1)] * rest) / (RATE * 32768)
        for left, rest in (divmod(index * rate, RATE) for index in range(len(pcm) * RATE // rate))
    ))


def units(text, language):
    text = unicodedata.normalize("NFKC", text).casefold()
    if language == "en":
        return re.findall(r"[a-z0-9]+(?:['’][a-z]+)?", text)
    return [char for char in text if char.isalnum()]


def edits(reference, text):
    row = list(range(len(text) + 1))
    for i, expected in enumerate(reference, 1):
        next_row = [i]
        for j, actual in enumerate(text, 1):
            next_row.append(min(next_row[-1] + 1, row[j] + 1, row[j - 1] + (actual != expected)))
        row = next_row
    return row[-1]


def evaluate(folder, library, report, model):
    import resource

    paths = []
    for artifact in model["artifacts"]:
        path = folder / artifact["file"]
        with path.open("rb") as stream:
            if path.stat().st_size != artifact["size_bytes"] or hashlib.file_digest(stream, "sha256").hexdigest() != artifact["sha256"]:
                raise RuntimeError(f"Model verification failed for {path.name}")
        paths.append(os.fsencode(path.resolve()))
    dll = ctypes.CDLL(str(library.resolve()))
    dll.vl_asr_version.restype = ctypes.c_char_p
    dll.vl_asr_open.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_int32, ctypes.c_int32]
    dll.vl_asr_open.restype = ctypes.c_void_p
    dll.vl_asr_run.argtypes = [
        ctypes.c_void_p, ctypes.POINTER(ctypes.c_float), ctypes.c_int32, ctypes.c_void_p,
        ctypes.c_int32, ctypes.POINTER(ctypes.c_int32), ctypes.c_void_p, ctypes.c_void_p,
    ]
    dll.vl_asr_run.restype = ctypes.c_int32
    dll.vl_asr_close.argtypes = [ctypes.c_void_p]
    dll.vl_asr_close.restype = None
    began = perf_counter()
    engine = dll.vl_asr_open(paths[0], paths[1] if len(paths) > 1 else None, 0, 2)
    load_ms = round((perf_counter() - began) * 1000)
    if not engine:
        raise RuntimeError(f"{model['name']} could not initialize")
    results = []
    try:
        for expected, (_, *sentences) in SAMPLES.items():
            for index, reference in enumerate(sentences, 1):
                sample = folder / f"{expected}-{index}.wav"
                pcm = waveform(sample)
                audio = (ctypes.c_float * len(pcm)).from_buffer(pcm)
                text = ctypes.create_string_buffer(8192)
                language = ctypes.c_int32()
                started = perf_counter()
                length = dll.vl_asr_run(engine, audio, len(pcm), text, len(text), ctypes.byref(language), None, None)
                elapsed = perf_counter() - started
                if not 0 <= length <= len(text):
                    raise RuntimeError("ASR inference failed")
                transcript = text.raw[:length].decode("utf-8").strip()
                expected_units = units(reference, expected)
                results.append({
                    "fixture": sample.name,
                    "sha256": hashlib.sha256(sample.read_bytes()).hexdigest(),
                    "expected": expected,
                    "detected": CODES.get(language.value, "unknown"),
                    "reference": reference,
                    "text": transcript,
                    "audio_ms": round(len(pcm) * 1000 / RATE),
                    "latency_ms": round(elapsed * 1000),
                    "rtf": round(elapsed * RATE / len(pcm), 4),
                    "edits": edits(expected_units, units(transcript, expected)),
                    "reference_units": len(expected_units),
                })
    finally:
        dll.vl_asr_close(engine)
    scores = {}
    for lang in SAMPLES:
        subset = [sample for sample in results if sample["expected"] == lang]
        scores[lang] = {
            "metric": "WER" if lang == "en" else "CER",
            "error_rate": round(sum(sample["edits"] for sample in subset) / sum(sample["reference_units"] for sample in subset), 4),
            "rtf": round(sum(sample["latency_ms"] for sample in subset) / sum(sample["audio_ms"] for sample in subset), 4),
            "lang_correct": sum(sample["detected"] == lang for sample in subset),
        }
    report.write_text(json.dumps({
        "platform": platform.platform(), "commit": os.environ.get("GITHUB_SHA"),
        "model": model["id"], "model_sha256": model["artifacts"][0]["sha256"],
        "runtime": dll.vl_asr_version().decode(), "policy": "auto", "threads": 2,
        "load_ms": load_ms, "peak_rss_kib": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
        "scoring": "NFKC, case-folded, punctuation excluded; Chinese script variants are scored as written",
        "inputs": {"zh": "Kokoro 1.0 INT8 / zh sid 45", "ja": "Open JTalk / Nitech ATR503", "en": "eSpeak NG en-us"},
        "scores": scores, "samples": results,
    }, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(model["id"], scores)
    if any(sample["expected"] != sample["detected"] or not sample["text"] for sample in results):
        raise RuntimeError("Auto policy did not recognize every fixture")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("folder", type=Path)
    parser.add_argument("--library", type=Path)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--model", choices=(WHISPER, SENSEVOICE), default=WHISPER)
    args = parser.parse_args()
    if args.library:
        evaluate(args.folder, args.library, args.report, candidate(args.model))
    else:
        prepare(args.folder)


if __name__ == "__main__":
    main()
