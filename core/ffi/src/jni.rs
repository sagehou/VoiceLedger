use std::ffi::c_void;

use super::*;

#[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeAbiVersion")]
pub extern "system" fn abi_version(_: *mut c_void, _: *mut c_void) -> i32 {
    vl_abi_version_v1()
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
