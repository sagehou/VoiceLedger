#include "../speech.h"
#include "../language.h"

#include <transcribe.h>
#include <transcribe/parakeet.h>

#include <cstring>
#include <memory>
#include <mutex>

struct GgufEngine {
    std::unique_ptr<transcribe_session, decltype(&transcribe_session_free)>
        session{nullptr, transcribe_session_free};
    int32_t language;
    bool nemotron;
};

struct AbortScope {
    transcribe_session *session;
    AbortScope(transcribe_session *session, vl_asr_cancel cancel, void *user) : session(session) {
        transcribe_set_abort_callback(session, cancel, user);
    }
    ~AbortScope() { transcribe_set_abort_callback(session, nullptr, nullptr); }
};

static const char *nemotron_prompt(int32_t language) {
    switch (language) {
        case 1: return "zh-CN";
        case 2: return "en-US";
        case 3: return "ja-JP";
        default: return "auto";
    }
}

static int32_t nemotron_language(const transcribe_session *session) {
    // Parakeet retains the emitted locale token in the raw decode.
    const char *raw = transcribe_raw_text(session);
    if (!raw) return 0;
    const std::string_view text(raw);
    int32_t language = 0;
    for (auto start = text.find('<'); start != text.npos; start = text.find('<', start + 1)) {
        const auto end = text.find('>', start + 1);
        if (end != text.npos && end - start == 6 && text[start + 3] == '-' &&
            text[start + 4] >= 'A' && text[start + 4] <= 'Z' &&
            text[start + 5] >= 'A' && text[start + 5] <= 'Z') {
            language = speech_language(text.substr(start + 1, 2));
        }
    }
    return language;
}

static int32_t copy_result(const GgufEngine &engine, char *text, int32_t capacity, int32_t *language) {
    const auto *session = engine.session.get();
    const char *output = transcribe_full_text(session);
    const auto length = output ? std::strlen(output) : 0;
    if (length > static_cast<size_t>(capacity)) return -1;
    if (length != 0) std::memcpy(text, output, length);
    if (engine.language != 0) {
        *language = engine.language;
    } else if (engine.nemotron) {
        *language = nemotron_language(session);
    } else {
        const char *detected = transcribe_detected_language(session);
        *language = detected ? speech_language(detected) : 0;
    }
    return static_cast<int32_t>(length);
}

extern "C" const char *vl_asr_version() {
    return transcribe_version();
}

extern "C" void *vl_asr_open(const char *model, const char *, int32_t language, int32_t threads) try {
    if (!model || threads < 1) return nullptr;
    static std::once_flag quiet;
    std::call_once(quiet, [] {
        transcribe_log_set([](transcribe_log_level, const char *, void *) {}, nullptr);
    });
    auto engine = std::make_unique<GgufEngine>();
    engine->language = language;
    transcribe_model_load_params load;
    transcribe_model_load_params_init(&load);
    load.backend = TRANSCRIBE_BACKEND_CPU;
    transcribe_session_params session_params;
    transcribe_session_params_init(&session_params);
    session_params.n_threads = threads;
    transcribe_session *session = nullptr;
    if (transcribe_open(model, &load, &session_params, &session) != TRANSCRIBE_OK) return nullptr;
    engine->session.reset(session);
    const auto *loaded = transcribe_get_model(session);
    const char *arch = transcribe_model_arch_string(loaded);
    const char *variant = transcribe_model_variant_string(loaded);
    engine->nemotron = variant && std::strcmp(variant, "nemotron-3.5-asr-streaming-0.6b") == 0 &&
        transcribe_model_accepts_ext_kind(loaded, TRANSCRIBE_EXT_SLOT_STREAM, TRANSCRIBE_EXT_KIND_PARAKEET_STREAM);
    return engine->nemotron || (arch && std::strcmp(arch, "sensevoice") == 0) ? engine.release() : nullptr;
} catch (...) {
    return nullptr;
}

extern "C" int32_t vl_asr_run(void *engine, const float *pcm, int32_t samples, char *text,
    int32_t capacity, int32_t *language, vl_asr_cancel cancel, void *user) try {
    if (!engine || !pcm || samples <= 0 || !text || capacity <= 0 || !language || (cancel && cancel(user))) return -1;
    const auto &gguf = *static_cast<const GgufEngine *>(engine);
    auto *session = gguf.session.get();
    transcribe_run_params params;
    transcribe_run_params_init(&params);
    if (gguf.nemotron) {
        params.language = nemotron_prompt(gguf.language);
    } else {
        params.language = gguf.language == 0 ? nullptr : speech_language(gguf.language);
        params.itn = TRANSCRIBE_ITN_MODE_ON;
    }
    AbortScope abort(session, cancel, user);
    if (transcribe_run(session, pcm, samples, &params) != TRANSCRIBE_OK || (cancel && cancel(user))) return -1;
    return copy_result(gguf, text, capacity, language);
} catch (...) {
    return -1;
}

extern "C" int32_t vl_asr_stream_step(void *engine, const float *pcm, int32_t samples, char *text,
    int32_t capacity, int32_t *language, vl_asr_cancel cancel, void *user, bool finish) try {
    if (!engine || samples < 0 || (samples != 0 && !pcm) || !text || capacity <= 0 || !language ||
        (cancel && cancel(user))) return -1;
    const auto &gguf = *static_cast<const GgufEngine *>(engine);
    if (!gguf.nemotron) return -1;
    auto *session = gguf.session.get();
    AbortScope abort(session, cancel, user);
    if (transcribe_stream_get_state(session) != TRANSCRIBE_STREAM_ACTIVE) {
        if (samples == 0) return -1;
        transcribe_run_params params;
        transcribe_run_params_init(&params);
        params.language = nemotron_prompt(gguf.language);
        transcribe_parakeet_stream_ext context;
        transcribe_parakeet_stream_ext_init(&context);
        context.att_context_right = 13;
        transcribe_stream_params stream;
        transcribe_stream_params_init(&stream);
        stream.family = &context.ext;
        if (transcribe_stream_begin(session, &params, &stream) != TRANSCRIBE_OK) return -1;
    }
    if (samples > 0 && transcribe_stream_feed(session, pcm, samples, nullptr) != TRANSCRIBE_OK) return -1;
    if (finish && transcribe_stream_finalize(session, nullptr) != TRANSCRIBE_OK) return -1;
    if (cancel && cancel(user)) return -1;
    return copy_result(gguf, text, capacity, language);
} catch (...) {
    return -1;
}

extern "C" void vl_asr_close(void *engine) {
    delete static_cast<GgufEngine *>(engine);
}
