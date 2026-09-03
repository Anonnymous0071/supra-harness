// Self-identification, backing guard layer L4 in T12.5.
//
// The threat is a coding agent spawning another copy of the harness, which
// recurses until the machine dies. Name comparison does not stop it: a copy,
// symlink, rename, or `sh -c` indirection all defeat a string check. Identity is
// (device, inode), which survives every renaming trick.
//
// The residual gap is stated rather than hidden: a *duplicated* binary has a
// different inode and will not match. T12.5 layers an environment marker for
// that, and the process-tree budget bounds the damage even if both are evaded.

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>

#include <unistd.h>

#include "supra/sandbox.h"
#include "supra/testing.hpp"

namespace {

void testSelfPath() {
    char path[4096];
    const std::size_t len = supra_sandbox_self_path(path, sizeof path);

    SUPRA_CHECK_MSG(len > 0, "self path is resolvable");
    SUPRA_CHECK_EQ_MSG(std::strlen(path), len, "returned length matches the string");
    SUPRA_CHECK_MSG(path[0] == '/', std::string("self path is absolute: ") + path);
    SUPRA_CHECK_MSG(std::strstr(path, "self_identity_test") != nullptr,
                    std::string("self path names this test binary: ") + path);

    // A short buffer must fail rather than truncate: a truncated path could
    // resolve to a *different* file, which would silently break the guard.
    char tiny[4];
    SUPRA_CHECK_EQ_MSG(supra_sandbox_self_path(tiny, sizeof tiny), std::size_t{0},
                       "short buffer fails rather than truncating");

    SUPRA_CHECK_EQ(supra_sandbox_self_path(nullptr, 100), std::size_t{0});
    SUPRA_CHECK_EQ(supra_sandbox_self_path(path, 0), std::size_t{0});
}

void testSelfIdentity() {
    std::uint64_t dev = 0;
    std::uint64_t ino = 0;
    SUPRA_CHECK_EQ_MSG(supra_sandbox_self_identity(&dev, &ino), 1, "self identity resolves");
    SUPRA_CHECK_MSG(ino != 0, "inode is non-zero");

    // Stable across calls: an identity that changed would make the guard
    // unreliable.
    std::uint64_t dev2 = 0;
    std::uint64_t ino2 = 0;
    supra_sandbox_self_identity(&dev2, &ino2);
    SUPRA_CHECK_EQ_MSG(dev2, dev, "device is stable");
    SUPRA_CHECK_EQ_MSG(ino2, ino, "inode is stable");

    SUPRA_CHECK_EQ(supra_sandbox_self_identity(nullptr, &ino), 0);
    SUPRA_CHECK_EQ(supra_sandbox_self_identity(&dev, nullptr), 0);
}

/// Identity by path must agree with identity by self, or the guard cannot compare
/// a candidate command against this binary.
void testPathIdentityMatchesSelf() {
    char path[4096];
    SUPRA_CHECK(supra_sandbox_self_path(path, sizeof path) > 0);

    std::uint64_t self_dev = 0;
    std::uint64_t self_ino = 0;
    SUPRA_CHECK(supra_sandbox_self_identity(&self_dev, &self_ino) == 1);

    std::uint64_t path_dev = 0;
    std::uint64_t path_ino = 0;
    SUPRA_CHECK_EQ_MSG(supra_sandbox_file_identity(path, &path_dev, &path_ino), 1,
                       "identity by path resolves");

    SUPRA_CHECK_EQ_MSG(path_dev, self_dev, "device matches");
    SUPRA_CHECK_EQ_MSG(path_ino, self_ino, "inode matches");
}

/// The case that motivates inode identity: a symlink has a different path but the
/// same inode, so a name check would let it through.
void testSymlinkResolvesToSameIdentity() {
    char path[4096];
    SUPRA_CHECK(supra_sandbox_self_path(path, sizeof path) > 0);

    const char* link = "/tmp/supra-self-identity-symlink";
    ::unlink(link);
    if (::symlink(path, link) != 0) {
        std::fprintf(stderr, "  (skipped symlink case: cannot create %s)\n", link);
        return;
    }

    std::uint64_t self_dev = 0;
    std::uint64_t self_ino = 0;
    supra_sandbox_self_identity(&self_dev, &self_ino);

    std::uint64_t link_dev = 0;
    std::uint64_t link_ino = 0;
    SUPRA_CHECK_EQ_MSG(supra_sandbox_file_identity(link, &link_dev, &link_ino), 1,
                       "symlink resolves");

    // stat follows the link, so the identity is the target's. A name comparison
    // would see two different strings and permit the spawn.
    SUPRA_CHECK_EQ_MSG(link_ino, self_ino, "symlink has the same inode as its target");
    SUPRA_CHECK_EQ_MSG(link_dev, self_dev, "symlink has the same device as its target");

    ::unlink(link);
}

/// An unrelated file must differ, or the guard would refuse every command.
void testUnrelatedFileDiffers() {
    std::uint64_t self_dev = 0;
    std::uint64_t self_ino = 0;
    supra_sandbox_self_identity(&self_dev, &self_ino);

    std::uint64_t other_dev = 0;
    std::uint64_t other_ino = 0;
    SUPRA_CHECK_EQ_MSG(supra_sandbox_file_identity("/bin/sh", &other_dev, &other_ino), 1,
                       "/bin/sh resolves");
    SUPRA_CHECK_MSG(other_ino != self_ino, "an unrelated binary has a different inode");
}

void testMissingFile() {
    std::uint64_t dev = 0;
    std::uint64_t ino = 0;
    SUPRA_CHECK_EQ_MSG(supra_sandbox_file_identity("/nonexistent-supra-guard-probe", &dev, &ino), 0,
                       "a missing file has no identity");
    SUPRA_CHECK_EQ(supra_sandbox_file_identity(nullptr, &dev, &ino), 0);
    SUPRA_CHECK_EQ(supra_sandbox_file_identity("/bin/sh", nullptr, &ino), 0);
    SUPRA_CHECK_EQ(supra_sandbox_file_identity("/bin/sh", &dev, nullptr), 0);
}

/// A copied binary has a different inode, so identity alone does not stop it.
/// Asserted rather than merely documented: the limit is real, and T12.5's
/// environment marker exists because of it.
void testCopyHasDifferentIdentity() {
    char path[4096];
    SUPRA_CHECK(supra_sandbox_self_path(path, sizeof path) > 0);

    const char* copy = "/tmp/supra-self-identity-copy";
    ::unlink(copy);

    char command[8320];
    std::snprintf(command, sizeof command, "cp '%s' '%s' 2>/dev/null", path, copy);
    if (std::system(command) != 0) {
        std::fprintf(stderr, "  (skipped copy case: cp failed)\n");
        return;
    }

    std::uint64_t self_dev = 0;
    std::uint64_t self_ino = 0;
    supra_sandbox_self_identity(&self_dev, &self_ino);

    std::uint64_t copy_dev = 0;
    std::uint64_t copy_ino = 0;
    SUPRA_CHECK(supra_sandbox_file_identity(copy, &copy_dev, &copy_ino) == 1);

    SUPRA_CHECK_MSG(copy_ino != self_ino,
                    "a copy has a different inode - this is why T12.5 also uses an env marker");

    ::unlink(copy);
}

}  // namespace

int main() {
    testSelfPath();
    testSelfIdentity();
    testPathIdentityMatchesSelf();
    testSymlinkResolvesToSameIdentity();
    testUnrelatedFileDiffers();
    testMissingFile();
    testCopyHasDifferentIdentity();
    return supra::test::finish("self_identity_test");
}
