use std::path::Path;
use std::sync::Arc;

use voiceledger_audio::{MAX_CHANNELS, MAX_FRAMES, Store};

use crate::{Error, guard, handles};

pub(crate) const MAX_PATH_BYTES: usize = 128 * 1024;
pub(crate) const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_PCM_SAMPLES: usize = MAX_CHANNELS * MAX_FRAMES;

impl From<voiceledger_audio::Error> for Error {
    fn from(error: voiceledger_audio::Error) -> Self {
        use voiceledger_audio::Error as AudioError;
        match error {
            AudioError::Invalid | AudioError::Json(_) => Self::InvalidArgument,
            AudioError::Locked => Self::AudioLocked,
            AudioError::Unavailable => Self::AudioUnavailable,
            AudioError::InvalidMedia => Self::AudioMedia,
            AudioError::UnsupportedSchema => Self::AudioSchema,
            AudioError::Internal => Self::Internal,
            AudioError::Io(_) | AudioError::Sql(_) => Self::AudioStore,
        }
    }
}

#[repr(C)]
pub struct AudioBuffer {
    data: *mut u8,
    length: u32,
}

impl AudioBuffer {
    pub(super) fn empty() -> Self {
        Self {
            data: std::ptr::null_mut(),
            length: 0,
        }
    }

    pub(super) fn owning(response: Vec<u8>) -> Result<Self, Error> {
        if response.is_empty() {
            return Ok(Self::empty());
        }
        let length = u32::try_from(response.len()).map_err(|_| Error::Internal)?;
        let data = Box::into_raw(response.into_boxed_slice()) as *mut u8;
        Ok(Self { data, length })
    }
}

/// Open and reconcile one device-local store.
///
/// # Safety
/// The caller supplies a readable UTF-8 string of exactly length bytes for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vl_audio_open_v1(path: *const u8, length: u32) -> i64 {
    guard(|| {
        if path.is_null() || length == 0 || length as usize > MAX_PATH_BYTES {
            return Err(Error::InvalidArgument);
        }
        open(unsafe { std::slice::from_raw_parts(path, length as usize) })
    })
}

pub(crate) fn open(path: &[u8]) -> Result<i64, Error> {
    let path = std::str::from_utf8(path).map_err(|_| Error::InvalidArgument)?;
    let store = Arc::new(Store::open(Path::new(path))?);
    let mut handles = handles()?;
    let id = handles.next_id()?;
    handles.stores.insert(id, store);
    Ok(id)
}

pub(crate) fn request(handle: i64, json: &[u8], pcm: &[f32]) -> Result<Option<Vec<u8>>, Error> {
    let store = handles()?
        .stores
        .get(&handle)
        .cloned()
        .ok_or(Error::UnknownHandle)?;
    Ok(store.execute_json(json, pcm)?.into_bytes())
}

/// Execute a versioned UTF-8 JSON command and optionally return an owned buffer.
///
/// # Safety
/// Inputs remain readable for the call. PCM is aligned 32-bit float and borrows count samples.
/// The output must point to a writable AudioBuffer, which the call initializes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vl_audio_request_v1(
    handle: i64,
    json: *const u8,
    length: u32,
    pcm: *const f32,
    count: u32,
    result: *mut AudioBuffer,
) -> i32 {
    guard(|| {
        if result.is_null() {
            return Err(Error::InvalidArgument);
        }
        unsafe { result.write(AudioBuffer::empty()) };
        if json.is_null()
            || length == 0
            || length as usize > MAX_REQUEST_BYTES
            || count as usize > MAX_PCM_SAMPLES
        {
            return Err(Error::InvalidArgument);
        }
        let json = unsafe { std::slice::from_raw_parts(json, length as usize) };
        let pcm = if count == 0 {
            &[]
        } else {
            if pcm.is_null() || !pcm.is_aligned() {
                return Err(Error::InvalidArgument);
            }
            unsafe { std::slice::from_raw_parts(pcm, count as usize) }
        };
        let Some(response) = request(handle, json, pcm)? else {
            return Ok(0);
        };
        let response = AudioBuffer::owning(response)?;
        unsafe { result.write(response) };
        Ok(0)
    })
}

/// # Safety
/// A non-null buffer and length must come from one successful request and be released once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vl_audio_buffer_release_v1(data: *mut u8, length: u32) {
    if !data.is_null() {
        let buffer = std::ptr::slice_from_raw_parts_mut(data, length as usize);
        unsafe { drop(Box::from_raw(buffer)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn vl_audio_release_v1(handle: i64) -> i32 {
    guard(|| {
        let store = handles()?
            .stores
            .remove(&handle)
            .ok_or(Error::UnknownHandle)?;
        drop(store);
        Ok(0)
    })
}
