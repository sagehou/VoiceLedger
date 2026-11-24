use std::path::Path;

use voiceledger_core::device_profile;

use crate::{Error, guard};

const MAX_PATH_BYTES: usize = 128 * 1024;

fn save(directory: &[u8], measurement: &[u8]) -> Result<i32, Error> {
    let directory = std::str::from_utf8(directory).map_err(|_| Error::InvalidArgument)?;
    let directory = Path::new(directory);
    if !directory.is_absolute() {
        return Err(Error::InvalidArgument);
    }
    match device_profile::save(directory, measurement) {
        Ok(()) => Ok(0),
        Err(device_profile::Error::Contract(error)) => Ok(error as i32),
        Err(device_profile::Error::Storage) => Err(Error::DeviceProfileStorage),
    }
}

/// Store a v1 device profile under an app-private directory.
///
/// # Safety
/// Both pointers must remain readable for their stated byte lengths during this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vl_device_profile_save_v1(
    directory: *const u8,
    directory_length: u32,
    measurement: *const u8,
    measurement_length: u32,
) -> i32 {
    guard(|| {
        if directory.is_null()
            || measurement.is_null()
            || !(1..=MAX_PATH_BYTES).contains(&(directory_length as usize))
            || !(1..=device_profile::MAX_BYTES).contains(&(measurement_length as usize))
        {
            return Err(Error::InvalidArgument);
        }
        let directory = unsafe { std::slice::from_raw_parts(directory, directory_length as usize) };
        let measurement =
            unsafe { std::slice::from_raw_parts(measurement, measurement_length as usize) };
        save(directory, measurement)
    })
}

#[cfg(any(target_os = "android", target_os = "linux"))]
mod jni {
    use std::panic::AssertUnwindSafe;

    use ::jni::JNIEnv;
    use ::jni::objects::{JByteArray, JObject};

    use super::*;

    fn read(env: &JNIEnv<'_>, array: &JByteArray<'_>, maximum: usize) -> Result<Vec<u8>, Error> {
        if array.as_raw().is_null()
            || !(1..=maximum)
                .contains(&(env.get_array_length(array).map_err(|_| Error::Internal)? as usize))
        {
            return Err(Error::InvalidArgument);
        }
        env.convert_byte_array(array).map_err(|_| Error::Internal)
    }

    #[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeSaveDeviceProfileV1")]
    pub extern "system" fn save_profile(
        env: JNIEnv<'_>,
        _: JObject<'_>,
        directory: JByteArray<'_>,
        measurement: JByteArray<'_>,
    ) -> i32 {
        guard(AssertUnwindSafe(|| {
            let directory = read(&env, &directory, MAX_PATH_BYTES)?;
            let measurement = read(&env, &measurement, device_profile::MAX_BYTES)?;
            save(&directory, &measurement)
        }))
    }
}
