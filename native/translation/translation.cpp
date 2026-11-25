#include <cstdint>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "ctranslate2/translator.h"
#include "sentencepiece_processor.h"

#if defined(_WIN32)
#define VL_EXPORT extern "C" __declspec(dllexport)
#else
#define VL_EXPORT extern "C" __attribute__((visibility("default")))
#endif

namespace {
constexpr uint32_t max_text_bytes = 8192;
constexpr size_t max_input_tokens = 512;
constexpr size_t max_output_tokens = 256;

class DirectoryReader : public ctranslate2::models::ModelReader {
 public:
    explicit DirectoryReader(std::filesystem::path root) : root_(std::move(root)) {}

    std::string get_model_id() const override { return "opus-mt-en-zh-int8"; }

    std::unique_ptr<std::istream> get_file(const std::string& filename, bool binary) override {
        if (filename != "model.bin" && filename != "config.json" && filename != "shared_vocabulary.json") {
            return nullptr;
        }
        auto input = std::make_unique<std::ifstream>(
            root_ / filename, binary ? std::ios::binary : std::ios::in);
        if (!*input) return nullptr;
        return input;
    }

 private:
    std::filesystem::path root_;
};

void load_tokenizer(sentencepiece::SentencePieceProcessor& tokenizer, const std::filesystem::path& path) {
    std::ifstream input(path, std::ios::binary);
    if (!input) throw std::runtime_error("Tokenizer file missing");
    const std::string serialized((std::istreambuf_iterator<char>(input)), {});
    if (!tokenizer.LoadFromSerializedProto(serialized).ok()) {
        throw std::runtime_error("Invalid tokenizer");
    }
}

struct Translation {
    sentencepiece::SentencePieceProcessor source;
    sentencepiece::SentencePieceProcessor target;
    std::unique_ptr<ctranslate2::Translator> engine;

    explicit Translation(const std::filesystem::path& root) {
        load_tokenizer(source, root / "source.spm");
        load_tokenizer(target, root / "target.spm");
        ctranslate2::models::ModelLoader loader(std::make_shared<DirectoryReader>(root));
        loader.compute_type = ctranslate2::ComputeType::INT8;
        ctranslate2::ReplicaPoolConfig pool;
        pool.num_threads_per_replica = 1;
        pool.max_queued_batches = 1;
        engine = std::make_unique<ctranslate2::Translator>(loader, pool);
    }
};
}

VL_EXPORT const char* vl_translation_version_v1() {
    return "4.7.1";
}

VL_EXPORT void* vl_translation_open_v1(const char* directory) {
    if (!directory) return nullptr;
    try {
        return new Translation(std::filesystem::u8path(directory));
    } catch (...) {
        return nullptr;
    }
}

VL_EXPORT int32_t vl_translation_run_v1(void* handle, const char* input, uint32_t length,
                                      char* output, uint32_t capacity) {
    if (!handle || !input || !output || length == 0) return -1;
    if (length > max_text_bytes) return -2;
    try {
        auto& translation = *static_cast<Translation*>(handle);
        std::vector<std::string> tokens;
        if (!translation.source.Encode(std::string_view(input, length), &tokens).ok() || tokens.empty()) return -1;
        if (tokens.size() > max_input_tokens - 2) return -2;
        tokens.insert(tokens.begin(), ">>cmn_Hans<<");
        tokens.emplace_back("</s>");
        ctranslate2::TranslationOptions options;
        options.beam_size = 6;
        options.max_input_length = max_input_tokens;
        options.max_decoding_length = max_output_tokens;
        const auto results = translation.engine->translate_batch({std::move(tokens)}, options);
        const auto& pieces = results.at(0).output();
        std::string text;
        if (!translation.target.Decode(pieces, &text).ok() || text.empty()) return -1;
        if (pieces.size() >= max_output_tokens || text.size() > capacity || text.size() > max_text_bytes) return -2;
        std::memcpy(output, text.data(), text.size());
        return static_cast<int32_t>(text.size());
    } catch (...) {
        return -1;
    }
}

VL_EXPORT void vl_translation_close_v1(void* handle) {
    delete static_cast<Translation*>(handle);
}
