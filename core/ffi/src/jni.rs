use std::ffi::c_void;
use std::panic::AssertUnwindSafe;

use ::jni::JNIEnv;
use ::jni::objects::{JByteArray, JFloatArray, JObject, JObjectArray};

use super::*;

fn bounded_bytes(
    env: &JNIEnv<'_>,
    data: &JByteArray<'_>,
    maximum: usize,
) -> Result<Vec<u8>, Error> {
    if data.as_raw().is_null() {
        return Err(Error::InvalidArgument);
    }
    let length = env.get_array_length(data).map_err(|_| Error::Internal)? as usize;
    if length == 0 || length > maximum {
        return Err(Error::InvalidArgument);
    }
    env.convert_byte_array(data).map_err(|_| Error::Internal)
}

#[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeAbiVersion")]
pub extern "system" fn abi_version(_: *mut c_void, _: *mut c_void) -> i32 {
    vl_abi_version_v1()
}

#[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeValidateDomain")]
pub extern "system" fn validate_domain(
    env: JNIEnv<'_>,
    _: JObject<'_>,
    data: JByteArray<'_>,
) -> i32 {
    guard(AssertUnwindSafe(|| {
        let bytes = bounded_bytes(&env, &data, MAX_DOMAIN_BATCH_BYTES)?;
        Ok(validate_domain_bytes(&bytes))
    }))
}

#[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeCreate")]
pub extern "system" fn create(_: *mut c_void, _: *mut c_void) -> i64 {
    vl_session_create_v1()
}

#[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeTransition")]
pub extern "system" fn transition(
    _: *mut c_void,
    _: *mut c_void,
    handle: i64,
    command: i32,
    at_ns: i64,
) -> i32 {
    vl_session_transition_v1(handle, command, at_ns)
}

#[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeCancel")]
pub extern "system" fn cancel(_: *mut c_void, _: *mut c_void, handle: i64) -> i32 {
    vl_session_cancel_v1(handle)
}

#[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeRelease")]
pub extern "system" fn release(_: *mut c_void, _: *mut c_void, handle: i64) -> i32 {
    vl_session_release_v1(handle)
}

#[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeRecommend")]
pub extern "system" fn recommend(
    _: *mut c_void,
    _: *mut c_void,
    compatibility: i32,
    budget_bytes: i64,
    peak_bytes: i64,
) -> i32 {
    vl_recommend_v1(compatibility, budget_bytes, peak_bytes)
}

#[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeAudioOpenV1")]
pub extern "system" fn audio_open(env: JNIEnv<'_>, _: JObject<'_>, path: JByteArray<'_>) -> i64 {
    guard(AssertUnwindSafe(|| {
        audio::open(&bounded_bytes(&env, &path, audio::MAX_PATH_BYTES)?)
    }))
}

#[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeAudioRequestV1")]
pub extern "system" fn audio_request(
    env: JNIEnv<'_>,
    _: JObject<'_>,
    handle: i64,
    command: JByteArray<'_>,
    pcm: JFloatArray<'_>,
    count: i32,
    output: JObjectArray<'_>,
) -> i32 {
    guard(AssertUnwindSafe(|| {
        let json = bounded_bytes(&env, &command, audio::MAX_REQUEST_BYTES)?;
        if output.as_raw().is_null()
            || env.get_array_length(&output).map_err(|_| Error::Internal)? != 1
            || count < 0
            || count as usize > audio::MAX_PCM_SAMPLES
        {
            return Err(Error::InvalidArgument);
        }
        let mut samples = Vec::new();
        if count != 0 {
            if pcm.as_raw().is_null()
                || env.get_array_length(&pcm).map_err(|_| Error::Internal)? < count
            {
                return Err(Error::InvalidArgument);
            }
            samples.resize(count as usize, 0.0);
            env.get_float_array_region(&pcm, 0, &mut samples)
                .map_err(|_| Error::Internal)?;
        }
        if let Some(data) = audio::request(handle, &json, &samples)? {
            let bytes = env
                .byte_array_from_slice(&data)
                .map_err(|_| Error::Internal)?;
            env.set_object_array_element(&output, 0, bytes)
                .map_err(|_| Error::Internal)?;
        }
        Ok(0)
    }))
}

#[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeAudioReleaseV1")]
pub extern "system" fn audio_release(_: *mut c_void, _: *mut c_void, handle: i64) -> i32 {
    audio::vl_audio_release_v1(handle)
}
