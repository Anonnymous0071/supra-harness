// Native macOS/Windows sandbox enforcement and process-lifecycle tests.

#include <atomic>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <thread>

#if defined(__APPLE__)
#include <arpa/inet.h>
#include <cerrno>
#include <fcntl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>
#elif defined(_WIN32)
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <direct.h>
#include <io.h>
#endif

#include "supra/sandbox.h"
#include "supra/testing.hpp"

namespace {

#if defined(__APPLE__)
constexpr const char* kShell = "/bin/sh";
constexpr const char* kAllowedDir = "/tmp/supra-native-allowed";
constexpr const char* kAllowed = "/tmp/supra-native-allowed/output";
constexpr const char* kDenied = "/tmp/supra-native-denied";
constexpr const char* kMarker = "/tmp/supra-native-allowed/marker";
constexpr const char* kEnvironment[] = {"PATH=/usr/bin:/bin", nullptr};
#elif defined(_WIN32)
constexpr const char* kAllowedDir = "C:\\Windows\\Temp\\supra-native-allowed";
constexpr const char* kAllowed = "C:\\Windows\\Temp\\supra-native-allowed\\output";
constexpr const char* kDenied = "C:\\Windows\\Temp\\supra-native-denied";
constexpr const char* kMarker = "C:\\Windows\\Temp\\supra-native-allowed\\marker";
#endif

void removePath(const char* path) {
#if defined(__APPLE__)
    ::unlink(path);
#elif defined(_WIN32)
    ::DeleteFileA(path);
#endif
}

bool ensureAllowedDirectory() {
#if defined(__APPLE__)
    return ::mkdir(kAllowedDir, 0700) == 0 || errno == EEXIST;
#elif defined(_WIN32)
    return ::CreateDirectoryA(kAllowedDir, nullptr) != FALSE ||
           ::GetLastError() == ERROR_ALREADY_EXISTS;
#endif
}

bool pathExists(const char* path) {
#if defined(__APPLE__)
    return ::access(path, F_OK) == 0;
#elif defined(_WIN32)
    return ::GetFileAttributesA(path) != INVALID_FILE_ATTRIBUTES;
#endif
}

bool writeCanary(const char* path) {
#if defined(__APPLE__)
    const int fd = ::open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) {
        return false;
    }
    const bool ok = ::write(fd, "canary", 6) == 6;
    ::close(fd);
    return ok;
#elif defined(_WIN32)
    HANDLE file = ::CreateFileA(path, GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS,
                                FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file == INVALID_HANDLE_VALUE) {
        return false;
    }
    DWORD written = 0;
    const bool ok = ::WriteFile(file, "canary", 6, &written, nullptr) != FALSE && written == 6;
    ::CloseHandle(file);
    return ok;
#endif
}

std::string shellPath() {
#if defined(__APPLE__)
    return kShell;
#elif defined(_WIN32)
    char system[MAX_PATH]{};
    const UINT length = ::GetSystemDirectoryA(system, MAX_PATH);
    if (length == 0 || length >= MAX_PATH) {
        return {};
    }
    return std::string(system) + "\\cmd.exe";
#endif
}

supra_sandbox_policy basePolicy(const std::string& shell) {
    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);
