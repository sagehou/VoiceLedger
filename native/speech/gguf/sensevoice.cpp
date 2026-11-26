#include "../speech.h"
#include "../language.h"

#include <transcribe.h>

#include <cstring>
#include <memory>
#include <mutex>

struct SenseVoice {
    std::unique_ptr<transcribe_session, decltype(&transcribe_session_free)>
        session{nullptr, transcribe_session_free};
    int32_t language;
};

extern "C" const char *vl_asr_version() {
    return transcribe_version();
}

extern "C" void *vl_asr_open(const char *model, const char *, int32_t language, int32_t threads) try {
    if (!model || threads < 1) return nullptr;
    static std::once_flag quiet;
    std::call_once(quiet, [] {
        transcribe_log_set([](transcribe_log_level, const char *, void *) {}, nullptr);
    });
    auto engine = std::make_unique<SenseVoice>();
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
    const char *arch = transcribe_model_arch_string(transcribe_get_model(session));
    return arch && std::strcmp(arch, "sensevoice") == 0 ? engine.release() : nullptr;
} catch (...) {
    return nullptr;
}

extern "C" int32_t vl_asr_run(void *engine, const float *pcm, int32_t samples, char *text,
    int32_t capacity, int32_t *language, vl_asr_cancel cancel, void *user) try {
    if (!engine || !pcm || samples <= 0 || !text || capacity <= 0 || !language || (cancel && cancel(user))) return -1;
    const auto &sensevoice = *static_cast<const SenseVoice *>(engine);
    auto *session = sensevoice.session.get();
    transcribe_run_params params;
    transcribe_run_params_init(&params);
    params.language = sensevoice.language == 0 ? nullptr : speech_language(sensevoice.language);
    params.itn = TRANSCRIBE_ITN_MODE_ON;
    transcribe_set_abort_callback(session, cancel, user);
    const auto status = transcribe_run(session, pcm, samples, &params);
    transcribe_set_abort_callback(session, nullptr, nullptr);
    if (status != TRANSCRIBE_OK || (cancel && cancel(user))) return -1;
    const char *output = transcribe_full_text(session);
    const char *detected = transcribe_detected_language(session);
    *language = sensevoice.language != 0 ? sensevoice.language : (detected ? speech_language(detected) : 0);
    const auto length = output ? std::strlen(output) : 0;
    if (length > static_cast<size_t>(capacity)) return -1;
    if (length != 0) std::memcpy(text, output, length);
    return static_cast<int32_t>(length);
} catch (...) {
    return -1;
}

extern "C" void vl_asr_close(void *engine) {
    delete static_cast<SenseVoice *>(engine);
}
