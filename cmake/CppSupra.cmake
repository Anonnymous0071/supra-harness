# Helpers shared by the C++20 libraries (T2-T4).
#
# Two jobs:
#   1. Let the root CMakeLists reference libraries that do not exist yet, so the
#      tree configures at every point in the T1..T4 sequence.
#   2. Give each library one way to declare itself, so the ABI contract (static
#      archive, C-linkage headers, noexcept boundary) is stated once.

include_guard(GLOBAL)

set(SUPRA_LIBRARIES "" CACHE INTERNAL "C++ libraries configured in this tree")
set(SUPRA_LIBRARIES_MISSING "" CACHE INTERNAL "C++ libraries not yet scaffolded")

# supra_add_library_if_present(<name>)
#
# Adds cpp/<name> when it contains a CMakeLists.txt, otherwise records it as
# pending. Stages land in order, so a missing directory is expected state, not
# an error.
function(supra_add_library_if_present name)
    set(lib_dir "${CMAKE_CURRENT_SOURCE_DIR}/cpp/${name}")
    if(EXISTS "${lib_dir}/CMakeLists.txt")
        add_subdirectory("cpp/${name}")
        set(SUPRA_LIBRARIES "${SUPRA_LIBRARIES};${name}" CACHE INTERNAL "")
    else()
        set(SUPRA_LIBRARIES_MISSING "${SUPRA_LIBRARIES_MISSING};${name}" CACHE INTERNAL "")
    endif()
endfunction()

# supra_declare_library(<target> SOURCES <...> [PUBLIC_HEADER_DIR <dir>])
#
# Declares a static archive carrying the shared flag contract from
# supra_cxx_flags and the include layout T5 supra_ffi expects.
function(supra_declare_library target)
    set(options "")
    set(one_value_args PUBLIC_HEADER_DIR)
    set(multi_value_args SOURCES)
    cmake_parse_arguments(ARG "${options}" "${one_value_args}" "${multi_value_args}" ${ARGN})

    if(NOT ARG_SOURCES)
        message(FATAL_ERROR "supra_declare_library(${target}): SOURCES is required")
    endif()

    if(NOT ARG_PUBLIC_HEADER_DIR)
        set(ARG_PUBLIC_HEADER_DIR "${CMAKE_CURRENT_SOURCE_DIR}/include")
    endif()

    add_library(${target} STATIC ${ARG_SOURCES})
    add_library(supra::${target} ALIAS ${target})

    target_include_directories(
        ${target}
        PUBLIC $<BUILD_INTERFACE:${ARG_PUBLIC_HEADER_DIR}>
        PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/src
    )

    target_link_libraries(${target} PRIVATE supra_cxx_flags)

    set_target_properties(
        ${target}
        PROPERTIES ARCHIVE_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib"
                   # Deterministic archive names: T5's build.rs links by path.
                   PREFIX "lib"
                   OUTPUT_NAME "${target}"
    )
endfunction()

# supra_add_test(<name> SOURCES <...> LINK <targets...>)
#
# Registers one CTest executable. Tests are plain executables asserting on exit
# code, so no test framework enters the dependency graph.
function(supra_add_test name)
    if(NOT SUPRA_BUILD_TESTS)
        return()
    endif()

    set(options "")
    set(one_value_args "")
    set(multi_value_args SOURCES LINK)
    cmake_parse_arguments(ARG "${options}" "${one_value_args}" "${multi_value_args}" ${ARGN})

    if(NOT ARG_SOURCES)
        message(FATAL_ERROR "supra_add_test(${name}): SOURCES is required")
    endif()

    add_executable(${name} ${ARG_SOURCES})
    target_link_libraries(${name} PRIVATE supra_cxx_flags supra_testing ${ARG_LINK})
    set_target_properties(${name} PROPERTIES RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin")

    add_test(NAME ${name} COMMAND ${name})
    # A hang is a failure: these are pure computation over fixtures.
    set_tests_properties(${name} PROPERTIES TIMEOUT 120)
endfunction()

# Print which libraries configured and which are still pending, so the state of
# the scaffold is visible in the configure log.
function(supra_report_libraries)
    if(SUPRA_LIBRARIES)
        string(REPLACE ";" " " configured "${SUPRA_LIBRARIES}")
        message(STATUS "supra: configured libraries:${configured}")
    else()
        message(STATUS "supra: no C++ libraries configured yet (expected before T2)")
    endif()

    if(SUPRA_LIBRARIES_MISSING)
        string(REPLACE ";" " " pending "${SUPRA_LIBRARIES_MISSING}")
        message(STATUS "supra: pending libraries:${pending}")
    endif()
endfunction()
