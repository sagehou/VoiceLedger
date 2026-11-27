use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use libloading::Library;
use voiceledger_core::contracts::{Language, LanguagePolicy};
use voiceledger_core::models::{Model, RuntimeIdentity, RuntimeProbe};

use crate::pipeline::{Engine, Recognition, StreamText};
use crate::{Error, Result, check_cancel};

type Open = unsafe extern "C" fn(*const c_char, *const c_char, i32, i32) -> *mut c_void;
type Cancel = extern "C" fn(*mut c_void) -> bool;
type Run = unsafe extern "C" fn(
    *mut c_void,
    *const f32,
    i32,
    *mut c_char,
    i32,
    *mut i32,
    Cancel,
    *mut c_void,
) -> i32;
type StreamStep = unsafe extern "C" fn(
    *mut c_void,
    *const f32,
    i32,
    *mut c_char,
    i32,
    *mut i32,
    Cancel,
    *mut c_void,
    bool,
) -> i32;
type Close = unsafe extern "C" fn(*mut c_void);

pub struct Adapter {
    identity: String,
    version: String,
    threads: i32,
    open: Open,
    run: Run,
    stream_step: Option<StreamStep>,
    close: Close,
    _library: Library,
}

impl Adapter {
    pub fn load(model: &Model, native: &Path, threads: i32) -> Result<Self> {
        let path = adapter_path(&model.runtime.id, native).ok_or(Error::Runtime)?;
        let library = load_library(&path).map_err(|_| Error::Runtime)?;
        unsafe {
            let version =
                library.get::<unsafe extern "C" fn() -> *const c_char>(b"vl_asr_version")?;
            let value = version();
            if value.is_null()
                || CStr::from_ptr(value).to_str().ok() != Some(model.runtime.version.as_str())
            {
                return Err(Error::Runtime);
            }
            let open = *library.get::<Open>(b"vl_asr_open")?;
            let run = *library.get::<Run>(b"vl_asr_run")?;
            let stream_step = if model.id == crate::live::NEMOTRON {
                Some(*library.get::<StreamStep>(b"vl_asr_stream_step")?)
            } else {
                None
            };
            let close = *library.get::<Close>(b"vl_asr_close")?;
            Ok(Self {
                identity: model.runtime.id.clone(),
                version: model.runtime.version.clone(),
                threads,
                open,
                run,
                stream_step,
                close,
                _library: library,
            })
        }
    }

    pub fn engine<'a>(
        &'a self,
        model: &Model,
        root: &Path,
        language: LanguagePolicy,
        stream: bool,
    ) -> Result<NativeEngine<'a>> {
        let weights = model.artifacts.first().ok_or(Error::Invalid)?;
        let weights = c_path(&root.join(&weights.file))?;
        let tokens = model
            .artifacts
            .get(1)
            .map(|file| c_path(&root.join(&file.file)))
            .transpose()?;
        let context = unsafe {
            (self.open)(
                weights.as_ptr(),
                tokens
                    .as_ref()
                    .map_or(std::ptr::null(), |file| file.as_ptr()),
                language_code(language),
                self.threads,
            )
        };
        Ok(NativeEngine {
            adapter: self,
            context: NonNull::new(context).ok_or(Error::Runtime)?,
            stream_step: self.stream_step.filter(|_| stream),
            _weights: weights,
            _tokens: tokens,
        })
    }
}

impl RuntimeProbe for Adapter {
    fn identity(&self) -> RuntimeIdentity<'_> {
        RuntimeIdentity {
            id: &self.identity,
            version: &self.version,
        }
    }

    fn load_and_release(
        &self,
        model: &Model,
        directory: &Path,
        cancelled: &dyn Fn() -> bool,
    ) -> bool {
        !cancelled()
            && self
                .engine(model, directory, LanguagePolicy::Auto, false)
                .is_ok()
    }
}

pub struct NativeEngine<'a> {
    adapter: &'a Adapter,
    context: NonNull<c_void>,
    stream_step: Option<StreamStep>,
    _weights: CString,
    _tokens: Option<CString>,
}

