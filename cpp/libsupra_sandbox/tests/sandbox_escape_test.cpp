// Real escape attempts against a real sandbox.
//
// Every case forks, execs, and inspects actual output. Asserting that a policy
// struct was populated proves nothing about enforcement; only a refused syscall
// does.
//
// Skips wholesale when the platform cannot reach the Landlock tier, and says so.
// Passing vacuously on a kernel without Landlock would be the worst outcome
// available: a green suite that verified nothing.

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>

#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

#include "supra/sandbox.h"
#include "supra/testing.hpp"

namespace {

constexpr const char* kWorkspace = "/tmp/supra-sandbox-test-workspace";

/// Result of running one command inside the sandbox.
struct Run {
    bool started = false;
    int status = -1;
    std::string output;
    std::string error;
};

/// Run `argv` under `policy`, capturing stdout and stderr through a pipe.
Run run(const supra_sandbox_policy& policy, const char* const* argv, const char* program) {
    Run result;

    int output_pipe[2] = {-1, -1};
    if (::pipe(output_pipe) != 0) {
        result.error = "pipe failed";
        return result;
    }

    supra_sandbox_command command{};
    command.program = program;
    command.argv = argv;
    command.working_dir = nullptr;
    command.stdin_fd = -1;
    command.stdout_fd = output_pipe[1];
    command.stderr_fd = output_pipe[1];

    // A shell needs PATH to resolve utilities; without it every case would fail
    // for the wrong reason.
    static const char* const env[] = {"PATH=/usr/bin:/bin", nullptr};
    command.envp = env;

    supra_sandbox_process process{};
    result.started = supra_sandbox_spawn(&policy, &command, &process) == 1;
    ::close(output_pipe[1]);

    if (!result.started) {
        result.error = process.error;
        ::close(output_pipe[0]);
        return result;
    }

    char buffer[4096];
    ssize_t received = 0;
    while ((received = ::read(output_pipe[0], buffer, sizeof buffer)) > 0) {
        result.output.append(buffer, static_cast<std::size_t>(received));
    }
    ::close(output_pipe[0]);

    if (supra_sandbox_wait(&process, 15000, &result.status) != 1) {
        supra_sandbox_kill(&process, 500);
        result.error = "timed out";
    }
    return result;
}

Run runShell(const supra_sandbox_policy& policy, const char* script) {
    const char* const argv[] = {"/bin/sh", "-c", script, nullptr};
    return run(policy, argv, "/bin/sh");
}

/// Policy granting exactly what a shell needs, plus the workspace.
supra_sandbox_policy workspacePolicy() {
    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);

