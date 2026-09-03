// Capability probe, and the refusals that depend on it.
//
// The probe's job is to distinguish "available" from "compiled in but inert".
// Landlock reporting an ABI version proves only the former, and a sandbox that
// reports success while enforcing nothing is worse than no sandbox because the
// caller stops looking.
//
// Environment-dependent by nature: the probe reports what this kernel offers.
// The suite therefore asserts on *internal consistency* and on the refusals,
// never on a specific tier - a fixed expectation would fail on an older kernel
// for the wrong reason.

#include <cstdio>
#include <cstring>
#include <string>

#include <unistd.h>

#include "supra/sandbox.h"
#include "supra/testing.hpp"

namespace {

void reportEnvironment(const supra_sandbox_capabilities& caps) {
    // Printed, not asserted: which tier is available is a property of the machine
    // rather than of the code. Visible in CI logs so a tier regression is
    // noticeable.
    std::fprintf(stderr, "\n--- sandbox capabilities on this host ---\n");
    std::fprintf(stderr, "tier                  : %u\n", static_cast<unsigned>(caps.tier));
    std::fprintf(stderr, "landlock_abi          : %u\n", static_cast<unsigned>(caps.landlock_abi));
    std::fprintf(stderr, "user_namespaces       : %u\n",
                 static_cast<unsigned>(caps.user_namespaces));
    std::fprintf(stderr, "network_isolation     : %u\n",
                 static_cast<unsigned>(caps.network_isolation));
    std::fprintf(stderr, "port_granular_network : %u\n",
                 static_cast<unsigned>(caps.port_granular_network));
    std::fprintf(stderr, "bubblewrap_present    : %u\n",
                 static_cast<unsigned>(caps.bubblewrap_present));
    std::fprintf(stderr, "detail                : %s\n",
                 caps.detail[0] != '\0' ? caps.detail : "(none)");
    std::fprintf(stderr, "----------------------------------------\n\n");
}

void testProbeIsConsistent() {
    supra_sandbox_capabilities caps{};
    supra_sandbox_probe(&caps);
    reportEnvironment(caps);

    SUPRA_CHECK_MSG(caps.tier <= SUPRA_SANDBOX_TIER_APPCONTAINER, "tier is a known value");

    // The LANDLOCK tier means Landlock actually enforced during the probe, so a
    // zero ABI at that tier would be contradictory.
    if (caps.tier == SUPRA_SANDBOX_TIER_LANDLOCK) {
        SUPRA_CHECK_MSG(caps.landlock_abi > 0, "LANDLOCK tier implies a non-zero ABI");
        SUPRA_CHECK_MSG(caps.user_namespaces != 0, "LANDLOCK tier implies user namespaces");
    }

    // Per-port policy is an ABI 4 feature; claiming it below that is incoherent.
    if (caps.port_granular_network != 0) {
        SUPRA_CHECK_MSG(caps.landlock_abi >= 4, "per-port network implies ABI >= 4");
    }

    // Any gap must be explained. An unexplained degradation is undebuggable.
    if (caps.tier < SUPRA_SANDBOX_TIER_LANDLOCK) {
        SUPRA_CHECK_MSG(std::strlen(caps.detail) > 0, "a degraded tier explains itself");
    }

    // Idempotent and cached: repeated probes must agree, or a caller checking
    // twice could get two different answers.
    supra_sandbox_capabilities again{};
    supra_sandbox_probe(&again);
    SUPRA_CHECK_EQ_MSG(again.tier, caps.tier, "probe is stable");
    SUPRA_CHECK_EQ(again.landlock_abi, caps.landlock_abi);

    supra_sandbox_probe(nullptr);  // must not crash
}

/// Filesystem rules on a platform that cannot enforce them must refuse the
/// spawn. Running unconfined while reporting success is the exact failure this
/// library exists to prevent.
void testRefusesUnenforceablePolicy() {
    supra_sandbox_capabilities caps{};
    supra_sandbox_probe(&caps);

    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);
    supra_sandbox_policy_allow(&policy, "/usr", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);

