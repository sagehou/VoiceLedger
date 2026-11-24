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
import shutil
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
OPUS_URL = "https://object.pouta.csc.fi/Tatoeba-MT-models/eng-zho/opus-2020-07-17.zip"
OPUS_SIZE = 288_947_487
OPUS_HASH = "ab90e5583fa3ec46d8c5c50f106cbd65221978cd052c7e2059981439325ae24b"
OPUS_WEIGHTS = "opus.spm32k-spm32k.transformer.model1.npz.best-perplexity.npz"
OPUS_VOCAB = "opus.spm32k-spm32k.vocab.yml"
OPUS_TOKENIZERS = ("source.spm", "target.spm")


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def download(url, path, size, digest):
    subprocess.run([
        "curl", "--fail", "--silent", "--show-error", "--location", "--retry", "3",
        "--max-time", "180", "--max-filesize", str(size), "--output", str(path), url,
    ], check=True)
    if path.stat().st_size != size or sha256_file(path) != digest:
        raise ValueError(f"Size or SHA-256 mismatch for {path.name}")


def fetch_bergamot(folder, files):
    wheel = folder / WHEEL
    download(WHEEL_URL + WHEEL, wheel, 11_007_126, WHEEL_SHA256)
    with ZipFile(wheel) as archive:
        (folder / MODULE).write_bytes(archive.read(f"bergamot/{MODULE}"))
    for name, (location, size, digest) in files.items():
        download(ATTACHMENTS + location, folder / name, size, digest)


def fetch_opus(folder):
    from ctranslate2.converters import MarianConverter

    bundle = folder / "opus-2020-07-17.zip"
    download(OPUS_URL, bundle, OPUS_SIZE, OPUS_HASH)
    with ZipFile(bundle) as archive:
        for name in (OPUS_WEIGHTS, OPUS_VOCAB, *OPUS_TOKENIZERS):
            with archive.open(name) as source, (folder / name).open("wb") as output:
                shutil.copyfileobj(source, output)
    vocabs = [str(folder / OPUS_VOCAB)] * 2
    converter = MarianConverter(str(folder / OPUS_WEIGHTS), vocabs)
    converter.convert(str(folder / "opus-int8"), quantization="int8")


def bergamot_translator(folder):
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
    options = bergamot.ResponseOptions(qualityScores=False, sentenceMappings=False)

    def translate(source):
        (response,) = engine.translate(model, bergamot.VectorString([source]), options)
        return response.target.text

    return translate


def opus_translator(folder):
    import ctranslate2
    from sentencepiece import SentencePieceProcessor

    source_tokenizer, target_tokenizer = (
        SentencePieceProcessor(model_file=str(folder / name)) for name in OPUS_TOKENIZERS
    )
    engine = ctranslate2.Translator(
        str(folder / "opus-int8"), device="cpu", compute_type="int8", inter_threads=1, intra_threads=1,
    )

    def translate(source):
        tokens = [">>cmn_Hans<<", *source_tokenizer.encode(source, out_type=str)]
        (result,) = engine.translate_batch([tokens], beam_size=6, length_penalty=1)
        return target_tokenizer.decode(result.hypotheses[0])

    return translate


def evaluate(folder, candidate, corpus, report):
    interfaces = [name for _, name in socket.if_nameindex()]
    if interfaces != ["lo"]:
        raise RuntimeError(f"Expected an isolated network namespace, got {interfaces}")
    samples = corpus.read_bytes()
    cases = json.loads(samples)
    assert 10 <= len(cases) <= 20

    if candidate == "opus-mt":
        files = sorted((folder / "opus-int8").iterdir()) + [folder / name for name in OPUS_TOKENIZERS]
        provenance = {
            "runtime": "CTranslate2 4.7.1 CPU int8",
            "model": "Helsinki-NLP OPUS-MT eng-zho 2020-07-17",
            "model_archive_sha256": OPUS_HASH,
            "model_archive_bytes": OPUS_SIZE,
            "model_bytes": sum(file.stat().st_size for file in files),
            "model_sha256": {file.relative_to(folder).as_posix(): sha256_file(file) for file in files},
            "beam_size": 6,
        }
        translator = opus_translator
    else:
        files = {MODEL: WEIGHTS[candidate], **SHARED_FILES}
        provenance = {
            "runtime": "Bergamot v0.4.5",
            "runtime_sha256": WHEEL_SHA256,
            "model": f"Mozilla en-zh-Hans v{candidate}",
            "model_bytes": sum(size for _, size, _ in files.values()),
            "model_sha256": {name: meta[2] for name, meta in files.items()},
            "beam_size": 1,
        }
        translator = bergamot_translator

    started = perf_counter()
    translate = translator(folder)
    load_ms = (perf_counter() - started) * 1000
    outputs = []
    for case in cases:
        started = perf_counter()
        output = translate(case["source"])
        elapsed = (perf_counter() - started) * 1000
        outputs.append({
            **case,
            "output": output,
            "latency_ms": round(elapsed, 2),
            "has_chinese": bool(re.search(r"[\u4e00-\u9fff]", output)),
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
        **provenance,
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
    parser.add_argument("candidate", choices=(*WEIGHTS, "opus-mt"))
    actions = parser.add_subparsers(dest="action", required=True)
    actions.add_parser("fetch")
    run = actions.add_parser("run")
    run.add_argument("--corpus", type=Path, required=True)
    run.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    folder = args.folder.resolve()
    if args.action == "fetch":
        folder.mkdir(parents=True, exist_ok=True)
        if args.candidate == "opus-mt":
            fetch_opus(folder)
        else:
            fetch_bergamot(folder, {MODEL: WEIGHTS[args.candidate], **SHARED_FILES})
    else:
        evaluate(folder, args.candidate, args.corpus, args.report)


if __name__ == "__main__":
    main()
