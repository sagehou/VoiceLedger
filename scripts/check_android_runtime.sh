#!/usr/bin/env bash
set -euo pipefail

export ANDROID_SERIAL=emulator-5554
export ANDROID_AVD_HOME="$RUNNER_TEMP/voiceledger-avd"
export PATH="$ANDROID_HOME/platform-tools:$PATH"
mkdir -p artifacts "$ANDROID_AVD_HOME"
sudo chown "$(id -u)" /dev/kvm

stop_sampler() {
    if [[ -n ${sampler_pid:-} ]]; then
        kill "$sampler_pid" 2>/dev/null || true
        wait "$sampler_pid" 2>/dev/null || true
        unset sampler_pid
    fi
}

cleanup() {
    if (( $? != 0 )); then
        tail -40 artifacts/android-emulator.log
        timeout 10s adb logcat -d -s AndroidRuntime VoiceLedger > artifacts/android-emulator-errors.log 2>&1 || true
    fi
    stop_sampler
    timeout 10s adb emu kill >/dev/null 2>&1 || true
    kill "$emulator_pid" 2>/dev/null || true
}
avdmanager create avd --force --name voiceledger_ci --device pixel_5 \
    --package 'system-images;android-35;google_apis;x86_64' \
    --path "$ANDROID_AVD_HOME/voiceledger_ci.avd" < /dev/null
test -s "$ANDROID_AVD_HOME/voiceledger_ci.ini"
"$ANDROID_HOME/emulator/emulator" -avd voiceledger_ci -port 5554 \
    -no-window -no-snapshot -no-boot-anim -noaudio -gpu swiftshader \
    -camera-back none -camera-front none -accel on -cores 2 -memory 7168 \
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
adb shell -T settings put system screen_off_timeout 1800000
adb shell -T wm dismiss-keyguard
apk="${1:-app/build/outputs/apk/debug/app-debug.apk}"
seed_apk="${2:-$RUNNER_TEMP/voiceledger-upgrade-base.apk}"
version_code() {
    "$ANDROID_HOME/build-tools/36.0.0/aapt" dump badging "$1" | awk -F "'" '/^package: / {print $4}'
}
old_version="$(version_code "$seed_apk")"
new_version="$(version_code "$apk")"
test "$new_version" -gt "$old_version"
adb install --abi arm64-v8a "$seed_apk"
adb shell -T "run-as com.voiceledger.app sh -c 'mkdir -p files && echo retained > files/ci-upgrade-marker'"
adb install -r --abi arm64-v8a "$apk"
test "$(adb shell -T run-as com.voiceledger.app cat files/ci-upgrade-marker)" = retained
echo "APK replacement retained app data from version code $old_version to $new_version." | tee artifacts/android-upgrade.log
if [[ $# -gt 0 ]]; then
    exit 0
fi
adb install -t app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
instrument() {
    timeout 340s adb shell -T am instrument -w -r --abi arm64-v8a "$@" \
        com.voiceledger.app.test/com.voiceledger.app.AndroidRuntimeSmoke
}

instrument | tee artifacts/android-runtime.log
for stage in recording device_profile sensevoice_memory model_download; do
    grep -Fx "INSTRUMENTATION_RESULT: $stage=passed" artifacts/android-runtime.log
done
grep -Fx 'INSTRUMENTATION_CODE: -1' artifacts/android-runtime.log

timeout 45s bash -c '
    until [[ "$(adb shell -T id -u 2>/dev/null)" == 0 ]]; do
        adb root || true
        adb wait-for-device
        sleep 2
    done
'
raw="$(mktemp -d "$RUNNER_TEMP/android-asr.XXXXXX")"
sample_memory() {
    adb shell -T sh > "$raw/$1.tsv" <<'ANDROID' &
while :; do
    for name in com.voiceledger.app com.voiceledger.app:speech; do
        pid=$(pidof "$name") || continue
        awk -v name="$name" -v pid="$pid" '/^VmHWM:/ {print name "\t" pid "\t" $2}' "/proc/$pid/status" 2>/dev/null
    done
    sleep 0.25
done
ANDROID
    sampler_pid=$!
}

models=(sensevoice-small-gguf-q8-0 sensevoice-small-gguf-q6-k nemotron-3-5-asr-streaming-0-6b-q4-k-m)
for model in "${models[@]}"; do
    echo "Installing $model"
    adb shell -T am force-stop com.voiceledger.app.test
    adb shell -T am force-stop com.voiceledger.app
    sample_memory "$model-install"
    instrument -e install_model "$model" | tee "artifacts/$model-install.log"
    stop_sampler
    grep -Fx "INSTRUMENTATION_RESULT: model_download=passed" "artifacts/$model-install.log"
done

adb shell -T cmd connectivity airplane-mode enable
adb shell -T svc wifi disable
adb shell -T svc data disable
test "$(adb shell -T settings get global airplane_mode_on)" = 1
for model in "${models[@]}"; do
    echo "Measuring $model"
    adb shell -T am force-stop com.voiceledger.app.test
    adb shell -T am force-stop com.voiceledger.app
    sample_memory "$model-live"
    instrument -e benchmark_model "$model" | tee "artifacts/$model-live.log"
    stop_sampler
    grep -Fx "INSTRUMENTATION_RESULT: live_asr=passed" "artifacts/$model-live.log"
    adb exec-out run-as com.voiceledger.app cat cache/asr-comparison.json > "$raw/$model.json"
    python3 -c 'import json, sys; r = json.load(sys.stdin); print({key: r[key] for key in ("first_visible_text_ms", "first_visible_partial_ms", "completion_after_stop_ms")})' < "$raw/$model.json"
done
python3 ../scripts/report_android_asr.py live-speech "$raw" artifacts/android-asr-comparison.json