    const char* const argv[] = {"/bin/true", nullptr};
    supra_sandbox_command command{};
    command.program = "/bin/true";
    command.argv = argv;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process process{};
    const int started = supra_sandbox_spawn(&policy, &command, &process);

    if (caps.tier >= SUPRA_SANDBOX_TIER_LANDLOCK) {
        SUPRA_CHECK_EQ_MSG(started, 1, "enforceable policy starts");
        if (started == 1) {
            int status = 0;
            supra_sandbox_wait(&process, 5000, &status);
        }
    } else {
        SUPRA_CHECK_EQ_MSG(started, 0, "unenforceable filesystem policy refuses to run");
        SUPRA_CHECK_MSG(std::strstr(process.error, "cannot enforce") != nullptr,
                        std::string("refusal names the reason: ") + process.error);
    }
}

/// A required tier above what the platform offers must refuse, so a caller can
/// demand enforcement rather than hope for it.
void testRequiredTierHonoured() {
    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);
    // Above every real tier, so this refuses on any platform.
    policy.required_tier = SUPRA_SANDBOX_TIER_APPCONTAINER;

    const char* const argv[] = {"/bin/true", nullptr};
    supra_sandbox_command command{};
    command.program = "/bin/true";
    command.argv = argv;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process process{};
    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, &command, &process), 0,
                       "unreachable required_tier refuses");
    SUPRA_CHECK_MSG(std::strstr(process.error, "requires tier") != nullptr,
                    std::string("refusal names the tier: ") + process.error);
}

/// A filesystem policy on a platform that cannot enforce it must refuse.
///
/// This branch is unreachable on a machine that supports Landlock, so a mutation
/// deleting it survived every test - the refusal is exactly what protects a user
/// on an older kernel, and it was going untested on the only kernel available
/// here. The tier is therefore pinned downward to reach the branch.
///
/// Without the refusal the command would run completely unconfined while the
/// caller was told the policy applied, which is the failure mode this library
/// exists to prevent.
void testFilesystemPolicyRefusedOnWeakTier() {
    supra_sandbox_force_tier_for_testing(SUPRA_SANDBOX_TIER_NAMESPACES);

    supra_sandbox_capabilities caps{};
    supra_sandbox_probe(&caps);
    SUPRA_CHECK_EQ_MSG(caps.tier, std::uint8_t{SUPRA_SANDBOX_TIER_NAMESPACES},
                       "override takes effect");

    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);
    supra_sandbox_policy_allow(&policy, "/usr", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);

    const char* const argv[] = {"/bin/true", nullptr};
    supra_sandbox_command command{};
    command.program = "/bin/true";
    command.argv = argv;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process process{};
    const int started = supra_sandbox_spawn(&policy, &command, &process);

    SUPRA_CHECK_EQ_MSG(started, 0,
                       "a filesystem policy that cannot be enforced refuses to run");
    SUPRA_CHECK_MSG(std::strstr(process.error, "cannot enforce") != nullptr,
                    std::string("refusal names the reason: ") + process.error);

    // A policy with no filesystem rules is still permitted at this tier: process
    // and network isolation remain real, and refusing would be over-strict.
    supra_sandbox_policy bare{};
    supra_sandbox_policy_init(&bare);
    supra_sandbox_process bare_process{};
    const int bare_started = supra_sandbox_spawn(&bare, &command, &bare_process);
    SUPRA_CHECK_EQ_MSG(bare_started, 1,
                       std::string("a rule-free policy still runs at NAMESPACES tier: ") +
                           bare_process.error);
    if (bare_started == 1) {
        int status = 0;
        supra_sandbox_wait(&bare_process, 5000, &status);
        SUPRA_CHECK_EQ_MSG(bare_process.tier, std::uint8_t{SUPRA_SANDBOX_TIER_NAMESPACES},
                           "reported tier matches what was applied");
    }

    supra_sandbox_force_tier_for_testing(SUPRA_SANDBOX_TIER_UNSET);

    // Restored: the real tier is reported again.
    supra_sandbox_capabilities restored{};
    supra_sandbox_probe(&restored);
    SUPRA_CHECK_MSG(restored.tier >= caps.tier, "clearing the override restores the real tier");
}

