use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ureq::Agent;
use ureq::config::Config;
use ureq::http::{HeaderValue, StatusCode, Uri};
use voiceledger_audio::CommittedReader;
use voiceledger_core::contracts::{Language, LanguagePolicy, ModelRef, StableId};
use voiceledger_core::scheduler::{JobKind, Reason, ResourceSample, Tier};

use crate::jobs::Outcome;
use crate::pipeline::{self, Engine, Processing, Recognition};
use crate::transcript::{Transcripts, Writer};
use crate::{Error, Result, check_cancel};

pub(crate) const PROTOCOL: &str = "external-audio-transcriptions-v1";
const MAX_RESPONSE: u64 = 64 * 1024;

#[derive(Clone, Copy, Serialize)]
pub(crate) struct Provider {
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    model_id: &'static str,
}

pub(crate) const PROVIDERS: [Provider; 4] = [
    Provider {
        id: "openai",
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        model_id: "gpt-4o-mini-transcribe",
    },
    Provider {
        id: "groq",
        name: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        model_id: "whisper-large-v3-turbo",
    },
    Provider {
        id: "siliconflow",
        name: "SiliconFlow",
        base_url: "https://api.siliconflow.cn/v1",
        model_id: "FunAudioLLM/SenseVoiceSmall",
    },
    Provider {
        id: "custom",
        name: "Custom",
        base_url: "",
        model_id: "",
    },
];

