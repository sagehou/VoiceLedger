use voiceledger_core::sync::{SyncError, check_provider};

use crate::{Error, guard};

const MAX_CONFIG: usize = 32 * 1024;

fn request(data: &[u8]) -> Result<i32, Error> {
    check_provider(data).map_err(|failure| match failure {
        SyncError::Configuration | SyncError::Scope => Error::SyncConfiguration,
        SyncError::AuthenticationRequired => Error::SyncAuthentication,
        SyncError::Quota => Error::SyncQuota,
        SyncError::ProviderUnsupported | SyncError::Redirect | SyncError::NotFound => {
            Error::SyncProvider
        }
        SyncError::Retry | SyncError::Transport | SyncError::LocalStorage => Error::SyncConnection,
        SyncError::Conflict | SyncError::Integrity => Error::SyncIntegrity,
    })?;
    Ok(0)
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

#[cfg(any(target_os = "android", target_os = "linux"))]
mod jni {
    use std::panic::AssertUnwindSafe;

    use ::jni::{
        JNIEnv,
        objects::{JByteArray, JObject},
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
}
