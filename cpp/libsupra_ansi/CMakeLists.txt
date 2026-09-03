# T3 - libsupra_ansi: escape sequence parsing, SGR state, style-safe truncation.
#
# Depends on libsupra_width (T2) for every cell-width decision; this library
# never computes a width itself.

supra_declare_library(
    libsupra_ansi
    SOURCES src/scanner.cpp src/style.cpp src/truncate.cpp
)

target_link_libraries(libsupra_ansi PUBLIC libsupra_width)

supra_add_test(scanner_test SOURCES tests/scanner_test.cpp LINK libsupra_ansi)
supra_add_test(style_test SOURCES tests/style_test.cpp LINK libsupra_ansi)
supra_add_test(ansi_truncate_test SOURCES tests/ansi_truncate_test.cpp LINK libsupra_ansi)
supra_add_test(streaming_test SOURCES tests/streaming_test.cpp LINK libsupra_ansi)
