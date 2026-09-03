# Shared test assertion helpers.
#
# Header-only INTERFACE target: no sources, nothing to link. Introduced when
# libsupra_ansi became the second consumer of what had been
# libsupra_width/tests/test_assert.hpp.

add_library(supra_testing INTERFACE)
add_library(supra::testing ALIAS supra_testing)

target_include_directories(supra_testing INTERFACE "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(supra_testing INTERFACE supra_cxx_flags)
