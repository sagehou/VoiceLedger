use voiceledger_core::sync::{SyncError, check_provider};

use crate::audio::AudioBuffer;
use crate::{Error, guard};

const MAX_CONFIG: usize = 32 * 1024;

fn request(data: &[u8]) -> Result<i32, Error> {
    check_provider(data).map_err(map_error)?;
    Ok(0)
}

fn map_error(failure: SyncError) -> Error {
    match failure {
        SyncError::Configuration | SyncError::Scope => Error::SyncConfiguration,
        SyncError::AuthenticationRequired => Error::SyncAuthentication,
        SyncError::Quota => Error::SyncQuota,
        SyncError::ProviderUnsupported | SyncError::Redirect | SyncError::NotFound => {
            Error::SyncProvider
        }
        SyncError::Retry | SyncError::Transport | SyncError::LocalStorage => Error::SyncConnection,
        SyncError::Conflict | SyncError::Integrity => Error::SyncIntegrity,
        SyncError::GenerationRetired => Error::SyncGenerationRetired,
    }
}

/// Test a configured provider with one encrypted synthetic sample.
///
/// # Safety
/// The caller supplies readable UTF-8 JSON for length bytes until the call returns,
/// and clears the credential buffer afterward. Call this from a background worker.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vl_sync_check_provider_v1(data: *const u8, length: u32) -> i32 {
    guard(|| {
        let length = length as usize;
        if data.is_null() || !(1..=MAX_CONFIG).contains(&length) {
            return Err(Error::InvalidArgument);
        }
        request(unsafe { std::slice::from_raw_parts(data, length) })
    })
}

unsafe fn borrowed<'a>(data: *const u8, length: u32, max: usize) -> Result<&'a [u8], Error> {
    let length = length as usize;
    if length == 0 {
        return Ok(&[]);
    }
    if data.is_null() || length > max {
        return Err(Error::InvalidArgument);
    }
    Ok(unsafe { std::slice::from_raw_parts(data, length) })
}

/// Execute an explicit synchronization or device operation. The caller clears credentials.
///
/// # Safety
/// All input buffers remain readable for their lengths through the call. Output is writable
/// and does not alias input. Use the audio buffer release function on the returned buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vl_sync_request_v1(
    command: *const u8,
    command_length: u32,
    provider: *const u8,
    provider_length: u32,
    recovery: *const u8,
    recovery_length: u32,
    successor: *const u8,
    successor_length: u32,
    output: *mut AudioBuffer,
) -> i32 {
    guard(|| {
        if output.is_null() {
            return Err(Error::InvalidArgument);
        }
        unsafe { output.write(AudioBuffer::empty()) };
        let command = unsafe { borrowed(command, command_length, MAX_CONFIG) }?;
        let provider = unsafe { borrowed(provider, provider_length, MAX_CONFIG) }?;
        let recovery = unsafe { borrowed(recovery, recovery_length, 128) }?;
        let successor = unsafe { borrowed(successor, successor_length, 128) }?;
        let result = voiceledger_speech::sync_request(command, provider, recovery, successor)
            .map_err(map_error)?;
        unsafe { output.write(AudioBuffer::owning(result)?) };
        Ok(0)
    })
}

#[cfg(any(target_os = "android", target_os = "linux"))]
mod jni {
    use std::panic::AssertUnwindSafe;

    use ::jni::{
        JNIEnv,
        objects::{JByteArray, JObject, JObjectArray},
    };
    use zeroize::Zeroizing;

    use super::*;

    #[unsafe(export_name = "Java_com_voiceledger_app_core_NativeSync_nativeCheckProvider")]
    pub extern "system" fn check(env: JNIEnv<'_>, _: JObject<'_>, data: JByteArray<'_>) -> i32 {
        guard(AssertUnwindSafe(|| {
            if data.as_raw().is_null()
                || !(1..=MAX_CONFIG)
                    .contains(&(env.get_array_length(&data).map_err(|_| Error::Internal)? as usize))
            {
                return Err(Error::InvalidArgument);
            }
            let data = Zeroizing::new(env.convert_byte_array(data).map_err(|_| Error::Internal)?);
            request(&data)
        }))
    }

    #[unsafe(export_name = "Java_com_voiceledger_app_core_NativeSync_nativeTransfer")]
    pub extern "system" fn transfer(
        env: JNIEnv<'_>,
        _: JObject<'_>,
        command: JByteArray<'_>,
        provider: JByteArray<'_>,
        recovery: JByteArray<'_>,
        successor: JByteArray<'_>,
        output: JObjectArray<'_>,
    ) -> i32 {
        guard(AssertUnwindSafe(|| {
            let copy = |input: &JByteArray<'_>, limit| {
                if input.as_raw().is_null()
                    || env.get_array_length(input).map_err(|_| Error::Internal)? as usize > limit
                {
                    return Err(Error::InvalidArgument);
                }
                env.convert_byte_array(input)
                    .map(Zeroizing::new)
                    .map_err(|_| Error::Internal)
            };
            if output.as_raw().is_null()
                || env.get_array_length(&output).map_err(|_| Error::Internal)? != 1
            {
                return Err(Error::InvalidArgument);
            }
            let command = copy(&command, MAX_CONFIG)?;
            let provider = copy(&provider, MAX_CONFIG)?;
            let recovery = copy(&recovery, 128)?;
            let successor = copy(&successor, 128)?;
            let result =
                voiceledger_speech::sync_request(&command, &provider, &recovery, &successor)
                    .map_err(map_error)?;
            let bytes = env
                .byte_array_from_slice(&result)
                .map_err(|_| Error::Internal)?;
            env.set_object_array_element(&output, 0, bytes)
                .map_err(|_| Error::Internal)?;
            Ok(0)
        }))
    }
}
