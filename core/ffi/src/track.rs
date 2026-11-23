use voiceledger_core::track::{TrackCommand, TrackTransitionError};

use super::{Error, MAX_DOMAIN_BATCH_BYTES, TrackMachine, guard, handles};

unsafe fn endpoint<'a>(data: *const u8, length: u32) -> Result<&'a str, Error> {
    if data.is_null() || length == 0 || length as usize > MAX_DOMAIN_BATCH_BYTES {
        return Err(Error::InvalidArgument);
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(data, length as usize) })
        .map_err(|_| Error::InvalidArgument)
}

/// Create an owned track with a fixed endpoint identity.
///
/// # Safety
/// A non-null pointer must be readable for `length` bytes throughout the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vl_track_create_v1(data: *const u8, length: u32) -> i64 {
    guard(|| {
        let machine = TrackMachine::new(unsafe { endpoint(data, length) }?);
        let mut handles = handles()?;
        let id = handles.next_id()?;
        handles.tracks.insert(id, machine);
        Ok(id)
    })
}

/// Apply one track transition and return its new state.
///
/// # Safety
/// If `length` is nonzero, a non-null pointer must be readable for `length` bytes during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vl_track_transition_v1(
    handle: i64,
    code: i32,
    at_ns: i64,
    data: *const u8,
    length: u32,
) -> i32 {
    guard(|| {
        let command = match code {
            1 => TrackCommand::Start,
            2 => TrackCommand::Pause,
            3 => TrackCommand::Resume,
            4 => TrackCommand::Disconnect,
            5 => TrackCommand::ReconnectConfirmed,
            6 => TrackCommand::Stop,
            7 => TrackCommand::Finish,
            8 => TrackCommand::Interrupt,
            _ => return Err(Error::InvalidArgument),
        };
        let confirmed = if length == 0 {
            None
        } else {
            Some(unsafe { endpoint(data, length) }?)
        };
        let at_ns = u64::try_from(at_ns).map_err(|_| Error::InvalidArgument)?;
        handles()?
            .tracks
            .get_mut(&handle)
            .ok_or(Error::UnknownHandle)?
            .apply(command, at_ns, confirmed)
            .map(|state| state as i32)
            .map_err(|error| match error {
                TrackTransitionError::InvalidTransition => Error::InvalidTransition,
                TrackTransitionError::InvalidConfirmation
                | TrackTransitionError::TimeWentBackwards => Error::InvalidArgument,
                TrackTransitionError::EpochOverflow => Error::Internal,
            })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn vl_track_source_epoch_v1(handle: i64) -> i64 {
    guard(|| {
        handles()?
            .tracks
            .get(&handle)
            .map(|track| i64::from(track.source_epoch()))
            .ok_or(Error::UnknownHandle)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn vl_track_release_v1(handle: i64) -> i32 {
    guard(|| {
        handles()?
            .tracks
            .remove(&handle)
            .map(|_| 0)
            .ok_or(Error::UnknownHandle)
    })
}
