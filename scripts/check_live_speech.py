"""Check the live speech ABI with synthetic audio and the packaged, offline runtimes."""
import argparse
from array import array
import ctypes as c
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import json
from pathlib import Path
import shutil
import sqlite3
from tempfile import TemporaryDirectory
from time import monotonic, sleep
from uuid import uuid4

from check_asr_languages import ROOT, waveform

RATE = 48000


class Buffer(c.Structure):
    _fields_ = [("data", c.c_void_p), ("length", c.c_uint32)]


def bind(library, name, result, *args):
    call = getattr(library, f"vl_{name}_v1")
    call.restype = result
    call.argtypes = args
    return call


class Core:
    def __init__(self, binary, root, adapters):
        lib = c.CDLL(str(binary.resolve()))
        self.root = root
        self.adapters = adapters.resolve()
        self.speech_call = bind(lib, "speech_request", c.c_int32, c.c_char_p, c.c_uint32, c.POINTER(Buffer))
        self.resources = bind(lib, "speech_resources", c.c_int32, c.c_char_p, c.c_uint32)
        self.cancel = bind(lib, "speech_cancel", None)
        self.release = bind(lib, "audio_buffer_release", None, c.c_void_p, c.c_uint32)
        self.audio_call = bind(lib, "audio_request", c.c_int32, c.c_int64, c.c_char_p, c.c_uint32,
                               c.POINTER(c.c_float), c.c_uint32, c.POINTER(Buffer))
        self.close = bind(lib, "audio_release", c.c_int32, c.c_int64)
        path = str(root).encode()
        open_audio = bind(lib, "audio_open", c.c_int64, c.c_char_p, c.c_uint32)
        self.handle = open_audio(path, len(path))
        assert self.handle > 0, self.handle
        profile = json.dumps(dict(
            id=str(uuid4()), device_id=str(uuid4()), observed_at_unix_ms=1, platform="windows",
            os_version="10.0.26100", logical_cpu_count=2, low_memory=False, process_memory_bytes=128_000_000,
            available_memory_bytes=4_000_000_000, storage_available_bytes=2_000_000_000,
        )).encode()
        save_profile = bind(lib, "device_profile_save", c.c_int32, c.c_char_p, c.c_uint32,
                            c.c_char_p, c.c_uint32)
        assert save_profile(path, len(path), profile, len(profile)) == 0

    def speech(self, op, session, expected=0, **fields):
        data = json.dumps(dict(
            recordings=str(self.root), profiles=str(self.root), adapters=str(self.adapters),
            action=dict(op=op, session_id=session, **fields),
        )).encode()
        output = Buffer()
        code = self.speech_call(data, len(data), c.byref(output))
        try:
            assert code == expected, (op, code, expected)
            return json.loads(c.string_at(output.data, output.length)) if output.length else None
        finally:
            self.release(output.data, output.length)

    def audio(self, op, pcm=None, **fields):
        data = json.dumps(dict(op=op, **fields)).encode()
        samples = (c.c_float * len(pcm)).from_buffer(pcm) if pcm is not None else None
        output = Buffer()
        try:
            code = self.audio_call(self.handle, data, len(data), samples, len(pcm) if pcm is not None else 0,
                                   c.byref(output))
            assert code == 0, (op, code)
        finally:
            self.release(output.data, output.length)

    def state(self, session, state, at=0):
        self.audio("session_state", id=session, state=state, elapsed_ns=at, snapshot={})

    def session(self, *sources):
        session = str(uuid4())
        tracks = {source: str(uuid4()) for source in sources}
        self.audio("create_session", id=session, created_at_ms=1, clock="synthetic", clock_origin=0,
                   tracks=[dict(id=track, source=source, endpoint_id="synthetic")
                           for source, track in tracks.items()])
        for track in tracks.values():
            self.audio("begin_stream", track_id=track, epoch=0, start_ns=0, rate=RATE, channels=1,
                       sample_format="pcm_f32le", native_format="synthetic")
        self.state(session, 2)
        return session, tracks

    def begin_clip(self, track, pcm, start):
        position = start
        chunks = []
        for offset in range(0, len(pcm), RATE * 2):
            segment = pcm[offset:offset + RATE * 2]
            stop = position + len(segment) * 1_000_000_000 // RATE
            chunks.append((track, str(uuid4()), position, stop, segment))
            position = stop
        return chunks, position

    def commit(self, chunk, staged=False):
        track, identifier, start, end, pcm = chunk
        if not staged:
            self.audio("begin_chunk", id=identifier, track_id=track, epoch=0, start_ns=start, end_ns=end)
        self.audio("seal_chunk", pcm, id=identifier, end_ns=end, packets=[
            dict(start_ns=start, end_ns=end, frames=len(pcm), device_position_frames=0, qpc100ns=0,
                 flags=0, estimated_timestamp=False),
        ])

    def pressure(self, low_memory):
        data = json.dumps(dict(low_memory=low_memory)).encode()
        assert self.resources(data, len(data)) == 0



def wait_for(check, worker, description):
    deadline = monotonic() + 45
    while monotonic() < deadline:
        if worker.done():
            raise AssertionError((description, worker.result()))
        value = check()
        if value:
            return value
        sleep(0.2)
    raise AssertionError(description)


def clip(folder, lang):
    return array("f", (value for frame in waveform(folder / f"{lang}-1.wav") for value in (frame,) * 3))


