"""Score Android model runs of the same paced synthetic recording."""
from bisect import bisect_right
from collections import Counter
from hashlib import sha256
import json
import os
from pathlib import Path
import sys

from check_asr_languages import ROOT, edits, units

MODELS = ("sensevoice-small-gguf-q8-0", "sensevoice-small-gguf-q6-k", "nemotron-3-5-asr-streaming-0-6b-q4-k-m")
SPEECH_WORKER = "com.voiceledger.app:speech"


def peaks(path):
    rss = {}
    workers = set()
    for row in path.read_text().splitlines():
        name, pid, peak = row.split("\t")
        rss[name] = max(rss.get(name, 0), int(peak) * 1024)
        if name == SPEECH_WORKER:
            workers.add(pid)
    if len(workers) != 1 or SPEECH_WORKER not in rss or "com.voiceledger.app" not in rss:
        raise RuntimeError(f"Missing or restarted Android speech worker in {path.name}")
    return rss


def compare(fixtures, raw, output):
    data = (fixtures / "live-speech-corpus.json").read_bytes()
    corpus = json.loads(data)
    for clip in corpus:
        if sha256((fixtures / clip["file"]).read_bytes()).hexdigest() != clip["sha256"]:
            raise RuntimeError(f"Fixture digest mismatch for {clip['file']}")
    catalog = json.loads((ROOT / "models/catalog-v1.json").read_bytes())
    models = {model["id"]: model for model in catalog["models"]}
    reports = []
    reference_timeline = None
    failures = []
    for identifier in MODELS:
        result = json.loads((raw / f"{identifier}.json").read_bytes())
        assert result["model"] == identifier
        timeline = result.pop("clips")
        assert [clip["file"] for clip in timeline] == [clip["file"] for clip in corpus]
        capture = (timeline, result["audio_ms"], result["committed_segments"])
        if reference_timeline is None:
            reference_timeline = capture
        assert capture == reference_timeline, "Models did not receive identical committed audio timelines."
        # A speech window can start up to 160 ms before the next clip due to VAD pre-roll.
        starts = [clip["start_ns"] - 160_000_000 for clip in timeline]
        recognized = [[] for _ in corpus]
        for line in result.pop("lines"):
            index = bisect_right(starts, line["start_ns"]) - 1
            assert index >= 0, "Transcript line preceded the corpus."
            recognized[index].append(line)
        samples = []
        for clip, lines in zip(corpus, recognized):
            lang = clip["language"]
            counts = Counter()
            for line in lines:
                counts[line["language"]] += len(units(line["text"], lang))
            detected = counts.most_common(1)[0][0] if counts else None
            expected = units(clip["reference"], lang)
            actual = units(" ".join(line["text"] for line in lines), lang)
            samples.append({
                "fixture": clip["file"], "expected": lang, "detected": detected,
                "edits": edits(expected, actual), "reference_units": len(expected),
            })
        scores = {}
        for lang in ("zh", "ja", "en"):
            subset = [item for item in samples if item["expected"] == lang]
            correct = sum(item["detected"] == lang for item in subset)
            scores[lang] = {
                "metric": "WER" if lang == "en" else "CER",
                "error_rate": round(sum(item["edits"] for item in subset) / sum(item["reference_units"] for item in subset), 4),
                "language_correct": correct, "fixtures": len(subset),
            }
            if correct == 0:
                failures.append(f"{identifier} did not recognize {lang} in Auto")
        install = peaks(raw / f"{identifier}-install.tsv")
        live = peaks(raw / f"{identifier}-live.tsv")
        model = models[identifier]
        estimate = model["peak_ram_estimate"]["bytes"]
        if max(install[SPEECH_WORKER], live[SPEECH_WORKER]) > estimate:
            failures.append(f"{identifier} worker exceeded the catalog peak estimate")
        reports.append({
            **result,
            "runtime": model["runtime"]["version"],
            "model_sha256": model["artifacts"][0]["sha256"],
            "stream_lookahead_ms": 1040 if identifier == MODELS[-1] else None,
            "peak_rss_bytes": {"install": install, "live": live},
            "scores": scores, "samples": samples,
        })
    report = {
        "commit": os.environ.get("GITHUB_SHA"),
        "environment": "Android API 35 x86_64 emulator, ARM64 translation, 4 vCPUs, 7 GiB",
        "workload": "six paced 48 kHz synthetic clips, 2 s silence between clips, Auto, translation disabled",
        "peak_method": "VmHWM of each process from /proc; fresh app and worker for each run",
        "first_text_method": "From paced capture start to the first visible caption in the Compose accessibility tree",
        "scoring": "NFKC, case-folded, punctuation excluded; Chinese script variants scored as written",
        "corpus_sha256": sha256(data).hexdigest(),
        "models": reports, "failures": failures,
    }
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    for model in reports:
        print(model["model"], model["first_visible_text_ms"], model["peak_rss_bytes"], model["scores"])
    if failures:
        raise RuntimeError("; ".join(failures))


if __name__ == "__main__":
    compare(*map(Path, sys.argv[1:]))
