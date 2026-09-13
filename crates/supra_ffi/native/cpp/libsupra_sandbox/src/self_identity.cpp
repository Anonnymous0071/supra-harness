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

#if defined(_WIN32)
#include <windows.h>
#else
#include <sys/stat.h>
#endif

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
    wchar_t wide[32768];
    const DWORD wide_len = ::GetModuleFileNameW(nullptr, wide, 32768);
    if (wide_len == 0 || wide_len >= 32768) {
        return 0;
    }
    const int needed = ::WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, wide,
                                             static_cast<int>(wide_len), nullptr, 0,
                                             nullptr, nullptr);
    if (needed <= 0 || static_cast<std::size_t>(needed) >= cap) {
        return 0;
    }
    const int written = ::WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, wide,
                                              static_cast<int>(wide_len), out, needed,
                                              nullptr, nullptr);
    if (written != needed) {
        out[0] = '\0';
        return 0;
    }
    out[written] = '\0';
    return static_cast<std::size_t>(written);

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

#if defined(_WIN32)
    const int wide_needed =
        ::MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, nullptr, 0);
    if (wide_needed <= 0) {
        return 0;
    }
    auto* wide = static_cast<wchar_t*>(
        ::HeapAlloc(::GetProcessHeap(), 0,
                    static_cast<SIZE_T>(wide_needed) * sizeof(wchar_t)));
    if (wide == nullptr ||
        ::MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1,
                              wide, wide_needed) <= 0) {
        if (wide != nullptr) {
            ::HeapFree(::GetProcessHeap(), 0, wide);
        }
        return 0;
    }
    HANDLE file = ::CreateFileW(wide, FILE_READ_ATTRIBUTES,
                                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                                nullptr, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, nullptr);
    ::HeapFree(::GetProcessHeap(), 0, wide);
    if (file == INVALID_HANDLE_VALUE) {
        return 0;
    }
    BY_HANDLE_FILE_INFORMATION info{};
    const bool ok = ::GetFileInformationByHandle(file, &info) != FALSE;
    ::CloseHandle(file);
    if (!ok) {
        return 0;
    }
    *out_dev = info.dwVolumeSerialNumber;
    *out_ino = (static_cast<std::uint64_t>(info.nFileIndexHigh) << 32U) |
               info.nFileIndexLow;
    return 1;
#else
    // stat, not lstat: a symlink must resolve to its target, or pointing a link
    // at this binary would evade the check.
    struct ::stat st{};
    if (::stat(path, &st) != 0) {
        return 0;
    }
    *out_dev = static_cast<std::uint64_t>(st.st_dev);
    *out_ino = static_cast<std::uint64_t>(st.st_ino);
    return 1;
#endif
}

}  // extern "C"
