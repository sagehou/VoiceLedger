use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::contracts::{Platform, Sha256};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    RecordingOnly,
    Lightweight,
    Balanced,
    Enhanced,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    pub id: String,
    pub version: String,
    pub source_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct License {
    pub id: String,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub platform: Platform,
    pub abi: String,
    pub min_os: u32,
    pub cpu_features: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub file: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: Sha256,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEstimate {
    pub bytes: u64,
    pub source_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub version: String,
    pub mode: Mode,
    pub quantization: String,
    pub languages: Vec<String>,
    pub runtime: Runtime,
    pub license: License,
    pub targets: Vec<Target>,
    pub artifacts: Vec<Artifact>,
    pub peak_ram_estimate: Option<MemoryEstimate>,
}

impl Model {
    pub fn download_bytes(&self) -> u64 {
        self.artifacts
            .iter()
            .map(|artifact| artifact.size_bytes)
            .sum()
    }

    pub(super) fn valid(&self) -> bool {
        let root = format!("/resolve/{}/", self.version);
        let mut filenames = BTreeSet::new();
        self.mode != Mode::RecordingOnly
            && key(&self.id)
            && self.version.len() == 40
            && self
                .version
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            && nonempty(&self.name)
            && nonempty(&self.quantization)
            && nonempty(&self.runtime.id)
            && nonempty(&self.runtime.version)
            && https(&self.runtime.source_url)
            && matches!(self.license.id.as_str(), "MIT" | "LicenseRef-FunASR-Model")
            && https(&self.license.url)
            && ["zh", "en", "ja"]
                .iter()
                .all(|language| self.languages.iter().any(|supported| supported == language))
            && !self.targets.is_empty()
            && self.targets.iter().all(|target| {
                nonempty(&target.abi)
                    && target.min_os > 0
                    && target.cpu_features.iter().all(|feature| key(feature))
            })
            && (1..=8).contains(&self.artifacts.len())
            && self.artifacts.iter().all(|artifact| {
                file_name(&artifact.file)
                    && filenames.insert(artifact.file.to_ascii_lowercase())
                    && artifact.url.starts_with("https://huggingface.co/")
                    && https(&artifact.url)
                    && artifact
                        .url
                        .ends_with(&format!("{}{}", root, artifact.file))
                    && (1..=8 * 1024 * 1024 * 1024).contains(&artifact.size_bytes)
            })
            && self
                .peak_ram_estimate
                .as_ref()
                .is_none_or(|estimate| estimate.bytes > 0 && https(&estimate.source_url))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    schema_version: u32,
    models: Vec<Model>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CatalogError {
    InvalidJson,
    InvalidCatalog,
}

impl Catalog {
    pub fn bundled() -> Result<Self, CatalogError> {
        Self::decode(include_bytes!("../../../models/catalog-v1.json"))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, CatalogError> {
        let catalog: Self = serde_json::from_slice(bytes).map_err(|_| CatalogError::InvalidJson)?;
        let mut identities = BTreeSet::new();
        if catalog.schema_version == 1
            && !catalog.models.is_empty()
            && catalog
                .models
                .iter()
                .all(|model| model.valid() && identities.insert(&model.id))
        {
            Ok(catalog)
        } else {
            Err(CatalogError::InvalidCatalog)
        }
    }

    pub fn models(&self) -> &[Model] {
        &self.models
    }
}

fn nonempty(value: &str) -> bool {
    !value.trim().is_empty()
}

fn key(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn file_name(value: &str) -> bool {
    let name = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let reserved = matches!(name.as_str(), "con" | "nul" | "prn" | "aux")
        || ((name.starts_with("com") || name.starts_with("lpt"))
            && name.len() == 4
            && name.as_bytes()[3].is_ascii_digit());
    !reserved
        && value.starts_with(|c: char| c.is_ascii_alphanumeric())
        && value.ends_with(|c: char| c.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn https(value: &str) -> bool {
    value.starts_with("https://") && value.len() > 8 && value.bytes().all(|b| b.is_ascii_graphic())
}
