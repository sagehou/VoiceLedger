"""Evaluate CAM++ and the shared clustering policy on synthetic speech in Actions."""

import argparse
from array import array
import ctypes
import hashlib
import io
from itertools import combinations
import json
import math
from pathlib import Path
import random
import resource
import socket
import subprocess
from statistics import median
from time import perf_counter
import wave

from build_speech import fetch


MODEL = "3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx"
INT8_MODEL = MODEL.removesuffix(".onnx") + ".weight-int8.onnx"
URL = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/" + MODEL
DIGEST = "53cab20ba8f2eb2f6e2d89ecc18e88ef3222169b61b4419cbc13666744a4574b"
VOICES = (("en-us+m1", 26), ("en-gb+f2", 65), ("en-sc+m3", 42),
          ("en-us+f4", 78), ("en-gb+m5", 51), ("en-us+f1", 34))
SENTENCES = (
    "The second recording will start as soon as everyone joins.",
    "Put the microphone on the table and switch to the other track.",
    "I saved the original file in the folder for this meeting.",
    "Could you read the names aloud and ask people to confirm?",
    "The battery was low during the first test but the recording recovered.",
)
RATE = 16000


def quantize_weights(source, target):
    import numpy as np
    import onnx
    from onnx import TensorProto, helper, numpy_helper

    model = onnx.load(source)
    assert any(item.domain in ("", "ai.onnx") and item.version >= 10 for item in model.opset_import)
    graph = model.graph
    weight_names = {node.input[1] for node in graph.node
                    if node.domain in ("", "ai.onnx") and node.op_type in ("Conv", "Gemm", "MatMul")
                    and len(node.input) >= 2}
    graph_inputs = {item.name for item in graph.input}
    updated, nodes = [], []
    for tensor in graph.initializer:
        if tensor.name not in weight_names or tensor.name in graph_inputs or tensor.data_type != TensorProto.FLOAT:
            updated.append(tensor)
            continue
        values = numpy_helper.to_array(tensor)
        assert np.all(np.isfinite(values))
        scale = np.float32(max(np.max(np.abs(values)) / 127, np.finfo(np.float32).tiny))
        packed = np.rint(values / scale).clip(-127, 127).astype(np.int8)
        name = tensor.name + ".voiceledger_int8"
        updated.extend([
            numpy_helper.from_array(packed, name),
            numpy_helper.from_array(np.array(scale), name + ".scale"),
            numpy_helper.from_array(np.array(0, dtype=np.int8), name + ".zero"),
        ])
        nodes.append(helper.make_node("DequantizeLinear", [name, name + ".scale", name + ".zero"], [tensor.name]))

    assert nodes, "The CAM++ graph contains no eligible FP32 weights"
    original = list(graph.node)
    graph.ClearField("node")
    graph.node.extend(nodes + original)
    graph.ClearField("initializer")
    graph.initializer.extend(updated)
    onnx.checker.check_model(model)
    onnx.save(model, target)
    assert target.stat().st_size < source.stat().st_size


def command(argv, *, data=None):
    return subprocess.run(argv, input=data, check=True, capture_output=True).stdout


