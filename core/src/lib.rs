use serde::{Deserialize, Serialize};

pub mod contracts;
pub mod device_profile;
pub mod models;
pub mod scheduler;
pub mod speakers;
pub mod track;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[repr(i32)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Idle = 0,
    Preparing = 1,
    Recording = 2,
    Paused = 3,
    Stopping = 4,
    Completed = 5,
    Interrupted = 6,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCommand {
    Prepare,
    Start,
    Pause,
    Resume,
    Stop,
    Finish,
    Interrupt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    pub from: SessionState,
    pub to: SessionState,
    pub reason: SessionCommand,
    pub at_ns: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TransitionError {
    InvalidTransition,
    TimeWentBackwards,
}

#[derive(Default)]
pub struct Session {
    last_transition: Option<Transition>,
}

impl Session {
    pub fn state(&self) -> SessionState {
        self.last_transition
            .map_or(SessionState::Idle, |step| step.to)
    }

    pub fn last_transition(&self) -> Option<Transition> {
        self.last_transition
    }

    pub fn apply(
        &mut self,
        command: SessionCommand,
        at_ns: u64,
    ) -> Result<SessionState, TransitionError> {
        use SessionCommand::*;
        use SessionState::*;

        let from = self.state();
        let to = match (from, command) {
            (Idle, Prepare) => Preparing,
            (Preparing, Start) | (Paused, Resume) => Recording,
            (Recording, Pause) => Paused,
            (Preparing | Recording | Paused, Stop) => Stopping,
            (Idle | Stopping | Completed | Interrupted, Stop) => from,
            (Stopping, Finish) => Completed,
            (Preparing | Recording | Paused | Stopping, Interrupt) => Interrupted,
            _ => return Err(TransitionError::InvalidTransition),
        };
        if from != to {
            if self.last_transition.is_some_and(|step| at_ns < step.at_ns) {
                return Err(TransitionError::TimeWentBackwards);
            }
            self.last_transition = Some(Transition {
                from,
                to,
                reason: command,
                at_ns,
            });
        }
        Ok(to)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compatibility {
    Unknown,
    Incompatible,
    Compatible,
}

pub struct RecommendationInput {
    pub compatibility: Compatibility,
    pub memory_budget_bytes: Option<u64>,
    pub peak_memory_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Recommendation {
    RecordingOnly = 0,
    CandidateForBenchmark = 1,
}

pub fn recommend(input: RecommendationInput) -> Recommendation {
    match (
        input.compatibility,
        input.memory_budget_bytes,
        input.peak_memory_bytes,
    ) {
        (Compatibility::Compatible, Some(budget), Some(peak)) if peak > 0 && peak <= budget => {
            Recommendation::CandidateForBenchmark
        }
        _ => Recommendation::RecordingOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }

    #[test]
    fn records_reason_and_monotonic_time_once() {
        let mut session = Session::default();
        assert_eq!(
            session.apply(SessionCommand::Prepare, 10),
            Ok(SessionState::Preparing)
        );
        assert_eq!(
            session.apply(SessionCommand::Start, 9),
            Err(TransitionError::TimeWentBackwards)
        );
        assert_eq!(
            session.apply(SessionCommand::Stop, 20),
            Ok(SessionState::Stopping)
        );
        assert_eq!(
            session.apply(SessionCommand::Stop, 30),
            Ok(SessionState::Stopping)
        );
        assert_eq!(
            session.last_transition(),
            Some(Transition {
                from: SessionState::Preparing,
                to: SessionState::Stopping,
                reason: SessionCommand::Stop,
                at_ns: 20,
            })
        );
    }
}
