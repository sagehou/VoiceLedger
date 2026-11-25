#!/usr/bin/env bash
set -euo pipefail

export ANDROID_SERIAL=emulator-5554
export PATH="$ANDROID_HOME/platform-tools:$PATH"
mkdir -p artifacts
sudo chown "$(id -u)" /dev/kvm

cleanup() {
    timeout 10s adb emu kill >/dev/null 2>&1 || true
    kill "$emulator_pid" 2>/dev/null || true
}
printf 'no\n' | avdmanager create avd --force --name voiceledger-ci \
    --package 'system-images;android-35;google_apis;x86_64'
"$ANDROID_HOME/emulator/emulator" -avd voiceledger-ci -port 5554 \
    -no-window -no-snapshot -no-boot-anim -noaudio -gpu swiftshader_indirect \
    -camera-back none -camera-front none -accel on -cores 2 -memory 4096 \
    > artifacts/android-emulator.log 2>&1 &
emulator_pid=$!
trap cleanup EXIT

timeout 220s bash -c 'until [[ "$(adb shell -T getprop sys.boot_completed 2>/dev/null)" == 1 ]]; do sleep 2; done'
adb shell -T getprop ro.product.cpu.abilist | tee artifacts/android-emulator-abis.txt | grep -q arm64-v8a
adb install app/build/outputs/apk/debug/app-debug.apk
adb install app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
timeout 220s adb shell -T am instrument -w -r \
    com.voiceledger.app.test/com.voiceledger.app.AndroidRuntimeSmoke | tee artifacts/android-runtime.log
for stage in recording device_profile model_download; do
    grep -Fx "INSTRUMENTATION_RESULT: $stage=passed" artifacts/android-runtime.log
done
grep -Fx 'INSTRUMENTATION_CODE: -1' artifacts/android-runtime.log
