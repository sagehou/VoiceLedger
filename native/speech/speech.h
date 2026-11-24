#pragma once

#include <stdint.h>
#include <stdbool.h>

#ifdef _WIN32
#  ifdef VL_SPEECH_BUILD
#    define VL_SPEECH_API __declspec(dllexport)
#  else
#    define VL_SPEECH_API __declspec(dllimport)
#  endif
#else
#  define VL_SPEECH_API __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef bool (*vl_asr_cancel)(void *);

VL_SPEECH_API const char *vl_asr_version(void);
VL_SPEECH_API void *vl_asr_open(const char *model, const char *tokens, int32_t language, int32_t threads);
VL_SPEECH_API int32_t vl_asr_run(void *engine, const float *pcm, int32_t samples, char *text,
    int32_t capacity, int32_t *language, vl_asr_cancel cancel, void *user);
VL_SPEECH_API void vl_asr_close(void *engine);

#ifdef __cplusplus
}
#endif
