#include "speech.h"

#include <cstdio>
#include <string_view>

int main(int argc, char **argv) {
    const char *version = vl_asr_version();
    if (argc != 2 || !version || std::string_view(version) != argv[1]) return 1;
    if (vl_asr_open("missing-model", "missing-tokens", 0, 1) != nullptr) return 2;
    std::printf("ASR runtime %s loaded and rejected a missing model.\n", version);
}
