# T4 - libsupra_sandbox: OS-level process isolation behind one C ABI.
#
# The Linux backend is native (namespaces + Landlock) rather than a bubblewrap
# wrapper. Measured reason, recorded in the header: applying a Landlock ruleset
# and then exec'ing bwrap fails with "Failed to make / slave: Operation not
# permitted", and still fails under a ruleset granting write access everywhere.
# The ordering supra needs - host applies policy, then execs - does not work with
# bwrap at all.

supra_declare_library(
    libsupra_sandbox
    SOURCES src/policy.cpp
            src/self_identity.cpp
            src/landlock.cpp
            src/linux_spawn.cpp
            src/unsupported.cpp
)

# Suites are Linux-only: they assert on real denials from real syscalls. On other
# platforms the backend refuses to spawn, and asserting on a stub would test
# nothing.
if(SUPRA_BUILD_TESTS AND CMAKE_SYSTEM_NAME STREQUAL "Linux")
    supra_add_test(sandbox_policy_test SOURCES tests/sandbox_policy_test.cpp LINK libsupra_sandbox)
    supra_add_test(sandbox_probe_test SOURCES tests/sandbox_probe_test.cpp LINK libsupra_sandbox)
    supra_add_test(sandbox_escape_test SOURCES tests/sandbox_escape_test.cpp LINK libsupra_sandbox)
    supra_add_test(self_identity_test SOURCES tests/self_identity_test.cpp LINK libsupra_sandbox)

    # Escape attempts fork, exec, and wait on real processes, so they are slower
    # than pure computation. Still bounded: a hang is a failure.
    set_tests_properties(sandbox_escape_test PROPERTIES TIMEOUT 300)
endif()