/// Per-port network policy on a platform without support must refuse rather than
/// silently granting full network access.
///
/// Also driven through the override, since this kernel does support per-port
/// policy and the refusal would otherwise never execute.
void testPortPolicyRefusedOnWeakTier() {
    supra_sandbox_force_tier_for_testing(SUPRA_SANDBOX_TIER_NAMESPACES);

    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);
    supra_sandbox_policy_allow_port(&policy, 443);

    const char* const argv[] = {"/bin/true", nullptr};
    supra_sandbox_command command{};
    command.program = "/bin/true";
    command.argv = argv;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process process{};
    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, &command, &process), 0,
                       "an unenforceable port policy refuses, never widens");
    SUPRA_CHECK_MSG(std::strstr(process.error, "per-port") != nullptr,
                    std::string("refusal names the reason: ") + process.error);

    supra_sandbox_force_tier_for_testing(SUPRA_SANDBOX_TIER_UNSET);
}

/// Tier NONE means no isolation is possible, so nothing may run.
void testNoTierRefusesEverything() {
    supra_sandbox_force_tier_for_testing(SUPRA_SANDBOX_TIER_NONE);

    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);

    const char* const argv[] = {"/bin/true", nullptr};
    supra_sandbox_command command{};
    command.program = "/bin/true";
    command.argv = argv;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process process{};
    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, &command, &process), 0,
                       "tier NONE refuses even an empty policy");
    SUPRA_CHECK_MSG(std::strlen(process.error) > 0, "the refusal explains itself");

    supra_sandbox_force_tier_for_testing(SUPRA_SANDBOX_TIER_UNSET);
}

/// Per-port network policy on a kernel without ABI 4 must refuse rather than
/// silently granting full network access.
///
/// The policy also grants the paths the binary needs. A port-only policy fails at
/// `execve` with EACCES - correctly, because Landlock is deny-by-default, so no
/// rules means no access including to the program itself - and that failure says
/// nothing about port handling.
void testPortPolicyRefusedWithoutSupport() {
    supra_sandbox_capabilities caps{};
    supra_sandbox_probe(&caps);

    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);
    supra_sandbox_policy_allow_port(&policy, 443);
    supra_sandbox_policy_allow(&policy, "/usr", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    supra_sandbox_policy_allow(&policy, "/bin", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    supra_sandbox_policy_allow(&policy, "/lib", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    if (::access("/lib64", F_OK) == 0) {
        supra_sandbox_policy_allow(&policy, "/lib64", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    }

    const char* const argv[] = {"/bin/true", nullptr};
    supra_sandbox_command command{};
    command.program = "/bin/true";
    command.argv = argv;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process process{};
    const int started = supra_sandbox_spawn(&policy, &command, &process);

    if (caps.port_granular_network != 0) {
        SUPRA_CHECK_EQ_MSG(started, 1,
                           std::string("supported port policy starts: ") + process.error);
        if (started == 1) {
            int status = 0;
            supra_sandbox_wait(&process, 5000, &status);
        }
    } else {
        SUPRA_CHECK_EQ_MSG(started, 0, "unsupported port policy refuses, never widens");
        SUPRA_CHECK_MSG(std::strstr(process.error, "per-port") != nullptr,
                        std::string("refusal names the reason: ") + process.error);
    }
}

/// A policy that does not permit the program denies executing it.
///
/// Fail-closed working as intended, tested explicitly so the behaviour is not
/// later mistaken for a bug: Landlock is deny-by-default, so "no rules" means "no
/// access", not "no restrictions".
void testPolicyMustPermitTheProgram() {
    supra_sandbox_capabilities caps{};
    supra_sandbox_probe(&caps);
    if (caps.tier < SUPRA_SANDBOX_TIER_LANDLOCK) {
        return;
    }

    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);
    // A rule exists, so Landlock applies - but it does not cover /bin/true.
    supra_sandbox_policy_allow(&policy, "/dev/null", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_WRITE);

    const char* const argv[] = {"/bin/true", nullptr};
    supra_sandbox_command command{};
    command.program = "/bin/true";
    command.argv = argv;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process process{};
    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, &command, &process), 0,
                       "a policy that does not permit the program refuses to run it");
    SUPRA_CHECK_MSG(std::strstr(process.error, "execve") != nullptr,
                    std::string("failure is reported at exec: ") + process.error);
}

