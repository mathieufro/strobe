cmake_minimum_required(VERSION 3.14)
project(strobe_test_fixtures CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_BUILD_TYPE Debug) # Always debug for DWARF symbols

# Fetch Catch2
include(FetchContent)
FetchContent_Declare(Catch2
    GIT_REPOSITORY https://github.com/catchorg/Catch2.git
    GIT_TAG v3.5.0
)
FetchContent_MakeAvailable(Catch2)

# Shared library of fixture functions
add_library(fixture_lib STATIC
    src/globals.cpp src/audio.cpp src/midi.cpp src/crash.cpp src/timing.cpp)
target_include_directories(fixture_lib PUBLIC src/)

# CLI target binary
add_executable(strobe_test_target src/main.cpp)
target_link_libraries(strobe_test_target PRIVATE fixture_lib)

# Catch2 test suite
add_executable(strobe_test_suite tests/test_main.cpp)
target_link_libraries(strobe_test_suite PRIVATE fixture_lib Catch2::Catch2WithMain)
