cmake_minimum_required(VERSION 3.20)
project(VoiceLedgerTranslation LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")

set(BUILD_SHARED_LIBS OFF CACHE BOOL "Build static dependencies")
set(BUILD_CLI OFF CACHE BOOL "Skip upstream CLI tools")
set(WITH_MKL OFF CACHE BOOL "Disable MKL")
set(WITH_RUY ON CACHE BOOL "Use Ruy CPU kernels")
set(OPENMP_RUNTIME NONE CACHE STRING "OpenMP runtime")
add_subdirectory("${CTRANSLATE2_SOURCE}" ctranslate2 EXCLUDE_FROM_ALL)
set_target_properties(ctranslate2 PROPERTIES POSITION_INDEPENDENT_CODE ON)
if(CMAKE_SYSTEM_NAME STREQUAL "Linux" AND CMAKE_SYSTEM_PROCESSOR STREQUAL "x86_64")
    # The dispatcher needs the generic kernel even if the compiler defaults to AVX.
    target_compile_options(ctranslate2 PRIVATE -march=x86-64)
endif()

set(SPM_ENABLE_SHARED OFF CACHE BOOL "Build SentencePiece statically")
set(SPM_ENABLE_TCMALLOC OFF CACHE BOOL "Use the platform allocator")
add_subdirectory("${SENTENCEPIECE_SOURCE}" sentencepiece EXCLUDE_FROM_ALL)

add_library(voiceledger_translation SHARED translation.cpp)
target_include_directories(voiceledger_translation PRIVATE
    "${CTRANSLATE2_SOURCE}/third_party" "${SENTENCEPIECE_SOURCE}/src")
target_link_libraries(voiceledger_translation PRIVATE ctranslate2 sentencepiece-static)
if(CMAKE_SYSTEM_NAME STREQUAL "Linux" OR ANDROID)
    target_link_options(voiceledger_translation PRIVATE -Wl,--no-undefined)
endif()
if(ANDROID)
    target_link_libraries(voiceledger_translation PRIVATE log)
endif()

install(TARGETS voiceledger_translation RUNTIME DESTINATION . LIBRARY DESTINATION .)
