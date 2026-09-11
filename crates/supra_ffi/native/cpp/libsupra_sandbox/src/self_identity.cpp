// Self-identification: which file is the running executable.
//
// Backs guard layer L4 in T12.5, refusing to spawn this binary from inside
// itself. Name comparison is insufficient - a copy, symlink, rename, or `sh -c`
// indirection defeats it - so identity is (device, inode), which is stable
// across every renaming trick.
//
// The limit is worth stating: a *duplicated* binary has a different inode and
// will not match. T12.5 layers an environment marker to catch that case, and the
// process-tree budget catches the consequence even when both are evaded.

#include <cstddef>
#include <cstdint>
#include <cstring>

#include <sys/stat.h>

#if defined(__linux__)
#include <unistd.h>
#elif defined(__APPLE__)
#include <mach-o/dyld.h>
#elif defined(_WIN32)
#include <windows.h>
#endif

#include "supra/sandbox.h"

extern "C" {

size_t supra_sandbox_self_path(char* out, size_t cap) {
    if (out == nullptr || cap == 0) {
        return 0;
    }
    out[0] = '\0';

#if defined(__linux__)
    // /proc/self/exe is a symlink to the real file, so it survives a rename of
    // the original path.
    const ssize_t len = ::readlink("/proc/self/exe", out, cap - 1);
    if (len <= 0) {
        return 0;
    }
    // readlink truncates silently when the buffer is too small, and a truncated
    // path can resolve to a *different* file - which would make guard layer L4
    // compare against the wrong inode. Filling the buffer exactly is
    // indistinguishable from truncation, so treat it as failure.
    if (static_cast<std::size_t>(len) >= cap - 1) {
        out[0] = '\0';
        return 0;
    }
    out[len] = '\0';
    return static_cast<size_t>(len);

#elif defined(__APPLE__)
    std::uint32_t size = static_cast<std::uint32_t>(cap);
    if (_NSGetExecutablePath(out, &size) != 0) {
        return 0;
    }
    return std::strlen(out);

#elif defined(_WIN32)
    const DWORD len = GetModuleFileNameA(nullptr, out, static_cast<DWORD>(cap));
    if (len == 0 || len >= cap) {
        return 0;
    }
    return static_cast<size_t>(len);

#else
    return 0;
#endif
}

int supra_sandbox_self_identity(uint64_t* out_dev, uint64_t* out_ino) {
    if (out_dev == nullptr || out_ino == nullptr) {
        return 0;
    }

#if defined(__linux__)
    // stat the symlink target directly: no path buffer, so no truncation risk.
    struct ::stat st{};
    if (::stat("/proc/self/exe", &st) != 0) {
        return 0;
    }
    *out_dev = static_cast<std::uint64_t>(st.st_dev);
    *out_ino = static_cast<std::uint64_t>(st.st_ino);
    return 1;

#else
    char path[4096];
    if (supra_sandbox_self_path(path, sizeof path) == 0) {
        return 0;
    }
    return supra_sandbox_file_identity(path, out_dev, out_ino);
#endif
}

int supra_sandbox_file_identity(const char* path, uint64_t* out_dev, uint64_t* out_ino) {
    if (path == nullptr || out_dev == nullptr || out_ino == nullptr) {
        return 0;
    }

    // stat, not lstat: a symlink must resolve to its target, or pointing a link
    // at this binary would evade the check.
    struct ::stat st{};
    if (::stat(path, &st) != 0) {
        return 0;
    }
    *out_dev = static_cast<std::uint64_t>(st.st_dev);
    *out_ino = static_cast<std::uint64_t>(st.st_ino);
    return 1;
}

}  // extern "C"
