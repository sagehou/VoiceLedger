"""Check the installed manifest and native libraries using the APK built in Actions."""

import argparse
from pathlib import Path
from xml.etree import ElementTree
from zipfile import ZipFile


ANDROID = "{http://schemas.android.com/apk/res/android}"
LIBRARIES = {
    "voiceledger_ffi", "voiceledger_whisper", "voiceledger_sensevoice",
    "sherpa-onnx-c-api", "onnxruntime",
}


def check(manifest_path, apk, package, preview):
    manifest = ElementTree.parse(manifest_path).getroot()
    assert manifest.get("package") == package, manifest.attrib
    application = manifest.find("application")
    assert application is not None
    assert application.get(ANDROID + "allowBackup") == "false"
    assert application.get(ANDROID + "dataExtractionRules")
    if preview:
        assert application.get(ANDROID + "debuggable") in (None, "false", "0")
        assert manifest.get(ANDROID + "versionName") == "0.1.0-preview.1"

    permissions = {entry.get(ANDROID + "name") for entry in manifest.findall("uses-permission")}
    required = {"RECORD_AUDIO", "FOREGROUND_SERVICE", "FOREGROUND_SERVICE_MICROPHONE",
                "POST_NOTIFICATIONS", "INTERNET"}
    assert {"android.permission." + name for name in required} <= permissions, permissions
    assert "android.permission.RECEIVE_BOOT_COMPLETED" not in permissions

    def service(suffix):
        item, = (
            service for service in application.findall("service")
            if service.get(ANDROID + "name", "").endswith(suffix)
        )
        assert item.get(ANDROID + "exported") == "false", item.attrib
        return item

    recording = service(".recording.RecordingService")
    kind = recording.get(ANDROID + "foregroundServiceType", "")
    assert kind == "microphone" or int(kind, 0) == 0x80, recording.attrib
    assert recording.get(ANDROID + "directBootAware") != "true", recording.attrib
    speech = service(".speech.SpeechWorkerService")
    assert speech.get(ANDROID + "process", "").endswith(":speech"), speech.attrib

    with ZipFile(apk) as archive:
        names = set(archive.namelist())
        assert {f"lib/arm64-v8a/lib{name}.so" for name in LIBRARIES} <= names
        if preview:
            abis = {name.split("/")[1] for name in names if name.startswith("lib/") and name.endswith(".so")}
            assert abis == {"arm64-v8a"}, abis
        assert "assets/speech-notices/THIRD-PARTY.md" in names
    print(f"Verified the {package} APK manifest, microphone service and native libraries.")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("apk", type=Path)
    parser.add_argument("package")
    parser.add_argument("--preview", action="store_true")
    args = parser.parse_args()
    check(args.manifest, args.apk, args.package, args.preview)


if __name__ == "__main__":
    main()
