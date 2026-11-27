use std::ptr;

use voiceledger_core::vault::{MAX_PLAINTEXT_BYTES, Vault};

use crate::{Error, guard};

const MAX_RECOVERY_BYTES: usize = 128;
const MAX_OBJECT_BYTES: usize = MAX_PLAINTEXT_BYTES + 64;

fn request(
    operation: i32,
    recovery: &[u8],
    expected: &[u8],
    data: &[u8],
    write: impl FnOnce(&[u8]) -> Result<i32, Error>,
) -> Result<i32, Error> {
    if operation == 1 && recovery.is_empty() && expected.is_empty() && data.is_empty() {
        let vault = Vault::create().map_err(|_| Error::VaultRejected)?;
        return write(vault.recovery_code().as_bytes());
    }
    let recover = || {
        let code = std::str::from_utf8(recovery).map_err(|_| Error::VaultRejected)?;
        Vault::recover(code).map_err(|_| Error::VaultRejected)
    };
    match operation {
        2 if expected.is_empty() => {
            let sealed = recover()?.seal(data).map_err(|_| Error::VaultRejected)?;
            write(sealed.envelope())
        }
        3 if expected.len() == 16 => {
            let object = expected.try_into().map_err(|_| Error::InvalidArgument)?;
            let opened = recover()?
                .open(object, data)
                .map_err(|_| Error::VaultRejected)?;
            write(&opened)
        }
        4 if expected.is_empty() && data.is_empty() => {
            let vault = recover()?;
            let mut identity = [0; 20];
            identity[..16].copy_from_slice(vault.id());
            identity[16..].copy_from_slice(&vault.generation().to_be_bytes());
            write(&identity)
        }
        5 if expected.is_empty() && data.is_empty() => {
            let next = recover()?.rotate().map_err(|_| Error::VaultRejected)?;
            write(next.recovery_code().as_bytes())
        }
        _ => Err(Error::InvalidArgument),
    }
}

unsafe fn input<'a>(data: *const u8, length: u32, maximum: usize) -> Result<&'a [u8], Error> {
    let length = length as usize;
    if length == 0 {
        return Ok(&[]);
    }
    if data.is_null() || length > maximum {
        return Err(Error::InvalidArgument);
    }
    Ok(unsafe { std::slice::from_raw_parts(data, length) })
}

/// Create or rotate recovery material, inspect its vault identity, or process a vault object.
///
/// # Safety
/// Inputs are readable for their lengths and stay alive through the call. The
/// output is writable for capacity bytes. No input overlaps the output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vl_vault_request_v1(
    operation: i32,
    recovery: *const u8,
    recovery_length: u32,
    expected: *const u8,
    expected_length: u32,
    data: *const u8,
    length: u32,
    output: *mut u8,
    capacity: u32,
) -> i32 {
    guard(|| {
        let recovery = unsafe { input(recovery, recovery_length, MAX_RECOVERY_BYTES) }?;
        let expected = unsafe { input(expected, expected_length, 16) }?;
        let data = unsafe { input(data, length, MAX_OBJECT_BYTES) }?;
        request(operation, recovery, expected, data, |bytes| {
            if !bytes.is_empty() {
                if output.is_null() || bytes.len() > capacity as usize {
                    return Err(Error::InvalidArgument);
                }
                unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len()) };
            }
            i32::try_from(bytes.len()).map_err(|_| Error::InvalidArgument)
        })
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

    #[unsafe(export_name = "Java_com_voiceledger_app_core_NativeVault_nativeRequest")]
    pub extern "system" fn execute(
        env: JNIEnv<'_>,
        _: JObject<'_>,
        operation: i32,
        recovery: JByteArray<'_>,
        expected: JByteArray<'_>,
        data: JByteArray<'_>,
        output: JObjectArray<'_>,
    ) -> i32 {
        guard(AssertUnwindSafe(|| {
            let copy = |input: &JByteArray<'_>, limit| {
                if input.as_raw().is_null()
                    || env.get_array_length(input).map_err(|_| Error::Internal)? as usize > limit
                {
                    return Err(Error::InvalidArgument);
                }
                env.convert_byte_array(input).map_err(|_| Error::Internal)
            };
            if output.as_raw().is_null()
                || env.get_array_length(&output).map_err(|_| Error::Internal)? != 1
            {
                return Err(Error::InvalidArgument);
            }
            let recovery = Zeroizing::new(copy(&recovery, MAX_RECOVERY_BYTES)?);
            let expected = copy(&expected, 16)?;
            let data = Zeroizing::new(copy(&data, MAX_OBJECT_BYTES)?);
            request(operation, &recovery, &expected, &data, |bytes| {
                let array = env
                    .byte_array_from_slice(bytes)
                    .map_err(|_| Error::Internal)?;
                env.set_object_array_element(&output, 0, array)
                    .map_err(|_| Error::Internal)?;
                Ok(0)
            })
        }))
    }
}
