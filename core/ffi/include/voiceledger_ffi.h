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
    VL_INTERNAL_ERROR = -5
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

/* Negative returns are vl_error_v1. Positive handles are owned by the caller. */
int32_t vl_abi_version_v1(void);
vl_session_handle_v1 vl_session_create_v1(void);
int32_t vl_session_transition_v1(vl_session_handle_v1 handle, int32_t command, int64_t at_ns);
int32_t vl_session_cancel_v1(vl_session_handle_v1 handle);
int32_t vl_session_release_v1(vl_session_handle_v1 handle);

/* Supply -1 for an unknown memory value. Memory values are measured in bytes. */
int32_t vl_recommend_v1(int32_t compatibility, int64_t budget_bytes, int64_t peak_bytes);

#ifdef __cplusplus
}
#endif

#endif