#if defined(__APPLE__)
    supra_sandbox_policy_allow(&policy, "/bin", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    supra_sandbox_policy_allow(&policy, "/usr/lib", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    supra_sandbox_policy_allow(&policy, "/dev/null", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_WRITE);
#elif defined(_WIN32)
    supra_sandbox_policy_allow(&policy, shell.c_str(),
                               SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
#endif
    supra_sandbox_policy_allow(&policy, kAllowedDir,
                               SUPRA_SANDBOX_READ | SUPRA_SANDBOX_WRITE |
                                   SUPRA_SANDBOX_MANAGE);
    return policy;
}

bool runScript(supra_sandbox_policy& policy, const std::string& shell,
               const std::string& script, int* status,
               supra_sandbox_process* process = nullptr) {
#if defined(__APPLE__)
    const char* const argv[] = {shell.c_str(), "-c", script.c_str(), nullptr};
#elif defined(_WIN32)
    const char* const argv[] = {shell.c_str(), "/d", "/s", "/c", script.c_str(), nullptr};
#endif
    supra_sandbox_command command{};
    command.program = shell.c_str();
    command.argv = argv;
#if defined(__APPLE__)
    command.envp = kEnvironment;
#endif
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process local{};
    supra_sandbox_process* launched = process != nullptr ? process : &local;
    if (supra_sandbox_spawn(&policy, &command, launched) != 1) {
        std::fprintf(stderr, "spawn failed: %s\n", launched->error);
        return false;
    }
    if (status == nullptr) {
        return true;
    }
    return supra_sandbox_wait(launched, 10000, status) == 1;
}

void testProbe() {
    supra_sandbox_capabilities caps{};
    supra_sandbox_probe(&caps);
#if defined(__APPLE__)
    SUPRA_CHECK_EQ_MSG(caps.tier, std::uint8_t{SUPRA_SANDBOX_TIER_SBPL}, caps.detail);
#elif defined(_WIN32)
    SUPRA_CHECK_EQ_MSG(caps.tier, std::uint8_t{SUPRA_SANDBOX_TIER_APPCONTAINER}, caps.detail);
#endif
    SUPRA_CHECK_MSG(caps.network_isolation != 0, "native tier isolates network");
    SUPRA_CHECK_EQ_MSG(caps.port_granular_network, std::uint8_t{0},
                       "native backend refuses unsupported port lists");
}

void testFilesystemEnforcement() {
    SUPRA_CHECK(ensureAllowedDirectory());
    removePath(kAllowed);
    removePath(kDenied);
    SUPRA_CHECK(writeCanary(kDenied));

    const std::string shell = shellPath();
    SUPRA_CHECK_MSG(!shell.empty(), "native shell resolves");
    auto policy = basePolicy(shell);

#if defined(__APPLE__)
    const std::string allowed = std::string("printf ok > '") + kAllowed + "'";
    const std::string denied = std::string("printf escaped > '") + kDenied + "'";
#elif defined(_WIN32)
    const std::string allowed = std::string("echo ok>") + kAllowed;
    const std::string denied = std::string("echo escaped>") + kDenied;
#endif

    int status = -1;
    SUPRA_CHECK(runScript(policy, shell, allowed, &status));
    SUPRA_CHECK_EQ_MSG(status, 0, "allowed path is writable");
    SUPRA_CHECK(pathExists(kAllowed));

    status = -1;
    SUPRA_CHECK(runScript(policy, shell, denied, &status));
    SUPRA_CHECK_MSG(status != 0, "unlisted path write is denied");

    removePath(kAllowed);
    removePath(kDenied);
}

void testNetworkEnforcement() {
#if defined(__APPLE__)
    const int listener = ::socket(AF_INET, SOCK_STREAM, 0);
    SUPRA_CHECK(listener >= 0);
    sockaddr_in address{};
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    SUPRA_CHECK(::bind(listener, reinterpret_cast<sockaddr*>(&address), sizeof address) == 0);
    SUPRA_CHECK(::listen(listener, 2) == 0);
    socklen_t length = sizeof address;
    SUPRA_CHECK(::getsockname(listener, reinterpret_cast<sockaddr*>(&address), &length) == 0);
    const std::string port = std::to_string(ntohs(address.sin_port));

    auto policy = basePolicy(kShell);
    SUPRA_CHECK(supra_sandbox_policy_allow(
                    &policy, "/usr/bin", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE) == 1);
    const char* const argv[] = {"/usr/bin/nc", "-z", "127.0.0.1", port.c_str(), nullptr};
    supra_sandbox_command command{};
    command.program = argv[0];
    command.argv = argv;
    command.envp = kEnvironment;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process process{};
    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, &command, &process), 1,
                       "no-network probe starts");
    int status = -1;
    SUPRA_CHECK_EQ_MSG(supra_sandbox_wait(&process, 10000, &status), 1,
                       "no-network probe exits");
    SUPRA_CHECK_MSG(status != 0, "TCP connect is denied by the default network policy");

    policy.network = SUPRA_SANDBOX_NET_FULL;
    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, &command, &process), 1,
                       "full-network probe starts");
    SUPRA_CHECK_EQ_MSG(supra_sandbox_wait(&process, 10000, &status), 1,
                       "full-network probe exits");
    SUPRA_CHECK_EQ_MSG(status, 0, "full network policy permits TCP connect");
    ::close(listener);
#elif defined(_WIN32)
    // The AppContainer capability is exercised by its native hosted test runner;
    // per-port widening is tested separately below.
#endif
}

