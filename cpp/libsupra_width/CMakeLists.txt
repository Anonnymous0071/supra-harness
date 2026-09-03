# T2 - libsupra_width: terminal cell width and grapheme segmentation.
#
# Sole arbiter of every layout decision in the supra TUI. The generated tables
# are committed, so this builds offline with no Python at build time; the
# generator scripts are run by hand when moving Unicode versions.

supra_declare_library(
    libsupra_width
    SOURCES src/utf8.cpp src/grapheme.cpp src/width.cpp src/tables_generated.cpp
)

# ---------------------------------------------------------------------------
# Tests
#
# Plain executables asserting on exit code; no test framework enters the
# dependency graph.
# ---------------------------------------------------------------------------

supra_add_test(utf8_test SOURCES tests/utf8_test.cpp LINK libsupra_width)
supra_add_test(width_test SOURCES tests/width_test.cpp LINK libsupra_width)
supra_add_test(truncate_test SOURCES tests/truncate_test.cpp LINK libsupra_width)
supra_add_test(hangul_arithmetic_test SOURCES tests/hangul_arithmetic_test.cpp LINK libsupra_width)

# UAX #29 conformance reads the vendored fixture. The path is passed as an
# argument so the test does not depend on the working directory, and the fixture
# is vendored so the suite runs offline and pins to the same Unicode version the
# tables were generated from.
if(SUPRA_BUILD_TESTS)
    add_executable(grapheme_conformance_test tests/grapheme_conformance_test.cpp)
    target_link_libraries(grapheme_conformance_test PRIVATE supra_cxx_flags libsupra_width)
    set_target_properties(
        grapheme_conformance_test PROPERTIES RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin"
    )
    add_test(
        NAME grapheme_conformance_test
        COMMAND grapheme_conformance_test
                "${CMAKE_CURRENT_SOURCE_DIR}/data/GraphemeBreakTest.txt"
    )
    set_tests_properties(grapheme_conformance_test PROPERTIES TIMEOUT 120)
endif()
