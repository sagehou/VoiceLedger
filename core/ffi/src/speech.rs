use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::audio::AudioBuffer;
use crate::{Error, guard};

const MAX_REQUEST: usize = 128 * 1024;
static WORK: Mutex<()> = Mutex::new(());
static CANCELLED: AtomicBool = AtomicBool::new(false);
static PARTIAL: Mutex<Vec<u8>> = Mutex::new(Vec::new());

impl From<voiceledger_speech::Error> for Error {
    fn from(error: voiceledger_speech::Error) -> Self {
        use voiceledger_speech::Error as Speech;
        match error {
            Speech::Invalid => Self::InvalidArgument,
            Speech::Cancelled => Self::Cancelled,
            Speech::Storage => Self::SpeechStorage,
            Speech::Runtime => Self::SpeechRuntime,
            Speech::NoAudio => Self::SpeechNoAudio,
            Speech::NotInstalled => Self::SpeechNotInstalled,
            Speech::Blocked(_) => Self::SpeechBlocked,
            Speech::Transfer => Self::SpeechTransfer,
            Speech::Integrity => Self::SpeechIntegrity,
        }
    }
}

fn request(data: &[u8]) -> Result<Vec<u8>, Error> {
    let request = voiceledger_speech::Request::decode(data)?;
    if request.read_only() {
        return Ok(request.run(&|| false, |_| {})?);
    }
    let _worker = WORK.try_lock().map_err(|_| Error::SpeechBusy)?;
    CANCELLED.store(false, Ordering::Release);
    struct ClearPartial;
    impl Drop for ClearPartial {
        fn drop(&mut self) {
            if let Ok(mut partial) = PARTIAL.lock() {
                partial.clear();
            }
        }
    }
    let _clear = ClearPartial;
    Ok(request.run(&|| CANCELLED.load(Ordering::Acquire), |value| {
        if let Ok(mut partial) = PARTIAL.lock() {
            *partial = value
                .and_then(|value| voiceledger_speech::encode_partial(value).ok())
                .unwrap_or_default();
        }
    })?)
}

fn partial() -> Result<Vec<u8>, Error> {
    Ok(PARTIAL.lock().map_err(|_| Error::Internal)?.clone())
}

/// Execute a private local speech request.
///
/// # Safety
/// The input remains readable for length bytes. The output points to a writable AudioBuffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vl_speech_request_v1(
    data: *const u8,
    length: u32,
    output: *mut AudioBuffer,
) -> i32 {
    guard(|| {
        if output.is_null() {
            return Err(Error::InvalidArgument);
        }
        unsafe { output.write(AudioBuffer::empty()) };
        if data.is_null() || !(1..=MAX_REQUEST).contains(&(length as usize)) {
            return Err(Error::InvalidArgument);
        }
        let result = request(unsafe { std::slice::from_raw_parts(data, length as usize) })?;
        let response = AudioBuffer::owning(result)?;
        unsafe { output.write(response) };
        Ok(0)
    })
}

/// Read the current ephemeral partial.
///
/// # Safety
/// The output points to a writable AudioBuffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vl_speech_partial_v1(output: *mut AudioBuffer) -> i32 {
    guard(|| {
        if output.is_null() {
            return Err(Error::InvalidArgument);
        }
        unsafe { output.write(AudioBuffer::empty()) };
        let response = AudioBuffer::owning(partial()?)?;
        unsafe { output.write(response) };
        Ok(0)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn vl_speech_cancel_v1() {
    CANCELLED.store(true, Ordering::Release);
}

#[cfg(any(target_os = "android", target_os = "linux"))]
mod jni {
    use super::*;
    use ::jni::JNIEnv;
    use ::jni::objects::{JByteArray, JObject, JObjectArray};
    use std::panic::AssertUnwindSafe;

    fn set(env: JNIEnv<'_>, output: JObjectArray<'_>, data: Vec<u8>) -> Result<i32, Error> {
        if output.as_raw().is_null()
            || env.get_array_length(&output).map_err(|_| Error::Internal)? != 1
        {
            return Err(Error::InvalidArgument);
        }
        let bytes = env
            .byte_array_from_slice(&data)
            .map_err(|_| Error::Internal)?;
        env.set_object_array_element(&output, 0, bytes)
            .map_err(|_| Error::Internal)?;
        Ok(0)
    }

    #[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeSpeechRequestV1")]
    pub extern "system" fn speech_request(
        env: JNIEnv<'_>,
        _: JObject<'_>,
        data: JByteArray<'_>,
        output: JObjectArray<'_>,
    ) -> i32 {
        guard(AssertUnwindSafe(|| {
            if data.as_raw().is_null()
                || !(1..=MAX_REQUEST)
                    .contains(&(env.get_array_length(&data).map_err(|_| Error::Internal)? as usize))
                || output.as_raw().is_null()
                || env.get_array_length(&output).map_err(|_| Error::Internal)? != 1
            {
                return Err(Error::InvalidArgument);
            }
            let bytes = env.convert_byte_array(&data).map_err(|_| Error::Internal)?;
            let data = request(&bytes)?;
            set(env, output, data)
        }))
    }

    #[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeSpeechPartialV1")]
    pub extern "system" fn speech_partial(
        env: JNIEnv<'_>,
        _: JObject<'_>,
        output: JObjectArray<'_>,
    ) -> i32 {
        guard(AssertUnwindSafe(|| set(env, output, partial()?)))
    }

    #[unsafe(export_name = "Java_com_voiceledger_app_core_NativeCore_nativeSpeechCancelV1")]
    pub extern "system" fn speech_cancel(_: JNIEnv<'_>, _: JObject<'_>) {
        vl_speech_cancel_v1();
    }
}
