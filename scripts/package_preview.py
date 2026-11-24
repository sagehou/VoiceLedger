"""Bundle the signed and validated preview packages. Run on Actions after CI gate."""

import argparse
import hashlib
import os
from pathlib import Path
import re
import shutil
from zipfile import ZIP_DEFLATED, ZipFile


VERSION = "0.1.0-preview.1"
WINDOWS_FILES = {
    "VoiceLedger.exe", "VoiceLedger.dll", "VoiceLedger.deps.json", "VoiceLedger.runtimeconfig.json",
    "voiceledger_ffi.dll", "NAudio.Core.dll", "NAudio.Wasapi.dll",
    "speech/voiceledger_whisper.dll", "speech/voiceledger_sensevoice.dll",
    "speech/sherpa-onnx-c-api.dll", "speech/onnxruntime.dll", "speech/THIRD-PARTY.md",
}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--windows", type=Path, required=True)
    parser.add_argument("--android", type=Path, required=True)
    parser.add_argument("--notes", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    commit = os.environ["GITHUB_SHA"]
    run_id = os.environ["GITHUB_RUN_ID"]
    assert re.fullmatch(r"[0-9a-f]{40}", commit) and run_id.isdecimal()
    build = (
        f"Version {VERSION}\nCommit {commit}\n"
        f"GitHub Actions {os.environ['GITHUB_SERVER_URL']}/{os.environ['GITHUB_REPOSITORY']}/actions/runs/{run_id}\n"
    )
    actual = {path.relative_to(args.windows).as_posix() for path in args.windows.rglob("*") if path.is_file()}
    assert WINDOWS_FILES <= actual, "The Windows client package is incomplete"
    args.output.mkdir(exist_ok=True, parents=True)
    notes = args.output / "PREVIEW.md"
    notes.write_text(args.notes.read_text(encoding="utf-8") + "\n## Build\n\n```text\n" + build + "```\n", encoding="utf-8")
    windows = args.output / f"VoiceLedger-{VERSION}-windows-x64.zip"
    with ZipFile(windows, "w", compression=ZIP_DEFLATED) as archive:
        for name in sorted(actual):
            path = Path(name)
            native_file = path.parent.as_posix() == "speech" and path.suffix in (".dll", ".txt", ".md")
            if name in WINDOWS_FILES or native_file:
                archive.write(args.windows / path, name)
        archive.write(notes, notes.name)
    with ZipFile(windows) as archive:
        assert archive.testzip() is None and WINDOWS_FILES <= set(archive.namelist())
    android = args.output / f"VoiceLedger-{VERSION}-android-arm64.apk"
    shutil.copyfile(args.android, android)
    checksums = []
    for file in (windows, android, notes):
        with file.open("rb") as payload:
            checksums.append(f"{hashlib.file_digest(payload, 'sha256').hexdigest()}  {file.name}\n")
    (args.output / "SHA256SUMS.txt").write_text("".join(checksums), encoding="ascii")
    print(build, end="")


if __name__ == "__main__":
    main()
