use std::collections::VecDeque;

use serde::Serialize;
use voiceledger_audio::CommittedChunk;
use voiceledger_core::contracts::{
    ChunkSpan, Count, Language, LanguagePolicy, StableId, TimeRange,
};
use voiceledger_core::scheduler::Tier;
use webrtc_vad::{SampleRate, Vad, VadMode};

use crate::{Error, Result};

pub const RATE: usize = 16_000;
const FRAME: usize = RATE / 50;
const PREROLL: usize = 8;
const TAIL: usize = 12;
const WINDOW: usize = RATE * 12;
const FIRST_PARTIAL: usize = RATE * 3 / 2;

pub struct Recognition {
    pub text: String,
    pub language: Language,
}

pub struct StreamText {
    pub text: String,
    pub language: Option<Language>,
}

impl StreamText {
    pub fn confirmed(self) -> Option<Recognition> {
        self.language.map(|language| Recognition {
            text: self.text,
            language,
        })
    }
}

pub trait Engine {
    fn recognize(
        &mut self,
        samples: &[f32],
        language: LanguagePolicy,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<Recognition>>;

    fn streams(&self) -> bool {
        false
    }

    fn stream_step(
        &mut self,
        _samples: &[f32],
        _finish: bool,
        _cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<StreamText>> {
        Err(Error::Runtime)
    }
}

#[derive(Clone, Serialize)]
pub struct Partial {
    pub id: StableId,
    pub track_id: StableId,
    pub revision: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
    pub text: String,
}

pub struct Final {
    pub id: StableId,
    pub track_id: StableId,
    pub spans: Vec<ChunkSpan>,
    pub range: TimeRange,
    pub recognition: Recognition,
}

pub trait Output {
    fn id(&mut self) -> Result<StableId>;
    fn partial(&mut self, value: Option<Partial>) -> Result<()>;
    fn finish(&mut self, value: Final) -> Result<()>;
}

struct Frame {
    pcm: Vec<f32>,
    span: ChunkSpan,
}

struct Window {
    id: StableId,
    pcm: Vec<f32>,
    spans: Vec<ChunkSpan>,
    quiet: usize,
    streamed: usize,
    last_partial: usize,
    revision: u32,
}

impl Window {
    fn push(&mut self, frame: Frame) {
        self.pcm.extend(frame.pcm);
        if let Some(span) = self.spans.last_mut()
            && span.chunk_id == frame.span.chunk_id
            && span.range.end_ns == frame.span.range.start_ns
        {
            span.range.end_ns = frame.span.range.end_ns;
        } else {
            self.spans.push(frame.span);
        }
    }

    fn show(
        &mut self,
        track_id: &StableId,
        value: StreamText,
        output: &mut dyn Output,
    ) -> Result<()> {
        self.revision = self.revision.saturating_add(1);
        output.partial(Some(Partial {
            id: self.id.clone(),
            track_id: track_id.clone(),
            revision: self.revision,
            language: value.language,
            text: value.text,
        }))
    }
}

pub struct Segmenter {
    track: Option<(StableId, u32, i64)>,
    vad: Vad,
    preroll: VecDeque<Frame>,
    window: Option<Window>,
}

pub struct Processing<'a> {
    pub engine: &'a mut dyn Engine,
    pub output: &'a mut dyn Output,
    pub language: LanguagePolicy,
    pub cancelled: &'a (dyn Fn() -> bool + Sync),
    pub partials: bool,
    pub policy: &'a mut dyn FnMut(Option<f64>) -> Result<Tier>,
}

impl Segmenter {
    pub fn new() -> Self {
        Self {
            track: None,
            vad: detector(),
            preroll: VecDeque::new(),
            window: None,
        }
    }

    pub fn ingest(
        &mut self,
        chunk: CommittedChunk,
        from_ns: i64,
        processing: &mut Processing<'_>,
    ) -> Result<()> {
        crate::check_cancel(processing.cancelled)?;
        if self.track.as_ref().is_some_and(|(track, epoch, end)| {
            track != &chunk.track_id || *epoch != chunk.epoch || *end != chunk.start_ns
        }) {
            self.flush(processing)?;
            self.preroll.clear();
            self.vad = detector();
        }
        self.track = Some((chunk.track_id.clone(), chunk.epoch, chunk.end_ns));
        let Some(samples) = chunk.samples else {
            self.flush(processing)?;
            self.track = None;
            self.preroll.clear();
            self.vad = detector();
            return Ok(());
        };
        let normalized = normalize(&samples, chunk.channels)?;
        for (index, pcm) in normalized.chunks(FRAME).enumerate() {
            crate::check_cancel(processing.cancelled)?;
            let start = index * FRAME;
            let range = mapped_range(
                chunk.start_ns,
                chunk.end_ns,
                start,
                start + pcm.len(),
                normalized.len(),
            )?;
            if range.start_ns == range.end_ns || range.end_ns.get() <= from_ns as u64 {
                continue;
            }
            let mut input = [0_i16; FRAME];
            for (output, value) in input.iter_mut().zip(pcm) {
                *output = (value.clamp(-1.0, 1.0) * 32767.0) as i16;
            }
            let voiced = self
                .vad
                .is_voice_segment(&input)
                .map_err(|()| Error::Invalid)?;
            let frame = Frame {
                pcm: pcm.to_vec(),
                span: ChunkSpan {
                    chunk_id: chunk.id.clone(),
                    range,
                },
            };
            self.accept(frame, voiced, processing)?;
        }
        self.preview(processing)
    }

