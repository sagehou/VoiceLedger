"""Exercise the adapter against the exact downloadable English-to-Chinese model."""

import argparse
import ctypes
import json
from pathlib import Path
import re
import socket

from build_speech import REPO, fetch


def prepare(root):
    model = next(model for model in json.loads((REPO / "models/catalog-v1.json").read_text())["models"]
                 if model["id"] == "opus-mt-en-zh-int8")
    root.mkdir(parents=True, exist_ok=True)
    for entry in model["artifacts"]:
        file = fetch(root, entry["file"], entry["url"], entry["sha256"])
        assert file.stat().st_size == entry["size_bytes"]


def smoke(root, library, report, offline):
    if offline:
        assert [name for _, name in socket.if_nameindex()] == ["lo"]
    adapter = ctypes.CDLL(str(library.resolve()))
    adapter.vl_translation_version_v1.restype = ctypes.c_char_p
    assert adapter.vl_translation_version_v1() == b"4.7.1"
    adapter.vl_translation_open_v1.argtypes = [ctypes.c_char_p]
    adapter.vl_translation_open_v1.restype = ctypes.c_void_p
    adapter.vl_translation_run_v1.argtypes = [
        ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint32, ctypes.c_void_p, ctypes.c_uint32,
    ]
    adapter.vl_translation_run_v1.restype = ctypes.c_int32
    adapter.vl_translation_close_v1.argtypes = [ctypes.c_void_p]
    adapter.vl_translation_close_v1.restype = None
    assert not adapter.vl_translation_open_v1(str(root / "missing").encode())
    handle = adapter.vl_translation_open_v1(str(root.resolve()).encode())
    assert handle, "Pinned translation model failed to load"
    try:
        cases = json.loads((REPO / "contracts/fixtures/translation-en-zh-v1.json").read_text(encoding="utf-8"))
        for case in cases:
            source = case["source"].encode()
            output = ctypes.create_string_buffer(8192)
            size = adapter.vl_translation_run_v1(handle, source, len(source), output, len(output))
            assert 0 < size <= len(output), case["scenario"]
            case["output"] = output.raw[:size].decode()
            assert re.search(r"[\u4e00-\u9fff]", case["output"]), case["scenario"]
        report.write_text(json.dumps(cases, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    finally:
        adapter.vl_translation_close_v1(handle)
    print(f"Verified all {len(cases)} synthetic translations with the pinned engine and model.")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path)
    parser.add_argument("--library", type=Path)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    if args.library:
        smoke(args.root, args.library, args.report, args.offline)
    else:
        prepare(args.root)


if __name__ == "__main__":
    main()
