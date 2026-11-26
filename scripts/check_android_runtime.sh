#!/usr/bin/env bash
set -euo pipefail

export ANDROID_SERIAL=emulator-5554
export ANDROID_AVD_HOME="$RUNNER_TEMP/voiceledger-avd"
export PATH="$ANDROID_HOME/platform-tools:$PATH"
mkdir -p artifacts "$ANDROID_AVD_HOME"
sudo chown "$(id -u)" /dev/kvm

cleanup() {
    if (( $? != 0 )); then
        tail -40 artifacts/android-emulator.log
        timeout 10s adb logcat -d -s AndroidRuntime VoiceLedger > artifacts/android-emulator-errors.log 2>&1 || true
    fi
    timeout 10s adb emu kill >/dev/null 2>&1 || true
    kill "$emulator_pid" 2>/dev/null || true
}
avdmanager create avd --force --name voiceledger_ci --device pixel_5 \
    --package 'system-images;android-35;google_apis;x86_64' \
    --path "$ANDROID_AVD_HOME/voiceledger_ci.avd" < /dev/null
test -s "$ANDROID_AVD_HOME/voiceledger_ci.ini"
"$ANDROID_HOME/emulator/emulator" -avd voiceledger_ci -port 5554 \
    -no-window -no-snapshot -no-boot-anim -noaudio -gpu swiftshader \
    -camera-back none -camera-front none -accel on -cores 2 -memory 6144 \
    > artifacts/android-emulator.log 2>&1 &
emulator_pid=$!
trap cleanup EXIT

timeout 220s bash -c '
    until [[ "$(adb shell -T getprop sys.boot_completed 2>/dev/null)" == 1 ]]; do
        kill -0 "$1" || exit 1
        sleep 2
    done
' bash "$emulator_pid"
adb shell -T getprop ro.product.cpu.abilist | tee artifacts/android-emulator-abis.txt | grep -q arm64-v8a
adb install --abi arm64-v8a app/build/outputs/apk/debug/app-debug.apk
adb install -t app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
timeout 360s adb shell -T am instrument -w -r --abi arm64-v8a \
    com.voiceledger.app.test/com.voiceledger.app.AndroidRuntimeSmoke | tee artifacts/android-runtime.log
for stage in recording device_profile model_download live_whisper live_sensevoice; do
    grep -Fx "INSTRUMENTATION_RESULT: $stage=passed" artifacts/android-runtime.log
done
grep -Fx 'INSTRUMENTATION_CODE: -1' artifacts/android-runtime.log