pub(crate) const SUMMARY_PROVIDERS: [Provider; 4] = [
    Provider {
        model_id: "gpt-4o-mini",
        ..PROVIDERS[0]
    },
    Provider {
        model_id: "llama-3.3-70b-versatile",
        ..PROVIDERS[1]
    },
    Provider {
        model_id: "Qwen/Qwen2.5-7B-Instruct",
        ..PROVIDERS[2]
    },
    PROVIDERS[3],
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    provider: String,
    base_url: Option<String>,
    model_id: String,
    api_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Job {
    session_id: StableId,
    track_id: StableId,
    language: Language,
    settings: Settings,
    pub approved: bool,
}

impl Job {
    pub fn run(
        self,
        reader: &CommittedReader,
        recordings: &Path,
        cancelled: &(dyn Fn() -> bool + Sync),
        resource_sample: &(dyn Fn() -> Option<ResourceSample> + Sync),
    ) -> Result<u64> {
        let mut policy = |_| {
            if reader.recording()? {
                return Err(Error::Deferred(Reason::Recording));
            }
            if let Some(reason) = resource_sample().and_then(|sample| sample.emergency()) {
                return Err(Error::Deferred(reason));
            }
            Ok(Tier::Normal)
        };
        policy(None)?;
        check_cancel(cancelled)?;
        if crate::sync_data::finished(recordings, Some(reader), &self.session_id)?.is_none() {
            return Err(Error::NoAudio);
        }
        let track = reader
            .inputs(&self.session_id)?
            .into_iter()
            .find(|track| track.track_id == self.track_id)
            .ok_or(Error::NoAudio)?;
        let service = Service::connect(self.settings, &PROVIDERS, "/audio/transcriptions")?;
        let provenance = service.provenance(PROTOCOL)?;
        let mut client = Client {
            service,
            language: self.language,
        };
        let language = match self.language {
            Language::Zh => LanguagePolicy::Zh,
            Language::En => LanguagePolicy::En,
            Language::Ja => LanguagePolicy::Ja,
        };
        let store = Transcripts::open(recordings)?;
        let queue = store.queue();
        queue.recover()?;
        let job = queue.enqueue(
            store.id()?,
            &self.session_id,
            JobKind::Asr,
            &(PROTOCOL, &track, &provenance, language),
        )?;
        if !queue.begin(&[&job])? {
            return Ok(0);
        }

        let result = (|| {
            let start = queue
                .cursor(&job)?
                .map_or(track.start_ns, |cursor| cursor.next_ns);
            let mut writer = Writer {
                transcripts: &store,
                session: &self.session_id,
                model: &provenance,
                language,
                job: Some(&job),
                translate: None,
                publish_partial: &mut |_| {},
                saved: 0,
            };
            pipeline::process_saved_track(
                reader,
                &self.session_id,
                &track,
                start,
                &mut Processing {
                    engine: &mut client,
                    output: &mut writer,
                    language,
                    cancelled,
                    partials: false,
                    policy: &mut policy,
                },
            )?;
            queue.checkpoint(&job, &track.track_id, track.end_ns)?;
            Ok(writer.saved)
        })();
        queue.finish(&[&job], Outcome::for_result(&result))?;
        result
    }
}

struct Client {
    service: Service,
    language: Language,
}

pub(crate) struct Service {
    agent: Agent,
    pub provider: &'static str,
    endpoint: String,
    pub model_id: String,
    bearer: Option<HeaderValue>,
}

impl Service {
    pub fn connect(settings: Settings, presets: &'static [Provider], path: &str) -> Result<Self> {
        let provider = presets
            .iter()
            .find(|provider| provider.id == settings.provider)
            .ok_or(Error::ExternalConfiguration)?;
        let model_id = settings.model_id.trim();
        let key = settings.api_key.as_deref().unwrap_or("").trim();
        if model_id.is_empty()
            || model_id.len() > 192
            || !model_id.bytes().all(|byte| byte.is_ascii_graphic())
            || key.len() > 4096
            || !key.bytes().all(|byte| byte.is_ascii_graphic())
            || (provider.id != "custom" && key.is_empty())
        {
            return Err(Error::ExternalConfiguration);
        }
        let base_url = match (provider.id, settings.base_url.as_deref()) {
            ("custom", Some(url)) => url.trim(),
            ("custom", None) | (_, Some(_)) => return Err(Error::ExternalConfiguration),
            (_, None) => provider.base_url,
        }
        .trim_end_matches('/');
        let (endpoint, tls) = endpoint(base_url, path)?;
        let mut config = Config::builder()
            .https_only(tls)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)));
        if !tls {
            config = config.proxy(None);
        }
        let bearer = if key.is_empty() {
            None
        } else {
            let mut value = HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| Error::ExternalConfiguration)?;
            value.set_sensitive(true);
            Some(value)
        };
        Ok(Self {
            agent: config.build().into(),
            provider: provider.id,
            endpoint,
            model_id: model_id.to_owned(),
            bearer,
        })
    }

    pub fn provenance(&self, protocol: &str) -> Result<ModelRef> {
        let identity =
            serde_json::to_vec(&(protocol, self.provider, &self.endpoint, &self.model_id))?;
        Ok(ModelRef {
            model_id: format!("{}/{}", self.provider, self.model_id),
            model_version: "provider-managed".to_owned(),
            runtime_version: protocol.to_owned(),
            sha256: format!("{:x}", Sha256::digest(identity))
                .try_into()
                .map_err(|_| Error::Invalid)?,
        })
    }
    pub fn post(
        &self,
        content_type: &str,
        body: &[u8],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>> {
        let mut request = self
            .agent
            .post(self.endpoint.as_str())
            .header("Accept", "application/json")
            .header("Content-Type", content_type);
        if let Some(value) = &self.bearer {
            request = request.header("Authorization", value.clone());
        }
        check_cancel(cancelled)?;
        let mut response = request.send(body).map_err(|_| Error::ExternalConnection)?;
        match response.status() {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(Error::ExternalAuthentication);
            }
            StatusCode::TOO_MANY_REQUESTS => return Err(Error::ExternalRateLimit),
            _ => return Err(Error::ExternalResponse),
        }
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE)
            .read_to_vec()
            .map_err(|_| Error::ExternalResponse)?;
        check_cancel(cancelled)?;
        Ok(bytes)
    }
}

