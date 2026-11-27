#include <algorithm>
#include <cmath>
#include <memory>

#include "sherpa-onnx/c-api/c-api.h"
#include "speech.h"

extern "C" {

VL_SPEECH_API const char *vl_speaker_version_v1() {
    return "1.12.20";
}

VL_SPEECH_API const SherpaOnnxSpeakerEmbeddingExtractor *vl_speaker_open_v1(const char *model) {
    if (!model || !*model) return nullptr;
    try {
        SherpaOnnxSpeakerEmbeddingExtractorConfig config{};
        config.model = model;
        config.num_threads = 1;
        config.provider = "cpu";
        return SherpaOnnxCreateSpeakerEmbeddingExtractor(&config);
    } catch (...) {
        return nullptr;
    }
}

VL_SPEECH_API int32_t vl_speaker_extract_v1(
    const SherpaOnnxSpeakerEmbeddingExtractor *engine, const float *pcm, int32_t count,
    float *output, int32_t capacity) {
    if (!engine || !pcm || !output || count <= 0 || count > 16000 * 12) return -1;
    if (count < 8000) return 0;
    if (std::any_of(pcm, pcm + count, [](float sample) {
        return !std::isfinite(sample) || sample < -1 || sample > 1;
    })) return -1;
    if (std::all_of(pcm, pcm + count, [](float sample) {
        return std::abs(sample) < 0.001f;
    })) return 0;
    try {
        const auto dim = SherpaOnnxSpeakerEmbeddingExtractorDim(engine);
        if (dim <= 0 || dim > capacity) return -1;
        std::unique_ptr<const SherpaOnnxOnlineStream, decltype(&SherpaOnnxDestroyOnlineStream)> stream(
            SherpaOnnxSpeakerEmbeddingExtractorCreateStream(engine), SherpaOnnxDestroyOnlineStream);
        if (!stream) return -1;
        SherpaOnnxOnlineStreamAcceptWaveform(stream.get(), 16000, pcm, count);
        SherpaOnnxOnlineStreamInputFinished(stream.get());
        if (!SherpaOnnxSpeakerEmbeddingExtractorIsReady(engine, stream.get())) return 0;
        std::unique_ptr<const float, decltype(&SherpaOnnxSpeakerEmbeddingExtractorDestroyEmbedding)> embedding(
            SherpaOnnxSpeakerEmbeddingExtractorComputeEmbedding(engine, stream.get()),
            SherpaOnnxSpeakerEmbeddingExtractorDestroyEmbedding);
        if (!embedding) return -1;
        std::copy_n(embedding.get(), dim, output);
        return dim;
    } catch (...) {
        return -1;
    }
}

VL_SPEECH_API void vl_speaker_close_v1(const SherpaOnnxSpeakerEmbeddingExtractor *engine) {
    if (engine) SherpaOnnxDestroySpeakerEmbeddingExtractor(engine);
}

}