def samples(voice, pitch, text, speed):
    audio = command(["espeak-ng", "-v", voice, "-p", str(pitch), "-s", str(speed), "--stdout", text])
    with wave.open(io.BytesIO(audio)) as stream:
        rate = stream.getframerate()
        assert stream.getnchannels() == 1 and stream.getsampwidth() == 2
        pcm = array("h", stream.readframes(12 * rate))
    output = array("h")
    for index in range(len(pcm) * RATE // rate):
        left, fraction = divmod(index * rate, RATE)
        right = min(left + 1, len(pcm) - 1)
        output.append((pcm[left] * (RATE - fraction) + pcm[right] * fraction) // RATE)
    return output


def overlap(a, b):
    return array("h", (int((x + y) / 2) for x, y in zip(a, b)))


def noisy(audio):
    rng = random.Random(20261125)
    return array("h", (max(-32768, min(32767, sample + rng.randrange(-4096, 4096))) for sample in audio))


def cosine(a, b):
    dot = math.fsum(x * y for x, y in zip(a, b, strict=True))
    norm = math.sqrt(math.fsum(x * x for x in a) * math.fsum(y * y for y in b))
    return dot / norm


def native(library):
    dll = ctypes.CDLL(str(library.resolve()))
    dll.vl_speaker_version_v1.restype = ctypes.c_char_p
    dll.vl_speaker_open_v1.argtypes = [ctypes.c_char_p]
    dll.vl_speaker_open_v1.restype = ctypes.c_void_p
    dll.vl_speaker_extract_v1.argtypes = [
        ctypes.c_void_p, ctypes.POINTER(ctypes.c_float), ctypes.c_int32,
        ctypes.POINTER(ctypes.c_float), ctypes.c_int32,
    ]
    dll.vl_speaker_extract_v1.restype = ctypes.c_int32
    dll.vl_speaker_close_v1.argtypes = [ctypes.c_void_p]
    dll.vl_speaker_close_v1.restype = None
    assert dll.vl_speaker_version_v1() == b"1.12.20"
    return dll


def fit(extract, audio):
    embeddings = [(speaker, extract(sample)) for speaker, turns in enumerate(audio) for sample in turns[:2]]
    assert all(vector for _, vector in embeddings)
    good, wrong = [], []
    for (left, a), (right, b) in combinations(embeddings, 2):
        (good if left == right else wrong).append(cosine(a, b))
    floor = min(good)
    impostor = max(wrong)
    threshold = (floor + impostor) / 2 if floor > impostor else min(1.0, impostor + 0.0001)
    return threshold, {"lowest_same_speaker": floor, "highest_different_speaker": impostor,
                       "same_speaker_pairs_accepted": sum(score >= threshold for score in good),
                       "same_speaker_pairs": len(good), "different_speaker_pairs": len(wrong)}


def scores(references, labels, durations, count):
    by_cluster = {label for label in labels if label is not None}
    intersections = {label: [0] * count for label in by_cluster}
    predicted = dict.fromkeys(by_cluster, 0)
    expected = [0] * count
    total = 0
    for speakers, cluster, frames in zip(references, labels, durations, strict=True):
        total += len(speakers) * frames
        for speaker in speakers:
            expected[speaker] += frames
        if cluster is not None:
            predicted[cluster] += frames
            for speaker in speakers:
                intersections[cluster][speaker] += frames

    # At most six reference identities, so a bitmask can find the best one-to-one mapping.
    mappings = {0: (0, {})}
    for cluster, rows in intersections.items():
        candidates = dict(mappings)
        for used, (matched, assigned) in mappings.items():
            for speaker, frames in enumerate(rows):
                bit = 1 << speaker
                if used & bit or frames == 0:
                    continue
                if matched + frames > candidates.get(used | bit, (-1, {}))[0]:
                    candidates[used | bit] = (matched + frames, {**assigned, cluster: speaker})
        mappings = candidates
    matched, mapping = max(mappings.values(), key=lambda item: item[0])
    union_scores = [0.0] * count
    for cluster, speaker in mapping.items():
        overlap_frames = intersections[cluster][speaker]
        union_scores[speaker] = overlap_frames / (expected[speaker] + predicted[cluster] - overlap_frames)

    # Count splits and merges on the single-speaker turns only.
    pairs = [(speakers[0], label) for speakers, label in zip(references, labels, strict=True)
             if len(speakers) == 1 and label is not None]
    return {
        "window_der": round((total - matched) / total, 4),
        "window_jer": round(1 - math.fsum(union_scores) / count, 4),
        "false_merge_clusters": sum(len({s for s, x in pairs if x == c}) > 1 for c in by_cluster),
        "false_split_speakers": sum(len({c for s, c in pairs if s == voice}) > 1 for voice in range(count)),
        "unknown_turns": labels.count(None),
        "overlap_unknown": labels[-1] is None,
    }


def evaluate(model, candidate, library, clustering, report):
    assert [name for _, name in socket.if_nameindex()] == ["lo"]
    dll = native(library)
    started = perf_counter()
    handle = dll.vl_speaker_open_v1(str(model.resolve()).encode())
    assert handle, "The verified CAM++ candidate could not be loaded"
    load_seconds = perf_counter() - started
    extract_times = []

    def extract(audio):
        raw = (ctypes.c_float * len(audio))(*(value / 32768 for value in audio))
        out = (ctypes.c_float * 1024)()
        started = perf_counter()
        dim = dll.vl_speaker_extract_v1(handle, raw, len(raw), out, len(out))
        if dim > 0:
            extract_times.append((perf_counter() - started) * 1000)
        assert 0 <= dim <= len(out)
        return out[:dim] if dim else None

    try:
        audio = [[samples(voice, pitch, sentence, 151 + 3 * index)
                  for index, sentence in enumerate(SENTENCES)] for voice, pitch in VOICES]
        voices = [hashlib.sha256(turns[0].tobytes()).hexdigest() for turns in audio]
        assert len(set(voices)) == len(VOICES), "Synthetic voices must produce distinct reference audio"
        assert extract(array("h", [0]) * RATE) is None
        assert extract(audio[0][2][:4000]) is None
        threshold, calibration = fit(extract, audio)
        assert 0 < threshold <= 1
        scenarios = []
        for count in range(2, 7):
            turns = [(speaker, audio[speaker][index]) for index in (2, 3, 4) for speaker in range(count)]
            turns.append((0, noisy(audio[0][2])))
            turns.append((1, audio[1][3][:4000]))
            references = [(speaker,) for speaker, _ in turns] + [(0, 1)]
            payloads = [pcm for _, pcm in turns] + [overlap(audio[0][4], audio[1][4])]
            request = json.dumps({"threshold": threshold, "embeddings": [extract(pcm) for pcm in payloads]})
            labels = json.loads(command([str(clustering.resolve())], data=request.encode()))
            assert labels[-2] is None and len(labels) == len(payloads)
            scenarios.append({"speakers": count, **scores(references, labels, list(map(len, payloads)), count)})
    finally:
        dll.vl_speaker_close_v1(handle)
    with model.open("rb") as weights:
        digest = hashlib.file_digest(weights, "sha256").hexdigest()
    with report.open("w", encoding="utf-8") as output:
        json.dump({"model": model.name, "candidate": candidate,
                   "sha256": digest, "source_sha256": DIGEST, "bytes": model.stat().st_size,
                   "runtime": "sherpa-onnx 1.12.20", "load_ms": round(load_seconds * 1000, 2),
                   "peak_rss_bytes": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * 1024,
                   "extract_median_ms": round(median(extract_times), 2),
                   "synthetic_voice_reference_sha256": {
                       f"{voice}:{pitch}": digest for (voice, pitch), digest in zip(VOICES, voices, strict=True)
                   },
                   "corpus_sha256": hashlib.sha256(b"".join(pcm.tobytes() for turns in audio for pcm in turns)).hexdigest(),
                   "synthesizer": command(["espeak-ng", "--version"]).decode().splitlines()[0],
                   "cosine_threshold": threshold, "calibration": calibration,
                   "synthetic_scenarios": scenarios}, output, indent=2)
    print("CAM++ embeddings and shared clustering evaluated on synthetic 2–6 voice scenarios.")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("folder", type=Path)
    parser.add_argument("--candidate", choices=("fp32", "int8"), default="fp32")
    parser.add_argument("--library", type=Path)
    parser.add_argument("--clustering", type=Path)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    if args.library:
        name = MODEL if args.candidate == "fp32" else INT8_MODEL
        evaluate(args.folder / name, args.candidate, args.library, args.clustering, args.report)
    else:
        args.folder.mkdir(parents=True, exist_ok=True)
        source = fetch(args.folder, MODEL, URL, DIGEST)
        quantize_weights(source, args.folder / INT8_MODEL)


if __name__ == "__main__":
    main()
