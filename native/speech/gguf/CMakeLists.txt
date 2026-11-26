cmake_minimum_required(VERSION 3.20)
project(VoiceLedgerSenseVoiceGguf LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(CMAKE_CXX_VISIBILITY_PRESET hidden)
foreach(option TRANSCRIBE_BUILD_TESTS TRANSCRIBE_BUILD_EXAMPLES TRANSCRIBE_BUILD_TOOLS
    TRANSCRIBE_BUILD_SHARED TRANSCRIBE_INSTALL TRANSCRIBE_USE_SYSTEM_BLAS GGML_NATIVE)
    set(${option} OFF CACHE BOOL "" FORCE)
endforeach()
add_subdirectory("${TRANSCRIBE_SOURCE}" transcribe EXCLUDE_FROM_ALL)
# The upstream target resolves its include directory from the top-level project.
target_include_directories(transcribe PUBLIC "${TRANSCRIBE_SOURCE}/include")

add_library(voiceledger_sensevoice_gguf SHARED sensevoice.cpp)
target_link_libraries(voiceledger_sensevoice_gguf PRIVATE transcribe)
target_link_options(voiceledger_sensevoice_gguf PRIVATE "-Wl,--exclude-libs,ALL")
install(TARGETS voiceledger_sensevoice_gguf LIBRARY DESTINATION .)
