cmake_minimum_required(VERSION 3.20)
project(VoiceLedgerTranslation LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")

set(BUILD_SHARED_LIBS OFF)
set(BUILD_CLI OFF)
set(WITH_MKL OFF)
set(WITH_RUY ON)
set(OPENMP_RUNTIME NONE CACHE STRING "OpenMP runtime")
add_subdirectory("${CTRANSLATE2_SOURCE}" ctranslate2 EXCLUDE_FROM_ALL)

set(SPM_ENABLE_SHARED OFF)
set(SPM_ENABLE_TCMALLOC OFF)
add_subdirectory("${SENTENCEPIECE_SOURCE}" sentencepiece EXCLUDE_FROM_ALL)

add_library(voiceledger_translation SHARED translation.cpp)
target_include_directories(voiceledger_translation PRIVATE
    "${CTRANSLATE2_SOURCE}/third_party" "${SENTENCEPIECE_SOURCE}/src")
target_link_libraries(voiceledger_translation PRIVATE ctranslate2 sentencepiece-static)
if(ANDROID)
    target_link_libraries(voiceledger_translation PRIVATE log)
endif()

install(TARGETS voiceledger_translation RUNTIME DESTINATION . LIBRARY DESTINATION .)
