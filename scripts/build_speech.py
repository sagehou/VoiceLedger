import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import urllib.request
from zipfile import ZipFile


WHISPER = "4979e04f5dcaccb36057e059bbaed8a2f5288315"
SHERPA = "5ce3d6d93a5f4fe11657bf11a6bf3a022eeef22f"
SHERPA_RELEASE = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.12.20/"
REPO = Path(__file__).resolve().parents[1]


def fetch(directory, name, url, digest):
    path = directory / name
    with urllib.request.urlopen(url, timeout=60) as response, path.open("wb") as file:
        shutil.copyfileobj(response, file)
    with path.open("rb") as file:
        if hashlib.file_digest(file, "sha256").hexdigest() != digest:
            raise RuntimeError(f"SHA-256 mismatch for {name}")
    return path


def unpack(path, directory):
    with tarfile.open(path) as archive:
        archive.extractall(directory, filter="data")


def run(*args, env=None):
    subprocess.run(args, check=True, env=env)


def android_sherpa(work, cmake_options):
    archive = fetch(work, "sherpa-source.tar.gz",
        f"https://codeload.github.com/k2-fsa/sherpa-onnx/tar.gz/{SHERPA}",
        "96b1a69ab0a85a8b42437a6c7f169b55dcf480997d761d5c6d79abbc2842e69b")
    unpack(archive, work)
    source = work / f"sherpa-onnx-{SHERPA}"
    aar = fetch(work, "onnxruntime.aar",
        "https://repo.maven.apache.org/maven2/com/microsoft/onnxruntime/onnxruntime-android/1.23.2/onnxruntime-android-1.23.2.aar",
        "82048d1f462218adae4ba76477089ab0ba76093d84f733540066db1a8ba6b827")
    runtime = work / "onnxruntime"
    with ZipFile(aar) as archive:
        names = [name for name in archive.namelist() if name.startswith("headers/")]
        archive.extractall(runtime, names + ["jni/arm64-v8a/libonnxruntime.so"])
    env = dict(os.environ,
        SHERPA_ONNXRUNTIME_INCLUDE_DIR=str(runtime / "headers"),
        SHERPA_ONNXRUNTIME_LIB_DIR=str(runtime / "jni/arm64-v8a"))
    target = work / "sherpa-build"
    install = work / "sherpa-install"
    run("cmake", "-S", str(source), "-B", str(target), *cmake_options,
        f"-DCMAKE_INSTALL_PREFIX={install}", "-DBUILD_SHARED_LIBS=ON",
        "-DSHERPA_ONNX_ENABLE_C_API=ON", "-DSHERPA_ONNX_ENABLE_JNI=OFF",
        "-DSHERPA_ONNX_ENABLE_PORTAUDIO=OFF", "-DSHERPA_ONNX_ENABLE_WEBSOCKET=OFF",
        "-DSHERPA_ONNX_ENABLE_TTS=OFF", "-DSHERPA_ONNX_ENABLE_SPEAKER_DIARIZATION=OFF",
        "-DSHERPA_ONNX_ENABLE_BINARY=OFF", env=env)
    run("cmake", "--build", str(target), "--parallel", "3", env=env)
    run("cmake", "--install", str(target), env=env)
    libs = list((install / "lib").glob("*.so"))
    if not any(path.name == "libonnxruntime.so" for path in libs):
        libs.append(runtime / "jni/arm64-v8a/libonnxruntime.so")
    return install / "include", install / "lib/libsherpa-onnx-c-api.so", libs


def desktop_sherpa(work):
    if os.name == "nt":
        platform = "win-x64"
        digest = "c10a095df39d10d48d97e22d9297ebd3bc12cca80ec3be70eb71f7a5b4a2f6c3"
        linker = "sherpa-onnx-c-api.lib"
        pattern = "*.dll"
    else:
        platform = "linux-x64"
        digest = "7200c1ee5b587d1bcb9a6974f88d4cc3a7d43020eb338b6801d6b9f5d2c565e4"
        linker = "libsherpa-onnx-c-api.so"
        pattern = "*.so*"
    prefix = f"sherpa-onnx-v1.12.20-{platform}-shared-no-tts"
    archive = fetch(work, f"{prefix}.tar.bz2", SHERPA_RELEASE + f"{prefix}.tar.bz2", digest)
    unpack(archive, work)
    root = work / prefix
    return root / "include", root / "lib" / linker, list((root / "lib").glob(pattern))


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
    archive = fetch(work, "whisper-source.tar.gz",
        f"https://codeload.github.com/ggml-org/whisper.cpp/tar.gz/{WHISPER}",
        "ec8e33f4c838ed459ad0a23da255c0262f150a6f54839a17e504ddf1495305d5")
    unpack(archive, work)

    options = ["-DCMAKE_BUILD_TYPE=Release", "-DCMAKE_POLICY_VERSION_MINIMUM=3.5"]
    if args.android_ndk:
        options.extend([
            f"-DCMAKE_TOOLCHAIN_FILE={args.android_ndk.resolve() / 'build/cmake/android.toolchain.cmake'}",
            "-DANDROID_PLATFORM=android-26", "-DANDROID_ABI=arm64-v8a", "-DANDROID_STL=c++_static",
        ])
        include, library, libs = android_sherpa(work, options)
    else:
        if os.name == "nt":
            options.extend(["-A", "x64"])
        include, library, libs = desktop_sherpa(work)

    build = work / "adapters"
    run("cmake", "-S", str(REPO / "native/speech"), "-B", str(build), *options,
        f"-DWHISPER_SOURCE={work / f'whisper.cpp-{WHISPER}'}",
        f"-DSHERPA_INCLUDE={include}", f"-DSHERPA_LIBRARY={library}")
    run("cmake", "--build", str(build), "--config", "Release", "--parallel", "3")
    if not args.android_ndk:
        env = dict(os.environ)
        key = "PATH" if os.name == "nt" else "LD_LIBRARY_PATH"
        env[key] = str(library.parent) + os.pathsep + env.get(key, "")
        run("ctest", "--test-dir", str(build), "-C", "Release", "--output-on-failure", env=env)
    run("cmake", "--install", str(build), "--config", "Release", "--prefix", str(output))
    for library in libs:
        shutil.copyfile(library, output / library.name)
    shutil.copyfile(REPO / "native/speech/THIRD-PARTY.md", output / "THIRD-PARTY.md")
    shutil.copyfile(work / f"whisper.cpp-{WHISPER}" / "LICENSE", output / "WHISPER-LICENSE.txt")


if __name__ == "__main__":
    main()
