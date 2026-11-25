mod opus;
mod store;

pub use store::{
    Command, CommittedChunk, CommittedReader, FinishedSession, HistoryTrack, NewTrack, Response,
    SavedSession, Store, TrackInput,
};

pub const RATE: usize = 48_000;
pub const MAX_CHANNELS: usize = 32;
pub const MAX_FRAMES: usize = RATE * 10;
pub const MAX_CHUNK_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum Error {
    Invalid,
    InvalidMedia,
    Unavailable,
    Locked,
    UnsupportedSchema,
    Internal,
    Io(std::io::Error),
    Sql(rusqlite::Error),
    Json(serde_json::Error),
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sql(value)
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

type Result<T> = std::result::Result<T, Error>;
