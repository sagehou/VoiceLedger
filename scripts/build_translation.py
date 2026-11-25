"""Build the pinned native translation adapter for the client packages."""

import argparse
import os
from pathlib import Path

from build_speech import REPO, fetch, run, unpack


CTRANSLATE2 = "226c95d94e660c48b11c62e108886b7ef76d589d"
SENTENCEPIECE = "31646a467d2051eb904e0b45de3a73e91fe1c1e3"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--android-ndk", type=Path)
    args = parser.parse_args()
    work = args.work.resolve()
    output = args.output.resolve()
    work.mkdir(parents=True, exist_ok=True)
    output.mkdir(parents=True, exist_ok=True)
    ct2 = work / "ctranslate2"
    run("git", "init", "--quiet", str(ct2))
    run("git", "-C", str(ct2), "fetch", "--quiet", "--depth", "1",
        "https://github.com/OpenNMT/CTranslate2.git", CTRANSLATE2)
    run("git", "-C", str(ct2), "checkout", "--quiet", "--detach", CTRANSLATE2)
    run("git", "-C", str(ct2), "submodule", "update", "--init", "--recursive", "--depth", "1",
        "third_party/cpu_features", "third_party/ruy", "third_party/spdlog")
    archive = fetch(work, "sentencepiece.tar.gz",
        f"https://codeload.github.com/google/sentencepiece/tar.gz/{SENTENCEPIECE}",
        "b4eb17e6bea5c9380ddd79d04d01d6aa5e280be8f3e3cef1f745a80e6d9451ce")
    unpack(archive, work, source=True)
    spm = work / f"sentencepiece-{SENTENCEPIECE}"
    options = ["-DCMAKE_BUILD_TYPE=Release", "-DCMAKE_POLICY_VERSION_MINIMUM=3.5"]
    if args.android_ndk:
        run("git", "-C", str(ct2), "apply", str(REPO / "native/translation/ctranslate2-android-affinity.patch"))
        options.extend([
            "-G", "Ninja", f"-DCMAKE_TOOLCHAIN_FILE={args.android_ndk / 'build/cmake/android.toolchain.cmake'}",
            "-DANDROID_PLATFORM=android-26", "-DANDROID_ABI=arm64-v8a", "-DANDROID_STL=c++_static",
        ])
    elif os.name == "nt":
        options.extend(["-A", "x64"])
    build = work / "build"
    run("cmake", "-S", str(REPO / "native/translation"), "-B", str(build), *options,
        f"-DCTRANSLATE2_SOURCE={ct2}", f"-DSENTENCEPIECE_SOURCE={spm}")
    run("cmake", "--build", str(build), "--config", "Release", "--parallel", "3")
    run("cmake", "--install", str(build), "--config", "Release", "--prefix", str(output))

    notices = []
    for label, source in (("CTranslate2", ct2), ("SentencePiece", spm)):
        for item in sorted(source.rglob("LICENSE*")):
            if item.is_file() and ".git" not in item.parts:
                notices.append(f"{label} / {item.relative_to(source).as_posix()}\n\n{item.read_text(encoding='utf-8')}")
    (output / "TRANSLATION-LICENSES.txt").write_text("\n\n".join(notices), encoding="utf-8")


if __name__ == "__main__":
    main()