fn endpoint(base_url: &str, path: &str) -> Result<(String, bool)> {
    if base_url.len() > 2048 || base_url.contains(['#', '?']) {
        return Err(Error::ExternalConfiguration);
    }
    let uri: Uri = base_url.parse().map_err(|_| Error::ExternalConfiguration)?;
    let authority = uri.authority().ok_or(Error::ExternalConfiguration)?;
    if authority.as_str().contains('@') {
        return Err(Error::ExternalConfiguration);
    }
    let host = authority.host().trim_matches(['[', ']']);
    if host.is_empty() {
        return Err(Error::ExternalConfiguration);
    }
    let tls = match uri.scheme_str() {
        Some("https") => true,
        Some("http")
            if host.eq_ignore_ascii_case("localhost")
                || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) =>
        {
            false
        }
        _ => return Err(Error::ExternalConfiguration),
    };
    Ok((format!("{base_url}{path}"), tls))
}

#[derive(Deserialize)]
struct TextResponse {
    text: String,
}

impl Engine for Client {
    fn recognize(
        &mut self,
        samples: &[f32],
        _: LanguagePolicy,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<Recognition>> {
        check_cancel(cancelled)?;
        let waveform = wav(samples)?;
        let digest = format!("{:x}", Sha256::digest(&waveform));
        let boundary = format!("VoiceLedger-{}", &digest[..32]);
        if waveform
            .windows(boundary.len())
            .any(|bytes| bytes == boundary.as_bytes())
        {
            return Err(Error::ExternalResponse);
        }
        let mut body = Vec::with_capacity(waveform.len() + 1024);
        field(&mut body, &boundary, "model", &self.service.model_id)?;
        if self.service.provider != "siliconflow" {
            field(&mut body, &boundary, "language", self.language.code())?;
            field(&mut body, &boundary, "response_format", "json")?;
        }
        write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"segment.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )?;
        body.extend_from_slice(&waveform);
        write!(body, "\r\n--{boundary}--\r\n")?;

        let bytes = self.service.post(
            &format!("multipart/form-data; boundary={boundary}"),
            &body,
            cancelled,
        )?;
        let parsed: TextResponse =
            serde_json::from_slice(&bytes).map_err(|_| Error::ExternalResponse)?;
        let text = parsed.text.trim();
        if text.len() > crate::translations::MAX_TEXT || text.contains('\0') {
            return Err(Error::ExternalResponse);
        }
        Ok((!text.is_empty()).then(|| Recognition {
            text: text.to_owned(),
            language: self.language,
        }))
    }
}

fn field(body: &mut Vec<u8>, boundary: &str, name: &str, value: &str) -> Result<()> {
    write!(
        body,
        "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
    )?;
    Ok(())
}

fn wav(samples: &[f32]) -> Result<Vec<u8>> {
    if !(1..=pipeline::MAX_WINDOW_SAMPLES).contains(&samples.len()) {
        return Err(Error::ExternalResponse);
    }
    let rate = pipeline::RATE as u32;
    let length = (samples.len() * std::mem::size_of::<i16>()) as u32;
    let mut output = Vec::with_capacity(44 + length as usize);
    output.extend_from_slice(b"RIFF");
    output.extend_from_slice(&(36 + length).to_le_bytes());
    output.extend_from_slice(b"WAVEfmt ");
    output.extend_from_slice(&16u32.to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes());
    output.extend_from_slice(&rate.to_le_bytes());
    output.extend_from_slice(&(rate * 2).to_le_bytes());
    output.extend_from_slice(&2u16.to_le_bytes());
    output.extend_from_slice(&16u16.to_le_bytes());
    output.extend_from_slice(b"data");
    output.extend_from_slice(&length.to_le_bytes());
    for sample in samples {
        output.extend_from_slice(&((*sample * 32768.0).round() as i16).to_le_bytes());
    }
    Ok(output)
}
