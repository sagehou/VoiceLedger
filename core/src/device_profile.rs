use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use serde_json::{Value, json};

use crate::contracts::{ContractError, DeviceCapabilityProfile};

pub const MAX_BYTES: usize = 16 * 1024;
const PROFILE_FILE: &str = "device-profile-v1.json";

#[derive(Debug)]
pub enum Error {
    Contract(ContractError),
    Storage,
}

pub fn save(directory: &Path, measurement: &[u8]) -> Result<(), Error> {
    let invalid_json = |_| Error::Contract(ContractError::InvalidJson);
    let mut measurement: Value = serde_json::from_slice(measurement).map_err(invalid_json)?;
    let fields = measurement
        .as_object_mut()
        .ok_or(Error::Contract(ContractError::InvalidJson))?;
    fields.insert("abi".into(), json!(abi()));
    fields.insert("cpu_features".into(), json!(cpu_features()));
    let profile: DeviceCapabilityProfile =
        serde_json::from_value(measurement).map_err(invalid_json)?;
    profile.validate().map_err(Error::Contract)?;
    write_profile(directory, profile).map_err(|_| Error::Storage)
}

fn write_profile(directory: &Path, mut profile: DeviceCapabilityProfile) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("device-profile.lock"))?;
    lock.lock()?;

    let path = directory.join(PROFILE_FILE);
    if let Some(previous) = read_profile(&path)?
        && previous.platform == profile.platform
    {
        profile.device_id = previous.device_id;
    }

    let bytes = serde_json::to_vec(&profile).map_err(io::Error::other)?;
    let temporary = directory.join("device-profile-v1.partial");
    let mut file = File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)
}

fn read_profile(path: &Path) -> io::Result<Option<DeviceCapabilityProfile>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut data = Vec::new();
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut data)?;
    if data.len() > MAX_BYTES {
        return Ok(None);
    }
    let profile = serde_json::from_slice::<DeviceCapabilityProfile>(&data).ok();
    Ok(profile.filter(|profile| profile.validate().is_ok()))
}

fn abi() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("android", "aarch64") => "arm64-v8a",
        (_, arch) => arch,
    }
}

fn cpu_features() -> Vec<&'static str> {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    let features = [
        ("sse2", std::is_x86_feature_detected!("sse2")),
        ("avx", std::is_x86_feature_detected!("avx")),
        ("avx2", std::is_x86_feature_detected!("avx2")),
        ("fma", std::is_x86_feature_detected!("fma")),
        ("avx512f", std::is_x86_feature_detected!("avx512f")),
    ];
    #[cfg(target_arch = "aarch64")]
    let features = [
        ("neon", std::arch::is_aarch64_feature_detected!("neon")),
        (
            "dotprod",
            std::arch::is_aarch64_feature_detected!("dotprod"),
        ),
    ];
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    let features: [(&str, bool); 0] = [];
    features
        .into_iter()
        .filter_map(|(feature, supported)| supported.then_some(feature))
        .collect()
}
