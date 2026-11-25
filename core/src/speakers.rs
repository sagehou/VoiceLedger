use crate::contracts::{Authorship, SpeakerAssignment};

/// The threshold comes from the corpus evaluation for the embedding model in use.
pub struct SpeakerClusters {
    threshold: f64,
    centers: Vec<Vec<f64>>,
}

impl SpeakerClusters {
    pub fn new(threshold: f64) -> Option<Self> {
        (0.0..=1.0).contains(&threshold).then(|| Self {
            threshold,
            centers: Vec::new(),
        })
    }

    /// Returns a session-local cluster index, or Unknown for an ambiguous embedding.
    pub fn observe(&mut self, embedding: &[f32]) -> Option<usize> {
        if !(1..=1024).contains(&embedding.len())
            || self
                .centers
                .first()
                .is_some_and(|center| center.len() != embedding.len())
        {
            return None;
        }
        let values: Vec<_> = embedding.iter().map(|value| f64::from(*value)).collect();
        let norm = values.iter().map(|value| value * value).sum::<f64>().sqrt();
        if !norm.is_finite() || norm == 0.0 {
            return None;
        }
        let unit: Vec<_> = values.iter().map(|value| value / norm).collect();
        let mut matched = None;
        for (index, center) in self.centers.iter().enumerate() {
            let size = center.iter().map(|value| value * value).sum::<f64>().sqrt();
            let score = center.iter().zip(&unit).map(|(a, b)| a * b).sum::<f64>() / size;
            if score >= self.threshold && matched.replace(index).is_some() {
                return None;
            }
        }
        if let Some(index) = matched {
            for (value, added) in self.centers[index].iter_mut().zip(unit) {
                *value += added;
            }
            Some(index)
        } else if self.centers.len() < 64 {
            let index = self.centers.len();
            self.centers.push(unit);
            Some(index)
        } else {
            None
        }
    }
}

pub fn selects_assignment(previous: &SpeakerAssignment, proposal: &SpeakerAssignment) -> bool {
    previous.id != proposal.id
        && previous.session_id == proposal.session_id
        && previous.track_id == proposal.track_id
        && previous.range.start_ns == proposal.range.start_ns
        && previous.range.end_ns == proposal.range.end_ns
        && previous.revision < proposal.revision
        && previous
            .authorship
            .allows_replacement_by(proposal.authorship)
        && (proposal.authorship == Authorship::Manual
            || (proposal.confirmed_name.is_none() && proposal.confirmed_profile_id.is_none()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::DomainBatch;

    #[test]
    fn clustering_uses_session_local_labels_and_rejects_ambiguous_vectors() {
        let mut clusters = SpeakerClusters::new(0.7).unwrap();
        assert_eq!(clusters.observe(&[1.0, 0.0]), Some(0));
        assert_eq!(clusters.observe(&[0.0, 1.0]), Some(1));
        assert_eq!(clusters.observe(&[1.0, 1.0]), None);
        assert_eq!(clusters.observe(&[0.0, 0.0]), None);
        assert_eq!(clusters.observe(&[f32::NAN, 1.0]), None);
        assert_eq!(clusters.observe(&[0.98, 0.02]), Some(0));

        let mut second = SpeakerClusters::new(0.7).unwrap();
        assert_eq!(second.observe(&[0.0, 1.0]), Some(0));
    }

    #[test]
    fn machine_proposal_cannot_select_over_confirmed_identity() {
        let mut batch: DomainBatch =
            serde_json::from_str(include_str!("../../contracts/fixtures/domain-v1.json")).unwrap();
        let confirmed = batch.speaker_assignments.pop().unwrap();
        assert_eq!(confirmed.authorship, Authorship::Manual);
        let mut candidate = SpeakerAssignment {
            id: "c549475b-191c-4e5f-9e0c-a3669f6c5fb2"
                .to_owned()
                .try_into()
                .unwrap(),
            revision: (confirmed.revision.get() + 1).try_into().unwrap(),
            session_id: confirmed.session_id.clone(),
            track_id: confirmed.track_id.clone(),
            range: confirmed.range,
            speaker_id: None,
            authorship: Authorship::Machine,
            confidence: None,
            model: batch
                .utterance_revisions
                .into_iter()
                .find_map(|revision| revision.model),
            confirmed_name: None,
            confirmed_profile_id: None,
        };
        assert!(!selects_assignment(&confirmed, &candidate));
        candidate.authorship = Authorship::Manual;
        candidate.speaker_id = confirmed.speaker_id.clone();
        candidate.model = None;
        candidate.confirmed_name = Some("Corrected name".to_owned());
        assert!(selects_assignment(&confirmed, &candidate));
    }
}
