use std::ffi::{CStr, c_char, c_void};
use std::path::Path;
use std::ptr::NonNull;

use libloading::Library;
use voiceledger_core::models::{Model, RuntimeIdentity, RuntimeProbe};

use crate::engine::{adapter_path, c_path, load_library};
use crate::translations::MAX_TEXT;
use crate::{Error, Result};

type Open = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type Run = unsafe extern "C" fn(*mut c_void, *const u8, u32, *mut u8, u32) -> i32;
type Close = unsafe extern "C" fn(*mut c_void);

pub(crate) struct TranslationAdapter {
    version: String,
    open: Open,
    run: Run,
    close: Close,
    _library: Library,
}

impl TranslationAdapter {
    pub(crate) fn load(model: &Model, native: &Path) -> Result<Self> {
        let path = adapter_path(&model.runtime.id, native).ok_or(Error::Runtime)?;
        let library = load_library(&path)?;
        unsafe {
            let version = library
                .get::<unsafe extern "C" fn() -> *const c_char>(b"vl_translation_version_v1")?(
            );
            if version.is_null()
                || CStr::from_ptr(version).to_str().ok() != Some(model.runtime.version.as_str())
            {
                return Err(Error::Runtime);
            }
            Ok(Self {
                version: model.runtime.version.clone(),
                open: *library.get(b"vl_translation_open_v1")?,
                run: *library.get(b"vl_translation_run_v1")?,
                close: *library.get(b"vl_translation_close_v1")?,
                _library: library,
            })
        }
    }

    pub(crate) fn engine(&self, directory: &Path) -> Result<Translator<'_>> {
        let path = c_path(directory)?;
        let context = unsafe { (self.open)(path.as_ptr()) };
        Ok(Translator {
            adapter: self,
            context: NonNull::new(context).ok_or(Error::Runtime)?,
        })
    }
}

impl RuntimeProbe for TranslationAdapter {
    fn identity(&self) -> RuntimeIdentity<'_> {
        RuntimeIdentity {
            id: "ctranslate2",
            version: &self.version,
        }
    }

    fn load_and_release(&self, _: &Model, directory: &Path, cancelled: &dyn Fn() -> bool) -> bool {
        !cancelled() && self.engine(directory).is_ok()
    }
}

pub(crate) struct Translator<'a> {
    adapter: &'a TranslationAdapter,
    context: NonNull<c_void>,
}

impl Translator<'_> {
    pub(crate) fn translate(&self, text: &str) -> Result<String> {
        if text.len() > MAX_TEXT {
            return Err(Error::TranslationTextLimit);
        }
        if text.is_empty() || text.contains('\0') {
            return Err(Error::Invalid);
        }
        let mut output = [0_u8; MAX_TEXT];
        let size = unsafe {
            (self.adapter.run)(
                self.context.as_ptr(),
                text.as_ptr(),
                text.len() as u32,
                output.as_mut_ptr(),
                output.len() as u32,
            )
        };
        if size == -2 {
            return Err(Error::TranslationTextLimit);
        }
        let size = usize::try_from(size).map_err(|_| Error::Runtime)?;
        let value = output.get(..size).ok_or(Error::Runtime)?;
        let text = std::str::from_utf8(value)
            .map_err(|_| Error::Runtime)?
            .trim();
        if text.is_empty() {
            return Err(Error::Runtime);
        }
        Ok(text.to_owned())
    }
}

impl Drop for Translator<'_> {
    fn drop(&mut self) {
        unsafe { (self.adapter.close)(self.context.as_ptr()) };
    }
}
