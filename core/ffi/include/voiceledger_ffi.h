#ifndef VOICELEDGER_FFI_H
#define VOICELEDGER_FFI_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum vl_error_v1 {
    VL_INVALID_ARGUMENT = -1,
    VL_UNKNOWN_HANDLE = -2,
    VL_INVALID_TRANSITION = -3,
    VL_CANCELLED = -4,
    VL_INTERNAL_ERROR = -5,
    VL_DEVICE_PROFILE_STORAGE_ERROR = -6,
    VL_UNSUPPORTED_SCHEMA_VERSION = -10,
    VL_INVALID_DOMAIN_JSON = -11,
    VL_DUPLICATE_DOMAIN_IDENTITY = -12,
    VL_INVALID_DOMAIN_REFERENCE = -13,
    VL_INVALID_DOMAIN_TRANSITION = -14,
    VL_INVALID_DOMAIN_TIMELINE = -15,
    VL_INVALID_AUDIO_DURABILITY = -16,
    VL_INVALID_DOMAIN_REVISION = -17,
    VL_INVALID_DOMAIN_METADATA = -18,
    VL_AUDIO_STORE_ERROR = -20,
    VL_AUDIO_STORE_LOCKED = -21,
    VL_AUDIO_UNAVAILABLE = -22,
    VL_INVALID_AUDIO_MEDIA = -23,
    VL_UNSUPPORTED_AUDIO_SCHEMA = -24,
    VL_SPEECH_STORAGE_ERROR = -30,
    VL_SPEECH_RUNTIME_ERROR = -31,
    VL_SPEECH_NO_AUDIO = -32,
    VL_SPEECH_MODEL_NOT_INSTALLED = -33,
    VL_SPEECH_BLOCKED = -34,
    VL_SPEECH_TRANSFER_ERROR = -35,
    VL_SPEECH_INTEGRITY_ERROR = -36,
    VL_SPEECH_BUSY = -37,
    VL_SPEECH_DEFERRED = -38,
    VL_SPEECH_REVISION_CONFLICT = -39,
    VL_SPEECH_TRANSLATION_TEXT_LIMIT = -40
};

enum vl_session_command_v1 {
    VL_PREPARE = 1,
    VL_START = 2,
    VL_PAUSE = 3,
    VL_RESUME = 4,
    VL_STOP = 5,
    VL_FINISH = 6,
    VL_INTERRUPT = 7
};

enum vl_session_state_v1 {
    VL_IDLE = 0,
    VL_PREPARING = 1,
    VL_RECORDING = 2,
    VL_PAUSED = 3,
    VL_STOPPING = 4,
    VL_COMPLETED = 5,
    VL_INTERRUPTED = 6
};

enum vl_compatibility_v1 {
    VL_COMPATIBILITY_UNKNOWN = 0,
    VL_INCOMPATIBLE = 1,
    VL_COMPATIBLE = 2
};

enum vl_recommendation_v1 {
    VL_RECORDING_ONLY = 0,
    VL_CANDIDATE_FOR_BENCHMARK = 1
};

typedef int64_t vl_session_handle_v1;
typedef int64_t vl_track_handle_v1;
typedef int64_t vl_audio_handle_v1;

typedef struct {
    uint8_t *data;
    uint32_t length;
} vl_audio_buffer_v1;

enum vl_track_command_v1 {
    VL_TRACK_START = 1, VL_TRACK_PAUSE = 2, VL_TRACK_RESUME = 3,
    VL_TRACK_DISCONNECT = 4, VL_TRACK_RECONNECT_CONFIRMED = 5,
    VL_TRACK_STOP = 6, VL_TRACK_FINISH = 7, VL_TRACK_INTERRUPT = 8
};

enum vl_track_state_v1 {
    VL_TRACK_PREPARING = 0, VL_TRACK_RECORDING = 1, VL_TRACK_PAUSED = 2,
    VL_TRACK_DISCONNECTED = 3, VL_TRACK_STOPPING = 4,
    VL_TRACK_COMPLETED = 5, VL_TRACK_INTERRUPTED = 6
};

/* Negative returns are vl_error_v1. Positive handles are owned by the caller. */
int32_t vl_abi_version_v1(void);
/* Borrowed UTF-8 input. The synchronous call accepts up to 16 MiB. */
int32_t vl_domain_validate_v1(const uint8_t *data, uint32_t length);
/* Borrowed UTF-8 directory and device measurement JSON, up to 128 KiB and 16 KiB respectively. */
int32_t vl_device_profile_save_v1(const uint8_t *directory, uint32_t directory_length,
    const uint8_t *measurement, uint32_t measurement_length);
/* Read-only catalog preview for an app-private profile directory. Returns bytes written. */
int32_t vl_model_preview_v1(const uint8_t *directory, uint32_t directory_length, uint8_t *output, uint32_t capacity);
vl_session_handle_v1 vl_session_create_v1(void);
int32_t vl_session_transition_v1(vl_session_handle_v1 handle, int32_t command, int64_t at_ns);
int32_t vl_session_cancel_v1(vl_session_handle_v1 handle);
int32_t vl_session_release_v1(vl_session_handle_v1 handle);

/* The UTF-8 endpoint is required at creation and for confirmed reconnect only. */
vl_track_handle_v1 vl_track_create_v1(const uint8_t *endpoint, uint32_t length);
int32_t vl_track_transition_v1(vl_track_handle_v1 handle, int32_t command, int64_t at_ns,
    const uint8_t *confirmed_endpoint, uint32_t length);
int64_t vl_track_source_epoch_v1(vl_track_handle_v1 handle);
int32_t vl_track_release_v1(vl_track_handle_v1 handle);

/* UTF-8 device-local directory. Opening reconciles the manifest before returning. */
vl_audio_handle_v1 vl_audio_open_v1(const uint8_t *path, uint32_t length);
/* Borrowed JSON command and optional bounded interleaved float32 PCM at 48 kHz.
   The caller releases any non-null result once with the original length. */
int32_t vl_audio_request_v1(vl_audio_handle_v1 handle, const uint8_t *command, uint32_t length,
    const float *pcm, uint32_t samples, vl_audio_buffer_v1 *result);
void vl_audio_buffer_release_v1(uint8_t *data, uint32_t length);
int32_t vl_audio_release_v1(vl_audio_handle_v1 handle);

/* Local speech I/O. Mutating requests belong in the platform's speech worker. */
int32_t vl_speech_request_v1(const uint8_t *request, uint32_t length, vl_audio_buffer_v1 *result);
int32_t vl_speech_partial_v1(vl_audio_buffer_v1 *result);
void vl_speech_cancel_v1(void);
int32_t vl_speech_resources_v1(const uint8_t *sample, uint32_t length);

/* Supply -1 for an unknown memory value. Memory values are measured in bytes. */
int32_t vl_recommend_v1(int32_t compatibility, int64_t budget_bytes, int64_t peak_bytes);

#ifdef __cplusplus
}
#endif

#endif
