use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[repr(i32)]
#[serde(rename_all = "snake_case")]
pub enum TrackState {
    Preparing = 0,
    Recording = 1,
    Paused = 2,
    Disconnected = 3,
    Stopping = 4,
    Completed = 5,
    Interrupted = 6,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackCommand {
    Start,
    Pause,
    Resume,
    Disconnect,
    ReconnectConfirmed,
    Stop,
    Finish,
    Interrupt,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TrackTransitionError {
    InvalidTransition,
    InvalidConfirmation,
    TimeWentBackwards,
    EpochOverflow,
}

pub struct TrackMachine {
    endpoint_id: String,
    state: TrackState,
    source_epoch: u32,
    last_change_ns: u64,
}

impl TrackMachine {
    pub fn new(endpoint_id: impl Into<String>) -> Self {
        Self {
            endpoint_id: endpoint_id.into(),
            state: TrackState::Preparing,
            source_epoch: 0,
            last_change_ns: 0,
        }
    }

    pub fn state(&self) -> TrackState {
        self.state
    }

    pub fn source_epoch(&self) -> u32 {
        self.source_epoch
    }

    pub fn apply(
        &mut self,
        command: TrackCommand,
        at_ns: u64,
        confirmed_endpoint: Option<&str>,
    ) -> Result<TrackState, TrackTransitionError> {
        use TrackCommand::*;
        use TrackState::*;

        if !matches!(command, ReconnectConfirmed) && confirmed_endpoint.is_some() {
            return Err(TrackTransitionError::InvalidConfirmation);
        }
        let to = match (self.state, command) {
            (Preparing, Start) | (Paused, Resume) | (Disconnected, ReconnectConfirmed) => Recording,
            (Recording, Pause) => Paused,
            (Preparing | Recording | Paused, Disconnect) => Disconnected,
            (Preparing | Recording | Paused | Disconnected, Stop) => Stopping,
            (Stopping | Completed | Interrupted, Stop) => self.state,
            (Stopping, Finish) => Completed,
            (Preparing | Recording | Paused | Disconnected | Stopping, Interrupt) => Interrupted,
            _ => return Err(TrackTransitionError::InvalidTransition),
        };
        if to == self.state {
            return Ok(to);
        }
        if at_ns < self.last_change_ns {
            return Err(TrackTransitionError::TimeWentBackwards);
        }
        if command == ReconnectConfirmed {
            if confirmed_endpoint != Some(self.endpoint_id.as_str()) {
                return Err(TrackTransitionError::InvalidConfirmation);
            }
            self.source_epoch = self
                .source_epoch
                .checked_add(1)
                .ok_or(TrackTransitionError::EpochOverflow)?;
        }
        self.state = to;
        self.last_change_ns = at_ns;
        Ok(to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_device_change_without_changing_the_track_or_epoch() {
        let mut track = TrackMachine::new("selected-endpoint");
        track.apply(TrackCommand::Start, 10, None).unwrap();
        track.apply(TrackCommand::Disconnect, 20, None).unwrap();
        assert_eq!(
            track.apply(TrackCommand::ReconnectConfirmed, 30, Some("other-endpoint")),
            Err(TrackTransitionError::InvalidConfirmation)
        );
        assert_eq!(track.state(), TrackState::Disconnected);
        assert_eq!(track.source_epoch(), 0);
        assert_eq!(
            track.apply(
                TrackCommand::ReconnectConfirmed,
                30,
                Some("selected-endpoint")
            ),
            Ok(TrackState::Recording)
        );
        assert_eq!(track.source_epoch(), 1);
    }
}