/// The probe must test a real denial, not trust the reported ABI version.
///
/// A non-zero Landlock ABI proves only that the LSM is compiled in. It can still
/// be absent from the boot-time LSM list, in which case the syscalls succeed and
/// nothing is enforced - and a sandbox reporting success while enforcing nothing
/// is worse than none, because the caller stops looking.
///
/// Checked behaviourally: when the probe claims the LANDLOCK tier, a spawned
/// process must actually be denied. Written after a surviving mutation showed
/// that replacing the enforcement check with a bare version check went
/// undetected - on a kernel where Landlock does work both produce the same tier,
/// so only asserting the *denial* distinguishes them.
void testLandlockTierImpliesRealEnforcement() {
    supra_sandbox_capabilities caps{};
    supra_sandbox_probe(&caps);

    if (caps.tier < SUPRA_SANDBOX_TIER_LANDLOCK) {
        // The claim is not being made, so there is nothing to verify.
        return;
    }

    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);
    policy.required_tier = SUPRA_SANDBOX_TIER_LANDLOCK;
    supra_sandbox_policy_allow(&policy, "/usr", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    supra_sandbox_policy_allow(&policy, "/bin", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    supra_sandbox_policy_allow(&policy, "/lib", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    if (::access("/lib64", F_OK) == 0) {
        supra_sandbox_policy_allow(&policy, "/lib64", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    }
    // /etc is deliberately absent, so reading from it must fail.

    int output_pipe[2] = {-1, -1};
    SUPRA_CHECK(::pipe(output_pipe) == 0);

    const char* const argv[] = {"/bin/sh", "-c", "cat /etc/hostname 2>&1", nullptr};
    static const char* const env[] = {"PATH=/usr/bin:/bin", nullptr};

    supra_sandbox_command command{};
    command.program = "/bin/sh";
    command.argv = argv;
    command.envp = env;
    command.stdin_fd = -1;
    command.stdout_fd = output_pipe[1];
    command.stderr_fd = output_pipe[1];

    supra_sandbox_process process{};
    const bool started = supra_sandbox_spawn(&policy, &command, &process) == 1;
    ::close(output_pipe[1]);

    std::string output;
    if (started) {
        char buffer[1024];
        ssize_t received = 0;
        while ((received = ::read(output_pipe[0], buffer, sizeof buffer)) > 0) {
            output.append(buffer, static_cast<std::size_t>(received));
        }
        int status = 0;
        supra_sandbox_wait(&process, 10000, &status);
    }
    ::close(output_pipe[0]);

    SUPRA_CHECK_MSG(started, std::string("spawn succeeds at the claimed tier: ") + process.error);
    SUPRA_CHECK_MSG(output.find("Permission denied") != std::string::npos,
                    "a LANDLOCK-tier claim means access is genuinely denied: " + output);
}

/// NET_PORTS with an empty port list is a caller error, not "allow everything".
void testEmptyPortListRejected() {
    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);
    policy.network = SUPRA_SANDBOX_NET_PORTS;  // set directly, no ports added

    const char* const argv[] = {"/bin/true", nullptr};
    supra_sandbox_command command{};
    command.program = "/bin/true";
    command.argv = argv;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process process{};
    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, &command, &process), 0,
                       "NET_PORTS with no ports is refused");
}

}  // namespace

int main() {
    testProbeIsConsistent();
    testRefusesUnenforceablePolicy();
    testRequiredTierHonoured();
    testFilesystemPolicyRefusedOnWeakTier();
    testPortPolicyRefusedOnWeakTier();
    testNoTierRefusesEverything();
    testPortPolicyRefusedWithoutSupport();
    testPolicyMustPermitTheProgram();
    testLandlockTierImpliesRealEnforcement();
    testEmptyPortListRejected();
    return supra::test::finish("sandbox_probe_test");
}
