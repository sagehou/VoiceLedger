#pragma once

#include <string_view>

inline const char *speech_language(int language) {
    switch (language) {
        case 1: return "zh";
        case 2: return "en";
        case 3: return "ja";
        default: return "auto";
    }
}

inline int speech_language(std::string_view language) {
    if (language == "zh" || language == "<|zh|>") return 1;
    if (language == "en" || language == "<|en|>") return 2;
    if (language == "ja" || language == "<|ja|>") return 3;
    return 0;
}