void testConcurrentProbeAndForcedTier() {
#if defined(__APPLE__)
    constexpr int kThreadCount = 8;
    constexpr int kIterations = 1000;
    std::atomic<bool> consistent{true};
    std::thread threads[kThreadCount];
    for (int thread = 0; thread < kThreadCount; ++thread) {
        threads[thread] = std::thread([thread, &consistent] {
            for (int iteration = 0; iteration < kIterations; ++iteration) {
                supra_sandbox_force_tier_for_testing(
                    ((thread + iteration) & 1) == 0 ? SUPRA_SANDBOX_TIER_UNSET
                                                    : SUPRA_SANDBOX_TIER_NONE);
                supra_sandbox_capabilities caps{};
                supra_sandbox_probe(&caps);
                if (caps.tier != SUPRA_SANDBOX_TIER_SBPL &&
                    caps.tier != SUPRA_SANDBOX_TIER_NONE) {
                    consistent.store(false, std::memory_order_relaxed);
                }
            }
        });
    }
    for (auto& thread : threads) {
        thread.join();
    }
    supra_sandbox_force_tier_for_testing(SUPRA_SANDBOX_TIER_UNSET);
    SUPRA_CHECK_MSG(consistent.load(std::memory_order_relaxed),
                    "concurrent probe and forced-tier access stays internally consistent");
    supra_sandbox_capabilities caps{};
    supra_sandbox_probe(&caps);
    SUPRA_CHECK_EQ_MSG(caps.tier, std::uint8_t{SUPRA_SANDBOX_TIER_SBPL},
                       "clearing a concurrent override restores the cached native tier");
#endif
}

void testUnsupportedIsolationRefused() {
#if defined(__APPLE__)
    const std::string shell = shellPath();
    const char* const argv[] = {shell.c_str(), "-c", "exit 0", nullptr};
    supra_sandbox_command command{};
    command.program = shell.c_str();
    command.argv = argv;
    command.envp = kEnvironment;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    for (int mode = 0; mode < 2; ++mode) {
        auto policy = basePolicy(shell);
        policy.isolate_processes = mode == 0 ? 1U : 0U;
        policy.isolate_ipc = mode == 1 ? 1U : 0U;
        supra_sandbox_process process{};
        SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, &command, &process), 0,
                           "unsupported macOS namespace isolation is refused");
        SUPRA_CHECK_MSG(std::strstr(process.error, "process or IPC") != nullptr,
                        std::string("refusal names unsupported isolation: ") + process.error);
    }
#endif
}

void testPortPolicyRefused() {
    const std::string shell = shellPath();
    auto policy = basePolicy(shell);
    SUPRA_CHECK(supra_sandbox_policy_allow_port(&policy, 443) == 1);

#if defined(__APPLE__)
    const char* const argv[] = {shell.c_str(), "-c", "exit 0", nullptr};
#elif defined(_WIN32)
    const char* const argv[] = {shell.c_str(), "/d", "/c", "exit 0", nullptr};
#endif
    supra_sandbox_command command{};
    command.program = shell.c_str();
    command.argv = argv;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process process{};
    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, &command, &process), 0,
                       "per-port policy is refused rather than widened");
    SUPRA_CHECK_MSG(std::strstr(process.error, "per-port") != nullptr,
                    std::string("refusal names unsupported granularity: ") + process.error);
}

void testTimeoutAndCleanup() {
    SUPRA_CHECK(ensureAllowedDirectory());
    removePath(kMarker);
    const std::string shell = shellPath();
    auto policy = basePolicy(shell);

#if defined(__APPLE__)
    const std::string script = std::string("(sleep 2; printf leaked > '") +
                               kMarker + "') & sleep 30";
#elif defined(_WIN32)
    const std::string script = std::string("start \"\" /b cmd /d /c ") +
                               "\"ping -n 3 127.0.0.1>nul & echo leaked>" +
                               kMarker + "\" & ping -n 31 127.0.0.1>nul";
#endif
    supra_sandbox_process process{};
    SUPRA_CHECK(runScript(policy, shell, script, nullptr, &process));

    int status = -1;
    SUPRA_CHECK_EQ_MSG(supra_sandbox_wait(&process, 50, &status), 0,
                       "short wait times out while tree is alive");
    SUPRA_CHECK_MSG(supra_sandbox_kill(&process, 50) == 1,
                    "kill terminates and reaps the whole native process tree");

    std::this_thread::sleep_for(std::chrono::milliseconds(2500));
    SUPRA_CHECK_MSG(!pathExists(kMarker), "descendant did not survive cleanup");
    removePath(kMarker);
}

}  // namespace

int main() {
    testProbe();
    testConcurrentProbeAndForcedTier();
    testFilesystemEnforcement();
    testNetworkEnforcement();
    testUnsupportedIsolationRefused();
    testPortPolicyRefused();
    testTimeoutAndCleanup();
    return supra::test::finish("sandbox_native_test");
}
