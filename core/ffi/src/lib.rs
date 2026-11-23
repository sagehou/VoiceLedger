use std::collections::BTreeMap;
use std::panic::{UnwindSafe, catch_unwind};
use std::sync::{Mutex, MutexGuard};

use voiceledger_core::contracts::DomainBatch;
use voiceledger_core::track::TrackMachine;
use voiceledger_core::{
    Compatibility, RecommendationInput, Session, SessionCommand, TransitionError,
};

#[cfg(any(target_os = "android", target_os = "linux"))]
mod jni;
mod track;

const MAX_DOMAIN_BATCH_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
#[repr(i32)]
enum Error {
    InvalidArgument = -1,
    UnknownHandle = -2,
    InvalidTransition = -3,
    Cancelled = -4,
    Internal = -5,
}

#[derive(Default)]
struct OwnedSession {
    session: Session,
    cancelled: bool,
}

struct Handles {
    last_id: i64,
    sessions: BTreeMap<i64, OwnedSession>,
    tracks: BTreeMap<i64, TrackMachine>,
}

static HANDLES: Mutex<Handles> = Mutex::new(Handles {
    last_id: 0,
    sessions: BTreeMap::new(),
    tracks: BTreeMap::new(),
});

impl Handles {
    fn next_id(&mut self) -> Result<i64, Error> {
        self.last_id = self.last_id.checked_add(1).ok_or(Error::Internal)?;
        Ok(self.last_id)
    }
}

fn handles() -> Result<MutexGuard<'static, Handles>, Error> {
    HANDLES.lock().map_err(|_| Error::Internal)
}

fn with_session(
    handle: i64,
    operation: impl FnOnce(&mut OwnedSession) -> Result<i32, Error>,
) -> Result<i32, Error> {
    let mut handles = handles()?;
    operation(
        handles
            .sessions
            .get_mut(&handle)
            .ok_or(Error::UnknownHandle)?,
    )
}

fn guard<T: From<i32>>(operation: impl FnOnce() -> Result<T, Error> + UnwindSafe) -> T {
    match catch_unwind(operation) {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => T::from(error as i32),
        Err(_) => T::from(Error::Internal as i32),
    }
}

fn command(code: i32) -> Result<SessionCommand, Error> {
    match code {
        1 => Ok(SessionCommand::Prepare),
        2 => Ok(SessionCommand::Start),
        3 => Ok(SessionCommand::Pause),
        4 => Ok(SessionCommand::Resume),
        5 => Ok(SessionCommand::Stop),
        6 => Ok(SessionCommand::Finish),
        7 => Ok(SessionCommand::Interrupt),
        _ => Err(Error::InvalidArgument),
    }
}

fn memory_bytes(value: i64) -> Result<Option<u64>, Error> {
    if value == -1 {
        return Ok(None);
    }
    u64::try_from(value)
        .map(Some)
        .map_err(|_| Error::InvalidArgument)
}

#[unsafe(no_mangle)]
pub extern "C" fn vl_abi_version_v1() -> i32 {
    1
}

fn validate_domain_bytes(bytes: &[u8]) -> i32 {
    DomainBatch::decode(bytes).map_or_else(|error| error as i32, |_| 0)
}

/// Validate one versioned UTF-8 domain batch without retaining the input buffer.
///
/// # Safety
/// A non-null `data` must be readable for `length` bytes throughout the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vl_domain_validate_v1(data: *const u8, length: u32) -> i32 {
    guard(|| {
        let length = length as usize;
        if data.is_null() || length == 0 || length > MAX_DOMAIN_BATCH_BYTES {
            return Err(Error::InvalidArgument);
        }
        let bytes = unsafe { std::slice::from_raw_parts(data, length) };
        Ok(validate_domain_bytes(bytes))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn vl_session_create_v1() -> i64 {
    guard(|| {
        let mut handles = handles()?;
        let id = handles.next_id()?;
        handles.sessions.insert(id, OwnedSession::default());
        Ok(id)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn vl_session_transition_v1(handle: i64, code: i32, at_ns: i64) -> i32 {
    guard(|| {
        let command = command(code)?;
        let at_ns = u64::try_from(at_ns).map_err(|_| Error::InvalidArgument)?;
        with_session(handle, |owned| {
            if owned.cancelled {
                return Err(Error::Cancelled);
            }
            owned
                .session
                .apply(command, at_ns)
                .map(|state| state as i32)
                .map_err(|error| match error {
                    TransitionError::InvalidTransition => Error::InvalidTransition,
                    TransitionError::TimeWentBackwards => Error::InvalidArgument,
                })
        })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn vl_session_cancel_v1(handle: i64) -> i32 {
    guard(|| {
        with_session(handle, |owned| {
            owned.cancelled = true;
            Ok(0)
        })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn vl_session_release_v1(handle: i64) -> i32 {
    guard(|| {
        handles()?
            .sessions
            .remove(&handle)
            .map(|_| 0)
            .ok_or(Error::UnknownHandle)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn vl_recommend_v1(compatibility: i32, budget_bytes: i64, peak_bytes: i64) -> i32 {
    guard(|| {
        let compatibility = match compatibility {
            0 => Compatibility::Unknown,
            1 => Compatibility::Incompatible,
            2 => Compatibility::Compatible,
            _ => return Err(Error::InvalidArgument),
        };
        Ok(voiceledger_core::recommend(RecommendationInput {
            compatibility,
            memory_budget_bytes: memory_bytes(budget_bytes)?,
            peak_memory_bytes: memory_bytes(peak_bytes)?,
        }) as i32)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_ownership_and_rejects_stale_handles() {
        for _ in 0..128 {
            let handle = vl_session_create_v1();
            assert!(handle > 0);
            assert_eq!(vl_session_transition_v1(handle, 1, 0), 1);
            assert_eq!(vl_session_cancel_v1(handle), 0);
            assert_eq!(vl_session_cancel_v1(handle), 0);
            assert_eq!(
                vl_session_transition_v1(handle, 2, 1),
                Error::Cancelled as i32
            );
            assert_eq!(vl_session_release_v1(handle), 0);
            assert_eq!(
                vl_session_transition_v1(handle, 2, 1),
                Error::UnknownHandle as i32
            );
            assert_eq!(vl_session_release_v1(handle), Error::UnknownHandle as i32);
        }
    }

    #[test]
    fn converts_an_unwinding_panic_to_an_error() {
        let result: i32 = guard(|| panic!("synthetic FFI smoke test panic"));
        assert_eq!(result, Error::Internal as i32);
    }
}
