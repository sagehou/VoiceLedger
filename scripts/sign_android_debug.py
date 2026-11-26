"""Sign debug APKs with the candidate environment's fixed key and verify its pin."""
import base64
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from tempfile import TemporaryDirectory


def main():
    signer, *paths = sys.argv[1:]
    if not paths or len(paths) % 2:
        raise SystemExit("Expected input and output APK pairs.")
    pin = os.environ.pop("VOICELEDGER_ANDROID_DEBUG_CERT_SHA256", "").lower()
    if not re.fullmatch("[0-9a-f]{64}", pin):
        raise SystemExit("The fixed Android debug certificate fingerprint is missing or invalid.")
    credentials = os.environ.pop("VOICELEDGER_ANDROID_DEBUG_SIGNING", "")
    if not credentials:
        raise SystemExit("Set VOICELEDGER_ANDROID_DEBUG_SIGNING in the android-debug-publish environment.")
    bundle = json.loads(credentials)
    with TemporaryDirectory(prefix="voiceledger-apk-signing-") as scratch:
        store = Path(scratch) / "debug.p12"
        store.write_bytes(base64.b64decode(bundle["pkcs12"], validate=True))
        store.chmod(0o600)
        env = dict(os.environ, VOICELEDGER_DEBUG_PKCS12_PASSWORD=bundle["password"])
        for original, signed in zip(paths[::2], paths[1::2]):
            subprocess.run([signer, "sign", "--ks", str(store), "--ks-type", "PKCS12",
                            "--ks-pass", "env:VOICELEDGER_DEBUG_PKCS12_PASSWORD",
                            "--key-pass", "env:VOICELEDGER_DEBUG_PKCS12_PASSWORD",
                            "--ks-key-alias", "androiddebugkey", "--out", signed, original],
                           env=env, check=True)
            verification = subprocess.check_output([signer, "verify", "--print-certs", signed], text=True)
            digests = re.findall(r"^Signer #\d+ certificate SHA-256 digest: ([0-9a-f]+)$", verification, re.M)
            if digests != [pin]:
                Path(signed).unlink()
                raise SystemExit("APK signer differs from the pinned Android debug certificate.")
    print(f"Android debug signer SHA-256 {pin}")


if __name__ == "__main__":
    main()
