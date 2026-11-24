mod catalog;
mod install;
mod policy;
mod qualification;

#[cfg(test)]
mod tests;

pub use catalog::*;
pub use install::*;
pub use policy::*;
pub use qualification::*;

use std::path::Path;

pub fn preview_json(profile_directory: &Path) -> Result<Vec<u8>, CatalogError> {
    let catalog = Catalog::bundled()?;
    let device = crate::device_profile::load(profile_directory)
        .ok()
        .flatten();
    serde_json::to_vec(&model_options(catalog.models(), device.as_ref(), &[]))
        .map_err(|_| CatalogError::InvalidJson)
}
