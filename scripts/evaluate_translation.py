"""Measure one offline candidate on synthetic inputs in GitHub Actions."""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import resource
import socket
import statistics
import subprocess
from time import perf_counter
from zipfile import ZipFile


WHEEL = "bergamot-0.4.5-cp310-cp310-manylinux_2_17_x86_64.manylinux2014_x86_64.whl"
WHEEL_URL = "https://github.com/browsermt/bergamot-translator/releases/download/v0.4.5/"
WHEEL_SHA256 = "2582019813a491af5a3a4e7f3823ceb9aeb1653a3b682a706c0455536355e56d"
MODULE = "_bergamot.cpython-310-x86_64-linux-gnu.so"
ATTACHMENTS = "https://firefox-settings-attachments.cdn.mozilla.net/main-workspace/translations-models/"
MODEL = "model.enzh.intgemm.alphas.bin"
SOURCE_VOCAB = "srcvocab.enzh.spm"
TARGET_VOCAB = "trgvocab.enzh.spm"
SHORTLIST = "lex.50.50.enzh.s2t.bin"
# Remote Settings publishes these models uncompressed. Both versions share
# the vocabularies and shortlist, with the same SHA-256 digests.
WEIGHTS = {
    "2.0": (
        "ff689b9c-0ab6-4562-bfa5-6902a94523bd.bin", 42_992_955,
        "ce4486f728641a36269a245248dcb53405e76d96d9eba68dcb4172f29521e092",
    ),
    "2.2": (
        "a7ff7d5e-e67e-406c-a34b-a7edea35b10e.bin", 43_849_787,
        "4e5accc141373565ddc8fa1565bceaa8d0c3482a82cab8131c719ebcc6c2157c",
    ),
}
SHARED_FILES = {
    SOURCE_VOCAB: (
        "57b4392b-58f1-448f-b968-857e99e1a501.spm", 806_952,
        "bd9b65504acc6d9726dd281f7defc2adb7c2c22d0688fe2f84697de25197c8c5",
    ),
    TARGET_VOCAB: (
        "0c8d28e9-8bbd-4570-8cf6-1188fee24d2b.spm", 772_004,
        "aded6993c36e440284d11cec3f6b8aef9c0e43188a772d80be342a713adf223d",
    ),
    SHORTLIST: (
        "3719cb80-29be-4a9d-b5c0-25500c95f822.bin", 6_506_248,
        "4a5e5827788060f1d718a8132b69440929387514a045796e9b77f935db68c055",
    ),
}


def download(url, path, size, digest):
    subprocess.run([
        "curl", "--fail", "--silent", "--show-error", "--location", "--retry", "3",
        "--max-time", "90", "--max-filesize", str(size), "--output", str(path), url,
    ], check=True)
    if path.stat().st_size != size or hashlib.sha256(path.read_bytes()).hexdigest() != digest:
        raise ValueError(f"Size or SHA-256 mismatch for {path.name}")


def fetch(folder, files):
    folder.mkdir(parents=True, exist_ok=True)
    wheel = folder / WHEEL
    download(WHEEL_URL + WHEEL, wheel, 11_007_126, WHEEL_SHA256)
    with ZipFile(wheel) as archive:
        (folder / MODULE).write_bytes(archive.read(f"bergamot/{MODULE}"))
    for name, (location, size, digest) in files.items():
        download(ATTACHMENTS + location, folder / name, size, digest)


def evaluate(folder, version, files, corpus, report):
    interfaces = [name for _, name in socket.if_nameindex()]
    if interfaces != ["lo"]:
        raise RuntimeError(f"Expected an isolated network namespace, got {interfaces}")
    samples = corpus.read_bytes()
    cases = json.loads(samples)
    assert 10 <= len(cases) <= 20

    started = perf_counter()
    spec = importlib.util.spec_from_file_location("_bergamot", folder / MODULE)
    bergamot = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bergamot)
    engine = bergamot.Service(bergamot.ServiceConfig(numWorkers=1))
    config = {
        "models": [str(folder / MODEL)],
        "vocabs": [str(folder / SOURCE_VOCAB), str(folder / TARGET_VOCAB)],
        "shortlist": [str(folder / SHORTLIST), False],
        "gemm-precision": "int8shiftAlphaAll",
        "workspace": 128,
        "beam-size": 1,
        "mini-batch-words": 1024,
        "max-length-break": 128,
        "quiet": True,
        "quiet-translation": True,
    }
    model = engine.modelFromConfig(json.dumps(config))
    load_ms = (perf_counter() - started) * 1000
    options = bergamot.ResponseOptions(qualityScores=False, sentenceMappings=False)
    outputs = []
    for case in cases:
        source = bergamot.VectorString([case["source"]])
        started = perf_counter()
        (response,) = engine.translate(model, source, options)
        elapsed = (perf_counter() - started) * 1000
        outputs.append({
            **case,
            "output": response.target.text,
            "latency_ms": round(elapsed, 2),
            "has_chinese": bool(re.search(r"[\u4e00-\u9fff]", response.target.text)),
        })

    result = {
        "build_commit": os.environ["GITHUB_SHA"],
        "actions_run_id": os.environ["GITHUB_RUN_ID"],
        "platform": platform.platform(),
        "cpu": next(
            line.split(":", 1)[1].strip()
            for line in Path("/proc/cpuinfo").read_text().splitlines()
            if line.startswith("model name")
        ),
        "python": platform.python_version(),
        "network_interfaces": interfaces,
        "runtime": "Bergamot v0.4.5",
        "runtime_sha256": WHEEL_SHA256,
        "model": f"Mozilla en-zh-Hans v{version}",
        "model_bytes": sum(size for _, size, _ in files.values()),
        "model_sha256": {name: meta[2] for name, meta in files.items()},
        "corpus_sha256": hashlib.sha256(samples).hexdigest(),
        "workers": 1,
        "load_ms": round(load_ms, 2),
        "warm_median_ms": round(statistics.median(row["latency_ms"] for row in outputs[1:]), 2),
        "peak_rss_kib": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
        "sentences": outputs,
    }
    report.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    assert all(row["has_chinese"] for row in outputs), "A sample produced no Chinese text"
    print(f"Translated {len(outputs)} synthetic utterances offline.")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("folder", type=Path)
    parser.add_argument("version", choices=WEIGHTS)
    actions = parser.add_subparsers(dest="action", required=True)
    actions.add_parser("fetch")
    run = actions.add_parser("run")
    run.add_argument("--corpus", type=Path, required=True)
    run.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    folder = args.folder.resolve()
    files = {MODEL: WEIGHTS[args.version], **SHARED_FILES}
    if args.action == "fetch":
        fetch(folder, files)
    else:
        evaluate(folder, args.version, files, args.corpus, args.report)


if __name__ == "__main__":
    main()
