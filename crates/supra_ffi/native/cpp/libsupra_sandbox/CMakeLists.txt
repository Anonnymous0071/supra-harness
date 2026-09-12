# T4 - libsupra_sandbox: OS-level process isolation behind one C ABI.
#
# Linux uses namespaces + Landlock, macOS uses sandbox_init with generated
# SBPL, and Windows uses AppContainer with a job object.

set(SUPRA_SANDBOX_PLATFORM_SOURCE src/unsupported.cpp)
if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
    set(SUPRA_SANDBOX_PLATFORM_SOURCE src/linux_spawn.cpp)
elseif(APPLE)
    set(SUPRA_SANDBOX_PLATFORM_SOURCE src/macos_spawn.cpp)
elseif(WIN32)
    set(SUPRA_SANDBOX_PLATFORM_SOURCE src/windows_spawn.cpp)
endif()

supra_declare_library(
    libsupra_sandbox
    SOURCES src/policy.cpp
            src/self_identity.cpp
            ${SUPRA_SANDBOX_PLATFORM_SOURCE}
)

if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
    target_sources(libsupra_sandbox PRIVATE src/landlock.cpp)
endif()

if(WIN32)
    target_link_libraries(libsupra_sandbox PRIVATE advapi32 userenv)
endif()

if(SUPRA_BUILD_TESTS)
    supra_add_test(sandbox_policy_test SOURCES tests/sandbox_policy_test.cpp LINK libsupra_sandbox)

    if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
        supra_add_test(self_identity_test SOURCES tests/self_identity_test.cpp LINK libsupra_sandbox)
        supra_add_test(sandbox_probe_test SOURCES tests/sandbox_probe_test.cpp LINK libsupra_sandbox)
        supra_add_test(sandbox_escape_test SOURCES tests/sandbox_escape_test.cpp LINK libsupra_sandbox)
        set_tests_properties(sandbox_escape_test PROPERTIES TIMEOUT 300)
    elseif(APPLE OR WIN32)
        supra_add_test(sandbox_native_test SOURCES tests/sandbox_native_test.cpp LINK libsupra_sandbox)
        set_tests_properties(sandbox_native_test PROPERTIES TIMEOUT 300)
    endif()
endif()
