// Policy construction and validation. Pure computation, no processes.

#include <cstring>
#include <string>

#include "supra/sandbox.h"
#include "supra/testing.hpp"

namespace {

/// Zeroing must be the most restrictive state, so a caller that forgets a field
/// gets less access rather than more.
void testInitIsRestrictive() {
    supra_sandbox_policy policy{};
    std::memset(&policy, 0xFF, sizeof policy);  // poison first
    supra_sandbox_policy_init(&policy);

    SUPRA_CHECK_EQ_MSG(policy.path_count, std::uint8_t{0}, "no paths by default");
    SUPRA_CHECK_EQ_MSG(policy.network, std::uint8_t{SUPRA_SANDBOX_NET_NONE},
                       "no network by default");
    SUPRA_CHECK_EQ(policy.port_count, std::uint8_t{0});
    SUPRA_CHECK_EQ(policy.max_processes, 0u);
    SUPRA_CHECK_EQ_MSG(policy.required_tier, std::uint8_t{0},
                       "required_tier 0 means accept what is available");
}

/// A relative path is refused at construction: resolving it would depend on the
/// caller's working directory at an unpredictable moment, and a sandbox whose
/// scope shifts with chdir is not a boundary.
void testRelativePathRejected() {
    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);

    SUPRA_CHECK_EQ_MSG(supra_sandbox_policy_allow(&policy, "relative/path", SUPRA_SANDBOX_READ), 0,
                       "relative path refused");
    SUPRA_CHECK_EQ_MSG(supra_sandbox_policy_allow(&policy, "./here", SUPRA_SANDBOX_READ), 0,
                       "dot-relative refused");
    SUPRA_CHECK_EQ_MSG(supra_sandbox_policy_allow(&policy, "../up", SUPRA_SANDBOX_READ), 0,
                       "parent-relative refused");
    SUPRA_CHECK_EQ_MSG(supra_sandbox_policy_allow(&policy, nullptr, SUPRA_SANDBOX_READ), 0,
                       "null path refused");
    SUPRA_CHECK_EQ_MSG(policy.path_count, std::uint8_t{0}, "nothing recorded");

    SUPRA_CHECK_EQ_MSG(supra_sandbox_policy_allow(&policy, "/absolute", SUPRA_SANDBOX_READ), 1,
                       "absolute path accepted");
    SUPRA_CHECK_EQ(policy.path_count, std::uint8_t{1});
}

void testZeroAccessRejected() {
    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);
    SUPRA_CHECK_EQ_MSG(supra_sandbox_policy_allow(&policy, "/tmp", 0), 0,
                       "a rule granting nothing is a caller error");
    SUPRA_CHECK_EQ(policy.path_count, std::uint8_t{0});
}

/// MANAGE without WRITE is incoherent - creating a file is a write - so it is
/// normalised at construction rather than left for each backend to infer.
void testManageImpliesWrite() {
    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);

    SUPRA_CHECK_EQ(supra_sandbox_policy_allow(&policy, "/tmp", SUPRA_SANDBOX_MANAGE), 1);
    SUPRA_CHECK_MSG((policy.paths[0].access & SUPRA_SANDBOX_WRITE) != 0,
                    "MANAGE implies WRITE");
    SUPRA_CHECK_MSG((policy.paths[0].access & SUPRA_SANDBOX_MANAGE) != 0, "MANAGE retained");
}

void testTableFull() {
    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);

    for (int i = 0; i < SUPRA_SANDBOX_MAX_PATHS; ++i) {
        SUPRA_CHECK_EQ_MSG(supra_sandbox_policy_allow(&policy, "/tmp", SUPRA_SANDBOX_READ), 1,
                           "rule " + std::to_string(i) + " accepted");
    }
    SUPRA_CHECK_EQ_MSG(supra_sandbox_policy_allow(&policy, "/tmp", SUPRA_SANDBOX_READ), 0,
                       "refuses past the limit rather than overflowing");
    SUPRA_CHECK_EQ(policy.path_count, std::uint8_t{SUPRA_SANDBOX_MAX_PATHS});
}

void testPortPolicy() {
    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);

    SUPRA_CHECK_EQ(supra_sandbox_policy_allow_port(&policy, 443), 1);
    SUPRA_CHECK_EQ_MSG(policy.network, std::uint8_t{SUPRA_SANDBOX_NET_PORTS},
                       "adding a port switches the mode");
    SUPRA_CHECK_EQ(policy.port_count, std::uint8_t{1});
    SUPRA_CHECK_EQ(policy.allowed_ports[0], std::uint16_t{443});

    // Idempotent: a duplicate must not consume a second slot.
    SUPRA_CHECK_EQ(supra_sandbox_policy_allow_port(&policy, 443), 1);
    SUPRA_CHECK_EQ_MSG(policy.port_count, std::uint8_t{1}, "duplicate port not stored twice");

    SUPRA_CHECK_EQ(supra_sandbox_policy_allow_port(&policy, 80), 1);
    SUPRA_CHECK_EQ(policy.port_count, std::uint8_t{2});

    for (int i = 0; i < SUPRA_SANDBOX_MAX_PORTS; ++i) {
        supra_sandbox_policy_allow_port(&policy, static_cast<std::uint16_t>(9000 + i));
    }
    SUPRA_CHECK_EQ_MSG(policy.port_count, std::uint8_t{SUPRA_SANDBOX_MAX_PORTS},
                       "port table capped");
}

void testNullSafety() {
    // Must not crash. A library that segfaults on a null argument turns a caller
    // bug into a crashed session.
    supra_sandbox_policy_init(nullptr);
    SUPRA_CHECK_EQ(supra_sandbox_policy_allow(nullptr, "/tmp", SUPRA_SANDBOX_READ), 0);
    SUPRA_CHECK_EQ(supra_sandbox_policy_allow_port(nullptr, 443), 0);
}

/// Spawn must refuse a malformed command rather than proceeding.
void testSpawnRejectsBadCommand() {
    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);

    supra_sandbox_process process{};

    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, nullptr, &process), 0, "null command refused");
    SUPRA_CHECK_MSG(std::strlen(process.error) > 0, "failure explains itself");

    const char* const argv[] = {"echo", nullptr};
    supra_sandbox_command command{};
    command.program = nullptr;
    command.argv = argv;
    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, &command, &process), 0, "null program refused");

    command.program = "/bin/echo";
    command.argv = nullptr;
    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, &command, &process), 0, "null argv refused");

    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(nullptr, &command, &process), 0, "null policy refused");
}

}  // namespace

int main() {
    testInitIsRestrictive();
    testRelativePathRejected();
    testZeroAccessRejected();
    testManageImpliesWrite();
    testTableFull();
    testPortPolicy();
    testNullSafety();
    testSpawnRejectsBadCommand();
    return supra::test::finish("sandbox_policy_test");
}
