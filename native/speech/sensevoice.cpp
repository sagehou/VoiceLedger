#include "speech.h"
#include "language.h"

#include <sherpa-onnx/c-api/c-api.h>

#include <cstring>
#include <filesystem>
#include <memory>

struct SenseVoice {
    std::unique_ptr<const SherpaOnnxOfflineRecognizer, decltype(&SherpaOnnxDestroyOfflineRecognizer)>
        recognizer{nullptr, SherpaOnnxDestroyOfflineRecognizer};
    int language;
};

extern "C" const char *vl_asr_version() {
    return SherpaOnnxGetVersionStr();
}

extern "C" void *vl_asr_open(const char *model, const char *tokens, int32_t language, int32_t threads) try {
    if (!model || !tokens || threads < 1 || !std::filesystem::is_regular_file(std::filesystem::u8path(model))
        || !std::filesystem::is_regular_file(std::filesystem::u8path(tokens))) return nullptr;
    auto engine = std::make_unique<SenseVoice>();
    engine->language = language;
    SherpaOnnxOfflineRecognizerConfig config {};
    config.feat_config.sample_rate = 16000;
    config.feat_config.feature_dim = 80;
    config.model_config.sense_voice.model = model;
    config.model_config.sense_voice.language = speech_language(language);
    config.model_config.sense_voice.use_itn = 1;
    config.model_config.tokens = tokens;
    config.model_config.model_type = "sense_voice";
    config.model_config.provider = "cpu";
    config.model_config.num_threads = threads;
    config.decoding_method = "greedy_search";
    engine->recognizer.reset(SherpaOnnxCreateOfflineRecognizer(&config));
    return engine->recognizer ? engine.release() : nullptr;
} catch (...) {
    return nullptr;
}

extern "C" int32_t vl_asr_run(void *engine, const float *pcm, int32_t samples, char *text,
    int32_t capacity, int32_t *language, vl_asr_cancel cancel, void *user) try {
    if (!engine || !pcm || samples <= 0 || !text || capacity <= 0 || !language || (cancel && cancel(user))) return -1;
    const auto &sensevoice = *static_cast<const SenseVoice *>(engine);
    const auto *recognizer = sensevoice.recognizer.get();
    auto stream = std::unique_ptr<const SherpaOnnxOfflineStream, decltype(&SherpaOnnxDestroyOfflineStream)>(
        SherpaOnnxCreateOfflineStream(recognizer), SherpaOnnxDestroyOfflineStream);
    if (!stream) return -1;
    SherpaOnnxAcceptWaveformOffline(stream.get(), 16000, pcm, samples);
    SherpaOnnxDecodeOfflineStream(recognizer, stream.get());
    if (cancel && cancel(user)) return -1;
    auto result = std::unique_ptr<const SherpaOnnxOfflineRecognizerResult, decltype(&SherpaOnnxDestroyOfflineRecognizerResult)>(
        SherpaOnnxGetOfflineStreamResult(stream.get()), SherpaOnnxDestroyOfflineRecognizerResult);
    if (!result || !result->text) return -1;
    const auto size = std::strlen(result->text);
    if (size > static_cast<size_t>(capacity)) return -1;
    *language = sensevoice.language != 0 ? sensevoice.language : (result->lang ? speech_language(result->lang) : 0);
    std::memcpy(text, result->text, size);
    return static_cast<int32_t>(size);
} catch (...) {
    return -1;
}

extern "C" void vl_asr_close(void *engine) {
    delete static_cast<SenseVoice *>(engine);
}
