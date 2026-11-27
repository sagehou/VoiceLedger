"""Build the pinned CPU-only SenseVoice GGUF adapter."""
import argparse
from pathlib import Path
import shutil

from build_speech import REPO, fetch, run, unpack

TRANSCRIBE = "275cd50d2c4104c204009a197ac091b9687668e5"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--android-ndk", type=Path, required=True)
    args = parser.parse_args()
    work = args.work.resolve()
    output = args.output.resolve()
    work.mkdir(parents=True, exist_ok=True)
    archive = fetch(work, "transcribe-source.tar.gz",
                    f"https://codeload.github.com/handy-computer/transcribe.cpp/tar.gz/{TRANSCRIBE}",
                    "63b604d87f87f5ceb16223240e0ae2dcd8a97b5119dfb3acf537af5bc2507556")
    unpack(archive, work)
    source = work / f"transcribe.cpp-{TRANSCRIBE}"
    build = work / "build"
    ndk = args.android_ndk.resolve()
    run("cmake", "-S", str(REPO / "native/speech/gguf"), "-B", str(build),
        f"-DTRANSCRIBE_SOURCE={source}", "-DCMAKE_BUILD_TYPE=Release",
        f"-DCMAKE_TOOLCHAIN_FILE={ndk / 'build/cmake/android.toolchain.cmake'}",
        "-DANDROID_PLATFORM=android-26", "-DANDROID_ABI=arm64-v8a", "-DANDROID_STL=c++_static",
        "-DGGML_CPU_ARM_ARCH=armv8-a")
    run("cmake", "--build", str(build), "--parallel", "3")
    run("cmake", "--install", str(build), "--prefix", str(output))
    shutil.copyfile(source / "LICENSE", output / "TRANSCRIBE-CPP-LICENSE.txt")
    shutil.copyfile(source / "ggml/LICENSE", output / "TRANSCRIBE-GGML-LICENSE.txt")
    shutil.copyfile(source / "THIRD-PARTY-LICENSES.md", output / "TRANSCRIBE-THIRD-PARTY.md")


if __name__ == "__main__":
    main()
