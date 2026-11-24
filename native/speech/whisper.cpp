#include "speech.h"
#include "language.h"

#include <whisper.h>

#include <cstring>
#include <filesystem>
#include <fstream>
#include <memory>
#include <string>

struct Whisper {
    std::unique_ptr<whisper_context, decltype(&whisper_free)> context{nullptr, whisper_free};
    int language;
    int threads;
};

extern "C" const char *vl_asr_version() {
    return whisper_version();
}

extern "C" void *vl_asr_open(const char *model, const char *, int32_t language, int32_t threads) try {
    if (!model || threads < 1) return nullptr;
    std::ifstream file(std::filesystem::u8path(model), std::ios::binary);
    if (!file) return nullptr;
    whisper_log_set([](enum ggml_log_level, const char *, void *) {}, nullptr);
    auto engine = std::make_unique<Whisper>();
    engine->language = language;
    engine->threads = threads;
    whisper_model_loader loader {
        &file,
        [](void *pointer, void *output, size_t size) {
            auto &reader = *static_cast<std::ifstream *>(pointer);
            reader.read(static_cast<char *>(output), static_cast<std::streamsize>(size));
            return static_cast<size_t>(reader.gcount());
        },
        [](void *pointer) { return static_cast<std::ifstream *>(pointer)->eof(); },
        [](void *) {},
    };
    auto settings = whisper_context_default_params();
    settings.use_gpu = false;
    engine->context.reset(whisper_init_with_params(&loader, settings));
    return engine->context ? engine.release() : nullptr;
} catch (...) {
    return nullptr;
}

extern "C" int32_t vl_asr_run(void *engine, const float *pcm, int32_t samples, char *text,
    int32_t capacity, int32_t *language, vl_asr_cancel cancel, void *user) try {
    if (!engine || !pcm || samples <= 0 || !text || capacity <= 0 || !language) return -1;
    const auto &whisper = *static_cast<const Whisper *>(engine);
    auto *context = whisper.context.get();
    auto settings = whisper_full_default_params(WHISPER_SAMPLING_GREEDY);
    settings.n_threads = whisper.threads;
    settings.language = speech_language(whisper.language);
    settings.print_special = false;
    settings.print_progress = false;
    settings.print_realtime = false;
    settings.print_timestamps = false;
    settings.no_context = true;
    settings.no_timestamps = true;
    settings.suppress_nst = true;
    settings.abort_callback = cancel;
    settings.abort_callback_user_data = user;
    if (whisper_full(context, settings, pcm, samples) != 0 || (cancel && cancel(user))) return -1;
    const char *detected = whisper_lang_str(whisper_full_lang_id(context));
    *language = whisper.language != 0 ? whisper.language : (detected ? speech_language(detected) : 0);
    std::string result;
    for (int segment = 0; segment < whisper_full_n_segments(context); ++segment) {
        const char *part = whisper_full_get_segment_text(context, segment);
        if (part) result.append(part);
        if (result.size() > static_cast<size_t>(capacity)) return -1;
    }
    std::memcpy(text, result.data(), result.size());
    return static_cast<int32_t>(result.size());
} catch (...) {
    return -1;
}

extern "C" void vl_asr_close(void *engine) {
    delete static_cast<Whisper *>(engine);
}