    fn accept(
        &mut self,
        frame: Frame,
        voiced: bool,
        processing: &mut Processing<'_>,
    ) -> Result<()> {
        if self.window.is_none() {
            if !voiced {
                self.preroll.push_back(frame);
                if self.preroll.len() > PREROLL {
                    self.preroll.pop_front();
                }
                return Ok(());
            }
            let mut window = Window {
                id: processing.output.id()?,
                pcm: Vec::new(),
                spans: Vec::new(),
                quiet: 0,
                streamed: 0,
                last_partial: 0,
                revision: 0,
            };
            for buffered in self.preroll.drain(..) {
                window.push(buffered);
            }
            self.window = Some(window);
        }
        let window = self.window.as_mut().ok_or(Error::Invalid)?;
        window.push(frame);
        window.quiet = if voiced { 0 } else { window.quiet + 1 };
        if window.pcm.len() >= WINDOW || window.quiet >= TAIL {
            return self.flush(processing);
        }
        Ok(())
    }

    fn preview(&mut self, processing: &mut Processing<'_>) -> Result<()> {
        let Some(window) = self.window.as_mut() else {
            return Ok(());
        };
        let track_id = &self.track.as_ref().ok_or(Error::Invalid)?.0;
        if processing.engine.streams() {
            let tier = (processing.policy)(None)?;
            let value = stream(window, processing, false)?;
            // Publishing a cached prefix does not rerun the encoder.
            let due = window.revision == 0
                || window.pcm.len() - window.last_partial >= tier.partial_seconds() * RATE;
            if processing.partials
                && due
                && let Some(value) = value
            {
                window.last_partial = window.pcm.len();
                window.show(track_id, value, processing.output)?;
            }
            return Ok(());
        }
        if !processing.partials || window.pcm.len() < FIRST_PARTIAL {
            return Ok(());
        }
        let tier = (processing.policy)(None)?;
        let due = if window.last_partial == 0 && tier < Tier::ReducePartials {
            FIRST_PARTIAL
        } else {
            window.last_partial + tier.partial_seconds() * RATE
        };
        if window.pcm.len() >= due {
            window.last_partial = window.pcm.len();
            if let Some(value) = recognize(processing, &window.pcm)? {
                window.show(
                    track_id,
                    StreamText {
                        language: Some(value.language),
                        text: value.text,
                    },
                    processing.output,
                )?;
            }
        }
        Ok(())
    }