impl Drop for NativeEngine<'_> {
    fn drop(&mut self) {
        unsafe { (self.adapter.close)(self.context.as_ptr()) };
    }
}

impl Engine for NativeEngine<'_> {
    fn recognize(
        &mut self,
        samples: &[f32],
        _: LanguagePolicy,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<Recognition>> {
        Ok(self
            .infer(samples, cancelled, None)?
            .and_then(StreamText::confirmed))
    }

    fn streams(&self) -> bool {
        self.stream_step.is_some()
    }

    fn stream_step(
        &mut self,
        samples: &[f32],
        finish: bool,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<StreamText>> {
        self.infer(samples, cancelled, Some(finish))
    }
}

impl NativeEngine<'_> {
    fn infer(
        &self,
        samples: &[f32],
        cancelled: &(dyn Fn() -> bool + Sync),
        finish: Option<bool>,
    ) -> Result<Option<StreamText>> {
        let mut text = [0_u8; 8 * 1024];
        let mut language = 0;
        let mut cancellation = cancelled;
        let count = i32::try_from(samples.len()).map_err(|_| Error::Invalid)?;
        let context = self.context.as_ptr();
        let capacity = text.len() as i32;
        let user = std::ptr::from_mut(&mut cancellation).cast();
        let length = unsafe {
            if let Some(finish) = finish {
                (self.stream_step.ok_or(Error::Runtime)?)(
                    context,
                    samples.as_ptr(),
                    count,
                    text.as_mut_ptr().cast(),
                    capacity,
                    &mut language,
                    cancel_callback,
                    user,
                    finish,
                )
            } else {
                (self.adapter.run)(
                    context,
                    samples.as_ptr(),
                    count,
                    text.as_mut_ptr().cast(),
                    capacity,
                    &mut language,
                    cancel_callback,
                    user,
                )
            }
        };
        check_cancel(cancelled)?;
        let length = usize::try_from(length).map_err(|_| Error::Runtime)?;
        let language = match language {
            1 => Some(Language::Zh),
            2 => Some(Language::En),
            3 => Some(Language::Ja),
            _ => None,
        };
        let output = text.get(..length).ok_or(Error::Runtime)?;
        let text = std::str::from_utf8(output)
            .map_err(|_| Error::Runtime)?
            .trim()
            .to_owned();
        Ok((!text.is_empty()).then_some(StreamText { language, text }))
    }
}

extern "C" fn cancel_callback(context: *mut c_void) -> bool {
    let check = unsafe { *context.cast::<&(dyn Fn() -> bool + Sync)>() };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(check)).unwrap_or(true)
}

pub fn adapter_path(runtime: &str, native: &Path) -> Option<PathBuf> {
    let basename = match runtime {
        "whisper.cpp" => "voiceledger_whisper",
        "sherpa-onnx" => "voiceledger_sensevoice",
        "transcribe.cpp" => "voiceledger_sensevoice_gguf",
        "ctranslate2" => "voiceledger_translation",
        _ => return None,
    };
    Some(native.join(libloading::library_filename(basename)))
}

#[cfg(windows)]
pub(crate) fn load_library(path: &Path) -> std::result::Result<Library, libloading::Error> {
    use libloading::os::windows::{
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        Library as WindowsLibrary,
    };
    unsafe {
        WindowsLibrary::load_with_flags(
            path,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
        .map(Library::from)
    }
}

#[cfg(not(windows))]
pub(crate) fn load_library(path: &Path) -> std::result::Result<Library, libloading::Error> {
    unsafe { Library::new(path) }
}

pub(crate) fn c_path(path: &Path) -> Result<CString> {
    CString::new(path.to_str().ok_or(Error::Invalid)?).map_err(|_| Error::Invalid)
}

fn language_code(policy: LanguagePolicy) -> i32 {
    match policy {
        LanguagePolicy::Auto => 0,
        LanguagePolicy::Zh => 1,
        LanguagePolicy::En => 2,
        LanguagePolicy::Ja => 3,
    }
}