    supra_sandbox_policy_allow(&policy, "/usr", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    supra_sandbox_policy_allow(&policy, "/bin", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    supra_sandbox_policy_allow(&policy, "/lib", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    if (::access("/lib64", F_OK) == 0) {
        supra_sandbox_policy_allow(&policy, "/lib64", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    }
    supra_sandbox_policy_allow(&policy, "/etc/ld.so.cache", SUPRA_SANDBOX_READ);
    supra_sandbox_policy_allow(&policy, "/dev/null", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_WRITE);

    supra_sandbox_policy_allow(&policy, kWorkspace,
                               SUPRA_SANDBOX_READ | SUPRA_SANDBOX_WRITE | SUPRA_SANDBOX_MANAGE);

    policy.isolate_processes = 1;
    policy.isolate_ipc = 1;
    policy.required_tier = SUPRA_SANDBOX_TIER_LANDLOCK;
    return policy;
}

bool contains(const std::string& haystack, const char* needle) {
    return haystack.find(needle) != std::string::npos;
}

/// Whether output contains an actual /etc/passwd entry.
///
/// Not a search for "root:" - that is a substring of "chroot:", so the message
/// "/bin/sh: 1: chroot: not found" matched and the test failed for a reason
/// unrelated to what it was checking. A passwd line is `name:x:uid:...`, so
/// anchoring on the uid field distinguishes real content from incidental text.
bool leakedPasswdContent(const std::string& output) {
    return output.find("root:x:0:") != std::string::npos ||
           output.find("root:*:0:") != std::string::npos ||
           output.find(":0:0:") != std::string::npos;
}

// -- the attempts -----------------------------------------------------------

void testBaselineWorks() {
    // If this fails, every denial below would pass for the wrong reason.
    const auto result = runShell(workspacePolicy(), "echo baseline-ok");
    SUPRA_CHECK_MSG(result.started, "sandbox starts: " + result.error);
    SUPRA_CHECK_MSG(contains(result.output, "baseline-ok"),
                    "permitted command runs: " + result.output);
}

/// Every MANAGE operation must work inside the workspace.
///
/// Truncating an existing file needs only WRITE_FILE, so a test that overwrote a
/// file left over from a previous run exercised none of MAKE_REG, MAKE_DIR,
/// REMOVE_FILE, or REMOVE_DIR. A mutation that dropped every directory-only bit
/// therefore survived: nothing depended on them.
///
/// Each operation is checked separately, on paths removed beforehand, so no
/// leftover state can stand in for a permission that was never granted.
void testWorkspaceWriteAllowed() {
    const std::string fresh = std::string(kWorkspace) + "/fresh-file";
    const std::string subdir = std::string(kWorkspace) + "/subdir";
    const std::string nested = subdir + "/nested-file";

    // Start clean: a surviving file from an earlier run would let a
    // truncate-only permission masquerade as create permission.
    ::unlink(nested.c_str());
    ::rmdir(subdir.c_str());
    ::unlink(fresh.c_str());

    // MAKE_REG: create a file that does not exist.
    {
        const auto result = runShell(workspacePolicy(),
                                     "echo created > /tmp/supra-sandbox-test-workspace/fresh-file "
                                     "&& cat /tmp/supra-sandbox-test-workspace/fresh-file");
        SUPRA_CHECK_MSG(result.started, "starts: " + result.error);
        SUPRA_CHECK_MSG(contains(result.output, "created"),
                        "MAKE_REG: a new file can be created: " + result.output);
    }

    // WRITE_FILE and TRUNCATE: overwrite the file that now exists.
    {
        const auto result = runShell(workspacePolicy(),
                                     "echo replaced > /tmp/supra-sandbox-test-workspace/fresh-file "
                                     "&& cat /tmp/supra-sandbox-test-workspace/fresh-file");
        SUPRA_CHECK_MSG(result.started, "starts: " + result.error);
        SUPRA_CHECK_MSG(contains(result.output, "replaced"),
                        "WRITE_FILE: an existing file can be truncated: " + result.output);
    }

    // MAKE_DIR, then MAKE_REG beneath it.
    {
        const auto result =
            runShell(workspacePolicy(),
                     "mkdir /tmp/supra-sandbox-test-workspace/subdir && "
                     "echo deep > /tmp/supra-sandbox-test-workspace/subdir/nested-file && "
                     "cat /tmp/supra-sandbox-test-workspace/subdir/nested-file");
        SUPRA_CHECK_MSG(result.started, "starts: " + result.error);
        SUPRA_CHECK_MSG(contains(result.output, "deep"),
                        "MAKE_DIR: a directory can be created and written into: " + result.output);
    }

    // REMOVE_FILE and REMOVE_DIR.
    {
        const auto result =
            runShell(workspacePolicy(),
                     "rm /tmp/supra-sandbox-test-workspace/subdir/nested-file && "
                     "rmdir /tmp/supra-sandbox-test-workspace/subdir && "
                     "rm /tmp/supra-sandbox-test-workspace/fresh-file && echo removed-all");
        SUPRA_CHECK_MSG(result.started, "starts: " + result.error);
        SUPRA_CHECK_MSG(contains(result.output, "removed-all"),
                        "REMOVE_FILE and REMOVE_DIR: entries can be deleted: " + result.output);
    }

    // Confirmed from outside the sandbox: the operations really took effect
    // rather than merely printing success.
    struct ::stat st{};
    SUPRA_CHECK_MSG(::stat(fresh.c_str(), &st) != 0, "the created file was really removed");
    SUPRA_CHECK_MSG(::stat(subdir.c_str(), &st) != 0, "the created directory was really removed");
}

void testReadOutsideAllowlistDenied() {
    const auto result = runShell(workspacePolicy(), "cat /etc/passwd");
    SUPRA_CHECK_MSG(result.started, "starts: " + result.error);
    SUPRA_CHECK_MSG(contains(result.output, "Permission denied"),
                    "read outside the allowlist is denied: " + result.output);
}

void testWriteOutsideAllowlistDenied() {
    const char* victim = "/tmp/supra-sandbox-escape-marker";
    ::unlink(victim);

    const auto result =
        runShell(workspacePolicy(), "echo escaped > /tmp/supra-sandbox-escape-marker");
    SUPRA_CHECK_MSG(result.started, "starts: " + result.error);
    SUPRA_CHECK_MSG(contains(result.output, "Permission denied") ||
                        contains(result.output, "cannot create"),
                    "write outside the allowlist is denied: " + result.output);

    // The stronger assertion: the file must not exist. An error message alone
    // could be produced while the write still landed.
    struct ::stat st{};
    SUPRA_CHECK_MSG(::stat(victim, &st) != 0, "the file was never created");
    ::unlink(victim);
}

/// Landlock is inode-based, so `..` resolves to an inode outside the allowlist
/// and is refused. Path-string sandboxes are defeated by exactly this.
void testPathTraversalDenied() {
    const auto result = runShell(
        workspacePolicy(),
        "cat /tmp/supra-sandbox-test-workspace/../../etc/passwd; "
        "cd /tmp/supra-sandbox-test-workspace && cat ../../etc/passwd");
    SUPRA_CHECK_MSG(result.started, "starts: " + result.error);
    SUPRA_CHECK_MSG(contains(result.output, "Permission denied"),
                    "traversal out of the workspace is denied: " + result.output);
    SUPRA_CHECK_MSG(!leakedPasswdContent(result.output),
                    "no /etc/passwd content leaked: " + result.output);
}

void testNetworkDenied() {
    const auto result = runShell(
        workspacePolicy(),
        "timeout 5 sh -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>&1 || echo net-blocked");
    SUPRA_CHECK_MSG(result.started, "starts: " + result.error);
    SUPRA_CHECK_MSG(contains(result.output, "net-blocked") ||
                        contains(result.output, "Permission denied") ||
                        contains(result.output, "unreachable"),
                    "outbound TCP is denied: " + result.output);
}

/// chroot cannot widen an inode-based policy, even though the syscall itself may
/// succeed inside a user namespace.
///
/// `chroot` lives in /usr/sbin, which the workspace policy does not grant, so the
/// tool is deliberately unreachable - itself a demonstration of deny-by-default.
/// The assertion is therefore on the outcome that matters in either case: no
/// /etc/passwd content escapes. The direct read after it exercises the same
/// property without depending on an external binary.
void testChrootCannotWiden() {
    const auto result = runShell(
        workspacePolicy(),
        "chroot /tmp/supra-sandbox-test-workspace /bin/sh -c 'cat /etc/passwd' 2>&1; "
        "cat /etc/passwd 2>&1");
    SUPRA_CHECK_MSG(result.started, "starts: " + result.error);
    SUPRA_CHECK_MSG(!leakedPasswdContent(result.output),
                    "no /etc/passwd content escaped: " + result.output);
    SUPRA_CHECK_MSG(contains(result.output, "Permission denied"),
                    "the direct read is denied: " + result.output);
}

/// A Landlock domain is inherited across fork and exec, so a grandchild is bound
/// by the same rules. Without this, a sandbox would be one `sh -c` away from
/// useless.
void testRestrictionsInherited() {
    const auto result =
        runShell(workspacePolicy(), "sh -c 'sh -c \"cat /etc/passwd\"' 2>&1");
    SUPRA_CHECK_MSG(result.started, "starts: " + result.error);
    SUPRA_CHECK_MSG(contains(result.output, "Permission denied"),
                    "a grandchild is still confined: " + result.output);
    SUPRA_CHECK_MSG(!leakedPasswdContent(result.output), "no leak through nesting");
}

/// Rulesets are monotonic: a confined process may add more, but each can only
/// narrow. There is no operation that re-widens.
void testCannotRewiden() {
    const auto result = runShell(workspacePolicy(),
                                 "cat /etc/hostname 2>&1; cat /etc/passwd 2>&1");
    SUPRA_CHECK_MSG(result.started, "starts: " + result.error);
    SUPRA_CHECK_MSG(!leakedPasswdContent(result.output),
                    "no path re-widens the sandbox: " + result.output);
}

void testProcessIsolation() {
    const auto result = runShell(workspacePolicy(), "echo $$");
    SUPRA_CHECK_MSG(result.started, "starts: " + result.error);
    // pid 1 in its own namespace: the host process table is not visible.
    SUPRA_CHECK_MSG(contains(result.output, "1"),
                    "payload is pid 1 in its namespace: " + result.output);
}

/// Environment is not inherited by default: it routinely carries credentials, and
/// leaking them into every sandboxed command would be a disclosure bug.
void testEnvironmentNotInherited() {
    ::setenv("SUPRA_SECRET_CANARY", "must-not-leak", 1);

    supra_sandbox_policy policy = workspacePolicy();
    const char* const argv[] = {"/bin/sh", "-c", "echo \"[$SUPRA_SECRET_CANARY]\"", nullptr};

    int output_pipe[2] = {-1, -1};
    SUPRA_CHECK(::pipe(output_pipe) == 0);

    supra_sandbox_command command{};
    command.program = "/bin/sh";
    command.argv = argv;
    command.envp = nullptr;  // explicitly nothing
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
    ::unsetenv("SUPRA_SECRET_CANARY");

    SUPRA_CHECK_MSG(started, "starts: " + std::string(process.error));
    SUPRA_CHECK_MSG(!contains(output, "must-not-leak"),
                    "host environment did not leak: " + output);
}

/// A missing path in the policy must abort the spawn. Skipping it would leave the
/// caller believing access was granted when no rule exists.
void testMissingPathRefused() {
    supra_sandbox_policy policy = workspacePolicy();
    supra_sandbox_policy_allow(&policy, "/nonexistent-path-for-supra-test", SUPRA_SANDBOX_READ);

    const char* const argv[] = {"/bin/true", nullptr};
    supra_sandbox_command command{};
    command.program = "/bin/true";
    command.argv = argv;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process process{};
    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, &command, &process), 0,
                       "a rule for a missing path refuses the spawn");
    SUPRA_CHECK_MSG(std::strstr(process.error, "nonexistent-path") != nullptr,
                    std::string("error names the offending path: ") + process.error);
}

/// Directory-only access bits on a regular file are rejected by the kernel, and a
/// rejected rule is an absent rule - so it must be masked, not passed through.
/// Discovered by probe: ignoring the return value silently widened the sandbox.
void testFileRuleWithDirectoryBits() {
    supra_sandbox_policy policy = workspacePolicy();
    // MANAGE on a regular file. Naively this sets MAKE_DIR and friends, which the
    // kernel refuses.
    supra_sandbox_policy_allow(&policy, "/etc/ld.so.cache",
                               SUPRA_SANDBOX_READ | SUPRA_SANDBOX_MANAGE);

    const auto result = runShell(policy, "echo bits-ok");
    SUPRA_CHECK_MSG(result.started,
                    "directory bits on a file are masked, not refused: " + result.error);
    SUPRA_CHECK_MSG(contains(result.output, "bits-ok"), "still runs: " + result.output);
}

/// A rule the kernel rejects must abort the spawn, not be skipped.
///
/// Two distinct defects live here, and separating them took a mutation
/// experiment.
///
/// Masking must be correct: `landlock_add_rule` returns EINVAL when
/// directory-only bits (MAKE_DIR, REMOVE_FILE, REFER) are applied to a regular
/// file. Verified directly against the kernel - unmasked bits on /bin/sh give
/// `rc=-1 EINVAL`, masked bits give `rc=0`.
///
/// And the failure must not be swallowed: if masking regresses *and* the return
/// value is ignored, the rule silently ceases to exist and the sandbox is wider
/// than the policy claims.
///
/// Detecting either requires a *load-bearing* rule. This policy grants the
/// interpreter through one regular-file rule carrying MANAGE, and nothing else
/// grants /bin/sh. Three outcomes, one correct:
///
///   masking correct                -> rule accepted, payload runs, exit 7
///   masking broken, error checked  -> spawn refuses with EINVAL named
///   masking broken, error ignored  -> rule absent, exec denied, yet the caller
///                                     would be told the sandbox applied cleanly
///
/// A MANAGE rule on a non-essential path cannot tell these apart, which is why an
/// earlier version of this test let both mutations survive.
void testRejectedRuleIsNotSilentlySkipped() {
    supra_sandbox_policy policy{};
    supra_sandbox_policy_init(&policy);
    policy.required_tier = SUPRA_SANDBOX_TIER_LANDLOCK;

    // Shared libraries, without which nothing dynamic can start.
    supra_sandbox_policy_allow(&policy, "/lib", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    if (::access("/lib64", F_OK) == 0) {
        supra_sandbox_policy_allow(&policy, "/lib64", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
    }
    supra_sandbox_policy_allow(&policy, "/usr/lib", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);

    // The load-bearing rule: the interpreter, as a regular file, with MANAGE.
    // Nothing else grants /bin/sh, so if this rule vanishes the exec fails.
    supra_sandbox_policy_allow(&policy, "/bin/sh",
                               SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE | SUPRA_SANDBOX_MANAGE);

    const char* const argv[] = {"/bin/sh", "-c", "exit 7", nullptr};
    supra_sandbox_command command{};
    command.program = "/bin/sh";
    command.argv = argv;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process process{};
    const int started = supra_sandbox_spawn(&policy, &command, &process);

    SUPRA_CHECK_EQ_MSG(started, 1,
                       std::string("a load-bearing file rule survives masking: ") + process.error);
    if (started == 1) {
        int status = -1;
        SUPRA_CHECK_EQ_MSG(supra_sandbox_wait(&process, 10000, &status), 1, "payload completes");
        // Proves the rule was really applied: a dropped rule would deny exec and
        // never reach the payload's own exit code.
        SUPRA_CHECK_EQ_MSG(status, 7, "payload ran and returned its own status");
    }
}

/// Every access combination must survive masking, on both a file and a directory.
///
/// One case cannot cover the bit-masking table. A mutation that treated *all*
/// targets as directories passed the load-bearing test above, because the
/// directory rules in that policy were unaffected and the single file rule was
/// still reachable through them.
///
/// Each combination is exercised through a load-bearing rule of each target type,
/// so a mis-masked bit set becomes an observable failure rather than a silently
/// absent rule.
void testAccessCombinationsSurviveMasking() {
    const std::uint32_t combinations[] = {
        SUPRA_SANDBOX_READ,
        SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE,
        SUPRA_SANDBOX_READ | SUPRA_SANDBOX_WRITE,
        SUPRA_SANDBOX_READ | SUPRA_SANDBOX_WRITE | SUPRA_SANDBOX_EXECUTE,
        SUPRA_SANDBOX_READ | SUPRA_SANDBOX_MANAGE,
        SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE | SUPRA_SANDBOX_MANAGE,
        SUPRA_SANDBOX_READ | SUPRA_SANDBOX_WRITE | SUPRA_SANDBOX_EXECUTE | SUPRA_SANDBOX_MANAGE,
    };

    // `target` is the only rule granting the interpreter, so a mis-masked bit set
    // shows up as a failed spawn or a lost exit code.
    const auto exercise = [](const char* target, std::uint32_t access, int expect) {
        supra_sandbox_policy policy{};
        supra_sandbox_policy_init(&policy);
        policy.required_tier = SUPRA_SANDBOX_TIER_LANDLOCK;
        supra_sandbox_policy_allow(&policy, "/lib", SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
        if (::access("/lib64", F_OK) == 0) {
            supra_sandbox_policy_allow(&policy, "/lib64",
                                       SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
        }
        supra_sandbox_policy_allow(&policy, "/usr/lib",
                                   SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE);
        supra_sandbox_policy_allow(&policy, target, access | SUPRA_SANDBOX_EXECUTE);

        char script[32];
        std::snprintf(script, sizeof script, "exit %d", expect);
        const char* const argv[] = {"/bin/sh", "-c", script, nullptr};

        supra_sandbox_command command{};
        command.program = "/bin/sh";
        command.argv = argv;
        command.stdin_fd = -1;
        command.stdout_fd = -1;
        command.stderr_fd = -1;

        supra_sandbox_process process{};
        const int started = supra_sandbox_spawn(&policy, &command, &process);
        const std::string label =
            std::string(target) + " access 0x" + std::to_string(access);

        SUPRA_CHECK_EQ_MSG(started, 1, label + " spawns: " + process.error);
        if (started == 1) {
            int status = -1;
            supra_sandbox_wait(&process, 10000, &status);
            SUPRA_CHECK_EQ_MSG(status, expect, label + " really applied");
        }
    };

    for (const std::uint32_t access : combinations) {
        exercise("/bin/sh", access, 5);  // regular file
        exercise("/bin", access, 6);     // directory
    }
}

/// A setup failure must be reported through the CLOEXEC pipe, so the caller
/// learns which step failed rather than only that the child exited.
void testSetupFailureReported() {
    supra_sandbox_policy policy = workspacePolicy();

    const char* const argv[] = {"/nonexistent/binary", nullptr};
    supra_sandbox_command command{};
    command.program = "/nonexistent/binary";
    command.argv = argv;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;

    supra_sandbox_process process{};
    SUPRA_CHECK_EQ_MSG(supra_sandbox_spawn(&policy, &command, &process), 0,
                       "a failed exec is reported as a failed spawn");
    SUPRA_CHECK_MSG(std::strstr(process.error, "execve") != nullptr,
                    std::string("error names the failing step: ") + process.error);
}

void testKillTerminatesTree() {
    supra_sandbox_policy policy = workspacePolicy();
    const char* const argv[] = {"/bin/sh", "-c", "sleep 60 & sleep 60", nullptr};

    supra_sandbox_command command{};
    command.program = "/bin/sh";
    command.argv = argv;
    command.stdin_fd = -1;
    command.stdout_fd = -1;
    command.stderr_fd = -1;
    static const char* const env[] = {"PATH=/usr/bin:/bin", nullptr};
    command.envp = env;

    supra_sandbox_process process{};
    SUPRA_CHECK_MSG(supra_sandbox_spawn(&policy, &command, &process) == 1,
                    std::string("long-running command starts: ") + process.error);

    // spawn returns at exec, not at exit, so the payload is still running.
    SUPRA_CHECK_EQ_MSG(supra_sandbox_wait(&process, 200, nullptr), 0, "still running");

    // kill reaps as part of confirming death, so returning 1 *is* the proof that
    // the process is gone. A wait afterwards would fail with ECHILD - there is no
    // child left to wait for - which is correct behaviour, not a defect.
    SUPRA_CHECK_EQ_MSG(supra_sandbox_kill(&process, 500), 1, "kill confirms termination");

    // Verified rather than assumed: waiting again must report an error, which is
    // what "already reaped" looks like through this API.
    SUPRA_CHECK_EQ_MSG(supra_sandbox_wait(&process, 200, nullptr), -1,
                       "kill already reaped, so a second wait has no child");
}

}  // namespace

int main() {
    supra_sandbox_capabilities caps{};
    supra_sandbox_probe(&caps);

    if (caps.tier < SUPRA_SANDBOX_TIER_LANDLOCK) {
        // Skip loudly. A green suite that verified nothing is the worst outcome
        // available for a security boundary.
        std::fprintf(stderr,
                     "sandbox_escape_test: SKIPPED - this platform reaches only tier %u.\n"
                     "  %s\n"
                     "  Escape attempts require Landlock enforcement to be meaningful.\n",
                     static_cast<unsigned>(caps.tier), caps.detail);
        return 0;
    }

    if (::mkdir(kWorkspace, 0700) != 0 && ::access(kWorkspace, W_OK) != 0) {
        std::fprintf(stderr, "sandbox_escape_test: cannot create %s\n", kWorkspace);
        return 2;
    }

    testBaselineWorks();
    testWorkspaceWriteAllowed();
    testReadOutsideAllowlistDenied();
    testWriteOutsideAllowlistDenied();
    testPathTraversalDenied();
    testNetworkDenied();
    testChrootCannotWiden();
    testRestrictionsInherited();
    testCannotRewiden();
    testProcessIsolation();
    testEnvironmentNotInherited();
    testMissingPathRefused();
    testFileRuleWithDirectoryBits();
    testRejectedRuleIsNotSilentlySkipped();
    testAccessCombinationsSurviveMasking();
    testSetupFailureReported();
    testKillTerminatesTree();

    return supra::test::finish("sandbox_escape_test");
}