def verify(core, samples, executor):
    session, tracks = core.session("mic", "system_loopback")
    mic = tracks["mic"]
    system = tracks["system_loopback"]
    snapshot = lambda: core.speech("live_transcript", session)
    worker = executor.submit(core.speech, "transcribe_live", session, language="auto", translate=True, approved=True)
    english, end_en = core.begin_clip(mic, clip(samples, "en"), 0)
    track, identifier, start, end, _ = english[0]
    core.audio("begin_chunk", id=identifier, track_id=track, epoch=0, start_ns=start, end_ns=end)
    assert not snapshot()["lines"]
    for index, chunk in enumerate(english):
        core.commit(chunk, staged=index == 0)
        sleep(0.3)

    def english_translation():
        return next((line for line in snapshot()["lines"] if line["source"] == "mic" and
                     line["language"] == "en" and line["translation"]), None)

    english_line = wait_for(english_translation, worker, "No finalized English translation during capture")
    japanese, end_ja = core.begin_clip(system, clip(samples, "ja"), 0)
    for chunk in japanese:
        core.commit(chunk)
    japanese_line = wait_for(
        lambda: next((line for line in snapshot()["lines"] if line["source"] == "system_loopback" and
                      line["language"] == "ja"), None),
        worker, "The current loopback track did not produce a Japanese final",
    )

    chinese, end_zh = core.begin_clip(mic, clip(samples, "zh"), end_en + 1_000_000_000)
    core.state(session, 4, max(end_en, end_ja))
    for chunk in chinese:
        core.commit(chunk)
    core.state(session, 5, max(end_zh, end_ja))
    result = worker.result(timeout=45)
    chinese_line = next(line for line in snapshot()["lines"] if line["source"] == "mic" and line["language"] == "zh")
    assert not japanese_line["translation"] and not chinese_line["translation"]
    assert result["saved"] >= 3
    replay = core.speech("transcribe", session, model_id="whisper-tiny-multilingual-q5-1",
                         language="auto", approved=True)
    assert replay["saved"] == 0

    pressured, tracks = core.session("mic")
    worker = executor.submit(core.speech, "transcribe_live", pressured, expected=-38,
                             language="auto", translate=False, approved=True)
    pcm = clip(samples, "en")
    initial, through = core.begin_clip(tracks["mic"], pcm, 0)
    for chunk in initial:
        core.commit(chunk)
    before_pressure = wait_for(lambda: core.speech("live_transcript", pressured)["lines"], worker,
                               "No final from the recording used for the low-memory check")
    core.pressure(True)
    worker.result(timeout=15)
    remaining, end = core.begin_clip(tracks["mic"], pcm, through + 1_000_000_000)
    for chunk in remaining:
        core.commit(chunk)
    core.state(pressured, 5, end)
    core.pressure(False)
    resumed = core.speech("transcribe", pressured, model_id="whisper-tiny-multilingual-q5-1",
                          language="auto", approved=True)
    assert resumed["saved"] > 0

    with closing(sqlite3.connect(core.root / "transcript-v1.sqlite3")) as db:
        sources = db.execute("SELECT json_extract(r.payload,'$.kind'),json_extract(r.payload,'$.language') "
                             "FROM translation_revisions t JOIN revisions r ON r.id=t.source_revision_id").fetchall()
        revisions = db.execute("SELECT selected_revision FROM utterances WHERE session_id=?", (pressured,)).fetchall()
        translated_when_disabled = db.execute("SELECT COUNT(*) FROM translation_revisions t "
                                               "JOIN utterances u ON u.id=t.utterance_id WHERE u.session_id=?",
                                               (pressured,)).fetchone()[0]
    assert sources and all(source == ("final", "en") for source in sources)
    assert not translated_when_disabled
    assert {line["revision_id"] for line in before_pressure} <= {row[0] for row in revisions}
    return {
        "during_capture": [english_line, japanese_line],
        "after_capture": chinese_line,
        "translated_sources": len(sources),
        "repeat_saved": replay["saved"],
        "resumed_after_memory_pressure": resumed["saved"],
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("samples", type=Path)
    parser.add_argument("translation", type=Path)
    parser.add_argument("ffi", type=Path)
    parser.add_argument("adapters", type=Path)
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    catalog = json.loads((ROOT / "models/catalog-v1.json").read_text(encoding="utf-8"))
    inputs = {"whisper-tiny-multilingual-q5-1": args.samples, "opus-mt-en-zh-int8": args.translation}
    with TemporaryDirectory() as work, ThreadPoolExecutor(max_workers=1) as executor:
        root = Path(work)
        core = Core(args.ffi, root, args.adapters)
        for model in catalog["models"]:
            if model["id"] in inputs:
                directory = root / "models" / f"{model['id']}-{model['version']}"
                directory.mkdir(parents=True)
                for artifact in model["artifacts"]:
                    shutil.copyfile(inputs[model["id"]] / artifact["file"], directory / artifact["file"])
        try:
            report = verify(core, args.samples, executor)
        finally:
            core.pressure(True)
            core.cancel()
            executor.shutdown(wait=True)
            assert core.close(core.handle) == 0
        args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        with args.report.with_name("live-speech-english.f32").open("wb") as fixture:
            clip(args.samples, "en").tofile(fixture)


if __name__ == "__main__":
    main()