    pub fn flush(&mut self, processing: &mut Processing<'_>) -> Result<()> {
        let Some(mut window) = self.window.take() else {
            return Ok(());
        };
        let value = if processing.engine.streams() {
            stream(&mut window, processing, true).map(|text| text.and_then(StreamText::confirmed))
        } else {
            recognize(processing, &window.pcm)
        };
        if window.revision != 0 {
            processing.output.partial(None)?;
        }
        if let Some(recognition) = value? {
            let range = TimeRange {
                start_ns: window.spans.first().ok_or(Error::Invalid)?.range.start_ns,
                end_ns: window.spans.last().ok_or(Error::Invalid)?.range.end_ns,
            };
            processing.output.finish(Final {
                id: window.id,
                track_id: self.track.as_ref().ok_or(Error::Invalid)?.0.clone(),
                spans: window.spans,
                range,
                recognition,
            })?;
        }
        Ok(())
    }
}

fn stream(
    window: &mut Window,
    processing: &mut Processing<'_>,
    finish: bool,
) -> Result<Option<StreamText>> {
    let samples = &window.pcm[window.streamed..];
    if samples.is_empty() && !finish {
        return Ok(None);
    }
    crate::check_cancel(processing.cancelled)?;
    (processing.policy)(None)?;
    let started = std::time::Instant::now();
    let value = processing
        .engine
        .stream_step(samples, finish, processing.cancelled)?;
    crate::check_cancel(processing.cancelled)?;
    let rtf = (!samples.is_empty())
        .then(|| started.elapsed().as_secs_f64() * RATE as f64 / samples.len() as f64);
    (processing.policy)(rtf)?;
    window.streamed = window.pcm.len();
    Ok(value)
}

fn recognize(processing: &mut Processing<'_>, samples: &[f32]) -> Result<Option<Recognition>> {
    crate::check_cancel(processing.cancelled)?;
    (processing.policy)(None)?;
    let started = std::time::Instant::now();
    let value = processing
        .engine
        .recognize(samples, processing.language, processing.cancelled)?;
    crate::check_cancel(processing.cancelled)?;
    (processing.policy)(Some(
        started.elapsed().as_secs_f64() * RATE as f64 / samples.len() as f64,
    ))?;
    Ok(value.filter(|value| !value.text.trim().is_empty()))
}

fn detector() -> Vad {
    Vad::new_with_rate_and_mode(SampleRate::Rate16kHz, VadMode::Aggressive)
}

fn normalize(samples: &[f32], channels: usize) -> Result<Vec<f32>> {
    if channels == 0 || !samples.len().is_multiple_of(channels) {
        return Err(Error::Invalid);
    }
    Ok(samples
        .chunks(3 * channels)
        .map(|block| block.iter().copied().sum::<f32>() / block.len() as f32)
        .collect())
}

fn mapped_range(start: i64, end: i64, from: usize, to: usize, length: usize) -> Result<TimeRange> {
    let time = |offset: usize| -> Result<Count> {
        let delta = i128::from(end) - i128::from(start);
        let ns = i128::from(start) + delta * offset as i128 / length as i128;
        i64::try_from(ns)
            .ok()
            .and_then(|ns| Count::try_from(ns).ok())
            .ok_or(Error::Invalid)
    };
    Ok(TimeRange {
        start_ns: time(from)?,
        end_ns: time(to)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid(last: u32) -> StableId {
        format!("00000000-0000-4000-8000-{last:012}")
            .try_into()
            .unwrap()
    }

    struct MockEngine;
    impl Engine for MockEngine {
        fn recognize(
            &mut self,
            samples: &[f32],
            _: LanguagePolicy,
            _: &(dyn Fn() -> bool + Sync),
        ) -> Result<Option<Recognition>> {
            Ok(Some(Recognition {
                text: samples.len().to_string(),
                language: Language::Zh,
            }))
        }
    }

    #[derive(Default)]
    struct Sink {
        updates: Vec<Option<Partial>>,
        results: Vec<Final>,
    }
    impl Output for Sink {
        fn id(&mut self) -> Result<StableId> {
            Ok(uid(30))
        }
        fn partial(&mut self, value: Option<Partial>) -> Result<()> {
            self.updates.push(value);
            Ok(())
        }
        fn finish(&mut self, value: Final) -> Result<()> {
            self.results.push(value);
            Ok(())
        }
    }

    fn voiced_frame(frame: i64, chunk: StableId) -> Frame {
        Frame {
            pcm: vec![0.5; FRAME],
            span: ChunkSpan {
                chunk_id: chunk,
                range: TimeRange {
                    start_ns: (frame * 20_000_000).try_into().unwrap(),
                    end_ns: ((frame + 1) * 20_000_000).try_into().unwrap(),
                },
            },
        }
    }

    #[test]
    fn partial_revisions_share_one_id_and_final_continues_chunk_provenance() {
        let track = uid(1);
        let mut segmenter = Segmenter::new();
        segmenter.track = Some((track.clone(), 0, 0));
        let mut sink = Sink::default();
        let mut processing = Processing {
            engine: &mut MockEngine,
            output: &mut sink,
            language: LanguagePolicy::Auto,
            cancelled: &|| false,
            partials: true,
            policy: &mut |_| Ok(Tier::Normal),
        };
        for frame in 0..325_i64 {
            let chunk = if frame < 200 { uid(2) } else { uid(3) };
            segmenter
                .accept(voiced_frame(frame, chunk), true, &mut processing)
                .unwrap();
            if frame % 100 == 99 {
                segmenter.preview(&mut processing).unwrap();
            }
        }
        segmenter.flush(&mut processing).unwrap();
        assert_eq!(sink.updates.len(), 3);
        let first = sink.updates[0].as_ref().unwrap();
        let second = sink.updates[1].as_ref().unwrap();
        assert_eq!(first.text, (2 * RATE).to_string());
        assert_eq!(first.id, second.id);
        assert_eq!((first.revision, second.revision), (1, 2));
        assert!(sink.updates[2].is_none());
        let result = &sink.results[0];
        assert_eq!(result.id, first.id);
        assert_eq!(result.track_id, track);
        assert_eq!(result.spans.len(), 2);
        assert_eq!(result.spans[0].chunk_id, uid(2));
        assert_eq!(result.spans[1].chunk_id, uid(3));
        assert_eq!(result.range.end_ns.get(), 6_500_000_000);
    }

    #[test]
    fn streaming_feeds_each_sample_once_and_finalizes_with_the_emitted_language() {
        #[derive(Default)]
        struct Stream(Vec<(usize, bool)>);

        impl Engine for Stream {
            fn recognize(
                &mut self,
                _: &[f32],
                _: LanguagePolicy,
                _: &(dyn Fn() -> bool + Sync),
            ) -> Result<Option<Recognition>> {
                panic!("Streaming must not re-decode the cumulative window");
            }

            fn streams(&self) -> bool {
                true
            }

            fn stream_step(
                &mut self,
                samples: &[f32],
                finish: bool,
                _: &(dyn Fn() -> bool + Sync),
            ) -> Result<Option<StreamText>> {
                self.0.push((samples.len(), finish));
                Ok(Some(StreamText {
                    language: finish.then_some(Language::Ja),
                    text: "こんにちは".into(),
                }))
            }
        }

        let mut engine = Stream::default();
        let mut sink = Sink::default();
        let mut segmenter = Segmenter::new();
        segmenter.track = Some((uid(1), 0, 0));
        let mut processing = Processing {
            engine: &mut engine,
            output: &mut sink,
            language: LanguagePolicy::Auto,
            cancelled: &|| false,
            partials: true,
            policy: &mut |_| Ok(Tier::Normal),
        };
        for index in 0..200_i64 {
            let chunk = uid(if index < 100 { 2 } else { 3 });
            segmenter
                .accept(voiced_frame(index, chunk), true, &mut processing)
                .unwrap();
            if index % 100 == 99 {
                segmenter.preview(&mut processing).unwrap();
            }
        }
        segmenter.flush(&mut processing).unwrap();
        segmenter
            .accept(voiced_frame(200, uid(4)), true, &mut processing)
            .unwrap();
        segmenter.flush(&mut processing).unwrap();

        assert_eq!(
            engine.0,
            [
                (2 * RATE, false),
                (2 * RATE, false),
                (0, true),
                (FRAME, true)
            ]
        );
        let partial = sink.updates[0].as_ref().unwrap();
        assert_eq!(partial.language, None);
        assert!(sink.updates[1].is_none());
        assert_eq!(sink.results[0].id, partial.id);
        assert_eq!(sink.results[0].recognition.language, Language::Ja);
        assert_eq!(sink.results[0].spans.len(), 2);
        assert_eq!(sink.results[1].spans[0].chunk_id, uid(4));
    }

    #[test]
    fn pressure_changes_partial_cadence_and_blocks_an_uncommitted_final() {
        use std::cell::Cell;
        use voiceledger_core::scheduler::Reason;

        let stopped = Cell::new(false);
        let mut sink = Sink::default();
        let mut segmenter = Segmenter::new();
        segmenter.track = Some((uid(1), 0, 0));
        let mut processing = Processing {
            engine: &mut MockEngine,
            output: &mut sink,
            language: LanguagePolicy::Auto,
            cancelled: &|| false,
            partials: true,
            policy: &mut |_| {
                if stopped.get() {
                    Err(Error::Deferred(Reason::Memory))
                } else {
                    Ok(Tier::ReducePartials)
                }
            },
        };
        for frame in 0..310_i64 {
            segmenter
                .accept(voiced_frame(frame, uid(2)), true, &mut processing)
                .unwrap();
            if frame % 100 == 99 {
                segmenter.preview(&mut processing).unwrap();
            }
        }
        stopped.set(true);
        assert!(matches!(
            segmenter.flush(&mut processing),
            Err(Error::Deferred(Reason::Memory))
        ));
        assert_eq!(sink.updates.len(), 2);
        assert_eq!(
            sink.updates[0].as_ref().unwrap().text,
            (6 * RATE).to_string()
        );
        assert!(sink.updates[1].is_none());
        assert!(sink.results.is_empty());
    }

    #[test]
    fn web_rtc_silence_does_not_emit_text() {
        let mut sink = Sink::default();
        let mut processing = Processing {
            engine: &mut MockEngine,
            output: &mut sink,
            language: LanguagePolicy::Auto,
            cancelled: &|| false,
            partials: true,
            policy: &mut |_| Ok(Tier::Normal),
        };
        let mut segmenter = Segmenter::new();
        segmenter
            .ingest(
                CommittedChunk {
                    id: uid(2),
                    track_id: uid(1),
                    epoch: 0,
                    start_ns: 0,
                    end_ns: 1_000_000_000,
                    channels: 2,
                    samples: Some(vec![0.0; 96_000]),
                },
                0,
                &mut processing,
            )
            .unwrap();
        segmenter.flush(&mut processing).unwrap();
        assert!(sink.results.is_empty());
        assert!(sink.updates.is_empty());
    }
}
