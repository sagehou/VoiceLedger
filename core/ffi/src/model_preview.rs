use std::path::Path;

use crate::{Error, guard};

const MAX_PATH_BYTES: usize = 128 * 1024;

fn preview(path: &[u8]) -> Result<Vec<u8>, Error> {
    let directory = std::str::from_utf8(path).map_err(|_| Error::InvalidArgument)?;
    let directory = Path::new(directory);
    if !directory.is_absolute() {
        return Err(Error::InvalidArgument);
    }
    voiceledger_core::models::preview_json(directory).map_err(|_| Error::Internal)
}

/// Read the bundled model options as UTF-8 JSON and return its byte length.
///
/// # Safety
/// The input is readable for length bytes and the output is writable for capacity bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vl_model_preview_v1(
    path: *const u8,
    length: u32,
    output: *mut u8,
    capacity: u32,
) -> i32 {
    guard(|| {
        if path.is_null() || output.is_null() || !(1..=MAX_PATH_BYTES).contains(&(length as usize))
        {
            return Err(Error::InvalidArgument);
        }
        let bytes = preview(unsafe { std::slice::from_raw_parts(path, length as usize) })?;
        if bytes.len() > capacity as usize {
            return Err(Error::InvalidArgument);
        }
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len()) };
        i32::try_from(bytes.len()).map_err(|_| Error::Internal)
    })
}

#[cfg(any(target_os = "android", target_os = "linux"))]
mod jni {
    use std::panic::AssertUnwindSafe;

    use ::jni::JNIEnv;
    use ::jni::objects::{JByteArray, JObject};

    use super::*;

    #[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeModelPreviewV1")]
    pub extern "system" fn model_preview(
        env: JNIEnv<'_>,
        _: JObject<'_>,
        path: JByteArray<'_>,
        output: JByteArray<'_>,
    ) -> i32 {
        guard(AssertUnwindSafe(|| {
            if path.as_raw().is_null()
                || output.as_raw().is_null()
                || !(1..=MAX_PATH_BYTES)
                    .contains(&(env.get_array_length(&path).map_err(|_| Error::Internal)? as usize))
            {
                return Err(Error::InvalidArgument);
            }
            let path = env.convert_byte_array(&path).map_err(|_| Error::Internal)?;
            let bytes = preview(&path)?;
            let capacity = env.get_array_length(&output).map_err(|_| Error::Internal)? as usize;
            if bytes.len() > capacity {
                return Err(Error::InvalidArgument);
            }
            let signed: Vec<i8> = bytes.iter().map(|byte| *byte as i8).collect();
            env.set_byte_array_region(&output, 0, &signed)
                .map_err(|_| Error::Internal)?;
            i32::try_from(bytes.len()).map_err(|_| Error::Internal)
        }))
    }
}
