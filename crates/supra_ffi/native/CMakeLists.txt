# Root CMake project for the supra-harness C++20 hot-path libraries.
#
# Three libraries land here, each behind a flat C ABI so that T5 supra_ffi can
# link them statically and expose a safe Rust surface:
#
#   T2  libsupra_width    terminal cell width: wcwidth, grapheme segmentation,
#                         East Asian Width and emoji ZWJ tables. Sole arbiter
#                         of every layout decision in the TUI.
#   T3  libsupra_ansi     escape sequence parser/serialiser, including
#                         supra_ansi_truncate which cuts to a cell width
#                         without ever leaking an SGR sequence.
#   T4  libsupra_sandbox  OS-level process isolation behind one ABI
#                         (bubblewrap+landlock, sandbox-exec, AppContainer).
#
# Targets are added by their own stages. This file only establishes the shared
# contract: C++20, no exceptions across the ABI, position-independent static
# archives, and warnings-as-errors.

cmake_minimum_required(VERSION 3.24)

project(
    supra_cpp
    VERSION 0.1.0
    DESCRIPTION "supra-harness C++20 hot-path libraries"
    LANGUAGES CXX
)

# ---------------------------------------------------------------------------
# Language contract
# ---------------------------------------------------------------------------

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# T5 links these archives into a Rust staticlib, which requires PIC.
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

set(CMAKE_VISIBILITY_INLINES_HIDDEN ON)
set(CMAKE_CXX_VISIBILITY_PRESET hidden)

if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
    set(CMAKE_BUILD_TYPE RelWithDebInfo CACHE STRING "" FORCE)
endif()

set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

# ---------------------------------------------------------------------------
# Shared interface target
#
# Every library links this instead of repeating flags. Kept as an INTERFACE
# target so the flags travel with the dependency edge.
# ---------------------------------------------------------------------------

add_library(supra_cxx_flags INTERFACE)

target_compile_features(supra_cxx_flags INTERFACE cxx_std_20)

if(MSVC)
    target_compile_options(supra_cxx_flags INTERFACE /W4 /WX /permissive- /utf-8 /EHsc)
else()
    target_compile_options(
        supra_cxx_flags
        INTERFACE -Wall
                  -Wextra
                  -Wpedantic
                  -Werror
                  -Wconversion
                  -Wsign-conversion
                  -Wshadow
                  -Wnon-virtual-dtor
                  -Wold-style-cast
                  -Wcast-align
                  -Wunused
                  -Woverloaded-virtual
                  -Wdouble-promotion
                  -Wformat=2
                  # A C++ exception unwinding into Rust is undefined behaviour.
                  # The ABI is noexcept by construction, so the machinery is
                  # removed outright rather than merely unused.
                  -fno-exceptions
                  -fno-rtti
    )
endif()

# Hardening for the release archives that ship in the binary.
include(CheckCXXCompilerFlag)
foreach(flag -fstack-protector-strong -D_FORTIFY_SOURCE=2)
    string(MAKE_C_IDENTIFIER "HAVE${flag}" flag_var)
    check_cxx_compiler_flag(${flag} ${flag_var})
    if(${flag_var})
        target_compile_options(supra_cxx_flags INTERFACE $<$<NOT:$<CONFIG:Debug>>:${flag}>)
    endif()
endforeach()

# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------

option(SUPRA_BUILD_TESTS "Build CTest suites for the C++20 libraries" ON)
option(SUPRA_ENABLE_SANITIZERS "Build with ASan+UBSan (developer builds only)" OFF)

if(SUPRA_ENABLE_SANITIZERS AND NOT MSVC)
    target_compile_options(
        supra_cxx_flags INTERFACE -fsanitize=address,undefined -fno-omit-frame-pointer
    )
    target_link_options(supra_cxx_flags INTERFACE -fsanitize=address,undefined)
endif()

if(SUPRA_BUILD_TESTS AND CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
    enable_testing()
endif()

# ---------------------------------------------------------------------------
# Libraries
#
# Each stage appends its add_subdirectory() call. Guarded so that a
# partially-scaffolded tree still configures, which keeps `just doctor` and
# `just ci` usable between stages.
# ---------------------------------------------------------------------------

include(cmake/CppSupra.cmake)

# Shared test helpers, declared before the libraries that consume them.
if(SUPRA_BUILD_TESTS)
    add_subdirectory(cpp/testing)
endif()

supra_add_library_if_present(libsupra_width)   # T2
supra_add_library_if_present(libsupra_ansi)    # T3
supra_add_library_if_present(libsupra_sandbox) # T4

supra_report_libraries()
