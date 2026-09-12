// Windows backend: AppContainer filesystem/network isolation, explicit handle
// inheritance, and a kill-on-close job object for process-tree ownership.

#ifdef _WIN32

#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <aclapi.h>
#include <appmodel.h>
#include <io.h>

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <cwchar>
#include <limits>
#include <mutex>

#include "internal.hpp"
#include "supra/sandbox.h"

namespace {

using supra::sandbox::detail::appendTruncated;
using supra::sandbox::detail::setError;

struct AclBackup {
    wchar_t* path;
    PSECURITY_DESCRIPTOR descriptor;
    PACL dacl;
    bool protected_dacl;
};

struct ProcessState {
    HANDLE job;
    wchar_t profile_name[96];
    AclBackup backups[SUPRA_SANDBOX_MAX_PATHS];
    std::size_t backup_count;
};

struct WideBuffer {
    wchar_t* data;
    std::size_t length;
    std::size_t capacity;
};

supra_sandbox_capabilities g_caps{};
bool g_caps_ready = false;
std::uint8_t g_forced_tier = SUPRA_SANDBOX_TIER_UNSET;
volatile LONG g_profile_counter = 0;
std::mutex g_probe_mutex;
std::mutex g_spawn_mutex;

void setWindowsError(char* dest, std::size_t cap, const char* step, DWORD error) {
    setError(dest, cap, step);
    appendTruncated(dest, cap, ": ");
    char message[160]{};
    const DWORD flags = FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS;
    const DWORD size = ::FormatMessageA(flags, nullptr, error, 0, message,
                                        static_cast<DWORD>(sizeof message), nullptr);
    if (size == 0) {
        char number[32]{};
        std::snprintf(number, sizeof number, "Windows error %lu",
                      static_cast<unsigned long>(error));
        appendTruncated(dest, cap, number);
        return;
    }
    while (message[0] != '\0') {
        const std::size_t length = std::strlen(message);
        if (length == 0 || (message[length - 1] != '\r' && message[length - 1] != '\n')) {
            break;
        }
        message[length - 1] = '\0';
    }
    appendTruncated(dest, cap, message);
}

bool utf8ToWide(const char* source, wchar_t** out) {
    *out = nullptr;
    if (source == nullptr) {
        return true;
    }
    const int needed = ::MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, source, -1, nullptr, 0);
    if (needed <= 0) {
        return false;
    }
    auto* value = static_cast<wchar_t*>(
        ::HeapAlloc(::GetProcessHeap(), HEAP_ZERO_MEMORY,
                    static_cast<SIZE_T>(needed) * sizeof(wchar_t)));
    if (value == nullptr ||
        ::MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, source, -1, value, needed) <= 0) {
        if (value != nullptr) {
            ::HeapFree(::GetProcessHeap(), 0, value);
        }
        return false;
    }
    *out = value;
    return true;
}

void freeWide(wchar_t* value) {
    if (value != nullptr) {
        ::HeapFree(::GetProcessHeap(), 0, value);
    }
}

bool reserve(WideBuffer* buffer, std::size_t extra) {
    if (extra > std::numeric_limits<std::size_t>::max() - buffer->length - 1U) {
        return false;
    }
    const std::size_t required = buffer->length + extra + 1U;
    if (required <= buffer->capacity) {
        return true;
    }
    std::size_t capacity = buffer->capacity == 0 ? 128U : buffer->capacity;
    while (capacity < required) {
        if (capacity > std::numeric_limits<std::size_t>::max() / 2U) {
            capacity = required;
            break;
        }
        capacity *= 2U;
    }
    void* memory = buffer->data == nullptr
                       ? ::HeapAlloc(::GetProcessHeap(), HEAP_ZERO_MEMORY,
                                     static_cast<SIZE_T>(capacity) * sizeof(wchar_t))
                       : ::HeapReAlloc(::GetProcessHeap(), HEAP_ZERO_MEMORY, buffer->data,
                                      static_cast<SIZE_T>(capacity) * sizeof(wchar_t));
    if (memory == nullptr) {
        return false;
    }
    buffer->data = static_cast<wchar_t*>(memory);
    buffer->capacity = capacity;
    return true;
}

bool push(WideBuffer* buffer, wchar_t value) {
    if (!reserve(buffer, 1U)) {
        return false;
    }
    buffer->data[buffer->length++] = value;
    buffer->data[buffer->length] = L'\0';
    return true;
}

bool append(WideBuffer* buffer, const wchar_t* value) {
    const std::size_t count = std::wcslen(value);
    if (!reserve(buffer, count)) {
        return false;
    }
    std::memcpy(buffer->data + buffer->length, value, count * sizeof(wchar_t));
    buffer->length += count;
    buffer->data[buffer->length] = L'\0';
    return true;
}

bool appendQuotedArgument(WideBuffer* buffer, const wchar_t* argument) {
    const bool quote = argument[0] == L'\0' || std::wcspbrk(argument, L" \t\n\v\"") != nullptr;
    if (!quote) {
        return append(buffer, argument);
    }
    if (!push(buffer, L'\"')) {
        return false;
    }
    std::size_t slashes = 0;
    for (const wchar_t* cursor = argument;; ++cursor) {
        if (*cursor == L'\\') {
            ++slashes;
            continue;
        }
        if (*cursor == L'\"') {
            for (std::size_t i = 0; i < slashes * 2U + 1U; ++i) {
                if (!push(buffer, L'\\')) {
                    return false;
                }
            }
            if (!push(buffer, L'\"')) {
                return false;
            }
            slashes = 0;
            continue;
        }
        if (*cursor == L'\0') {
            for (std::size_t i = 0; i < slashes * 2U; ++i) {
                if (!push(buffer, L'\\')) {
                    return false;
                }
            }
            return push(buffer, L'\"');
        }
        for (std::size_t i = 0; i < slashes; ++i) {
            if (!push(buffer, L'\\')) {
                return false;
            }
        }
        slashes = 0;
        if (!push(buffer, *cursor)) {
            return false;
        }
    }
}

void freeBuffer(WideBuffer* buffer) {
    if (buffer->data != nullptr) {
        ::HeapFree(::GetProcessHeap(), 0, buffer->data);
    }
    *buffer = WideBuffer{};
}

bool buildCommandLine(const char* const* argv, WideBuffer* command_line) {
    for (std::size_t i = 0; argv[i] != nullptr; ++i) {
        wchar_t* argument = nullptr;
        if (!utf8ToWide(argv[i], &argument)) {
            return false;
        }
        const bool separated = i == 0 || push(command_line, L' ');
        const bool ok = separated && appendQuotedArgument(command_line, argument);
        freeWide(argument);
        if (!ok) {
            return false;
        }
    }
    return command_line->length != 0;
}

bool buildEnvironment(const char* const* envp, WideBuffer* environment) {
    if (envp != nullptr) {
        for (std::size_t i = 0; envp[i] != nullptr; ++i) {
            if (envp[i][0] == '=' || std::strchr(envp[i], '=') == nullptr) {
                return false;
            }
            wchar_t* entry = nullptr;
            if (!utf8ToWide(envp[i], &entry)) {
                return false;
            }
            const bool ok = append(environment, entry) && push(environment, L'\0');
            freeWide(entry);
            if (!ok) {
                return false;
            }
        }
    }
    if (environment->length == 0 && !push(environment, L'\0')) {
        return false;
    }
    return push(environment, L'\0');
}

bool isCanonicalAbsolutePath(const wchar_t* path) {
    if (path == nullptr || path[0] == L'\0' || std::wcsncmp(path, L"\\\\?\\", 4) == 0 ||
        std::wcsncmp(path, L"\\\\.\\", 4) == 0) {
        return false;
    }
    const DWORD needed = ::GetFullPathNameW(path, 0, nullptr, nullptr);
    if (needed == 0) {
        return false;
    }
    auto* full = static_cast<wchar_t*>(
        ::HeapAlloc(::GetProcessHeap(), HEAP_ZERO_MEMORY,
                    static_cast<SIZE_T>(needed) * sizeof(wchar_t)));
    if (full == nullptr) {
        return false;
    }
    const DWORD written = ::GetFullPathNameW(path, needed, full, nullptr);
    const bool absolute = written > 0 && written < needed &&
                          ((path[0] != L'\0' && path[1] == L':') ||
                           (path[0] == L'\\' && path[1] == L'\\'));
    const bool canonical = absolute &&
                           ::CompareStringOrdinal(path, -1, full, -1, TRUE) == CSTR_EQUAL;
    ::HeapFree(::GetProcessHeap(), 0, full);
    return canonical;
}

DWORD accessMask(std::uint32_t access) {
    DWORD mask = READ_CONTROL | SYNCHRONIZE;
    if ((access & SUPRA_SANDBOX_READ) != 0U) {
        mask |= FILE_GENERIC_READ;
    }
    if ((access & SUPRA_SANDBOX_WRITE) != 0U) {
        mask |= FILE_GENERIC_WRITE;
    }
    if ((access & SUPRA_SANDBOX_EXECUTE) != 0U) {
        mask |= FILE_GENERIC_EXECUTE;
    }
    if ((access & SUPRA_SANDBOX_MANAGE) != 0U) {
        mask |= DELETE | FILE_DELETE_CHILD | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY;
    }
    return mask;
}

void restoreAcls(ProcessState* state) {
    while (state != nullptr && state->backup_count > 0) {
        AclBackup& backup = state->backups[--state->backup_count];
        const SECURITY_INFORMATION protection =
            backup.protected_dacl ? PROTECTED_DACL_SECURITY_INFORMATION
                                  : UNPROTECTED_DACL_SECURITY_INFORMATION;
        static_cast<void>(::SetNamedSecurityInfoW(
            backup.path, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION | protection,
            nullptr, nullptr, backup.dacl, nullptr));
        if (backup.descriptor != nullptr) {
            ::LocalFree(backup.descriptor);
        }
        freeWide(backup.path);
        backup = AclBackup{};
    }
}

void destroyState(ProcessState* state, bool terminate_tree) {
    if (state == nullptr) {
        return;
    }
    if (state->job != nullptr) {
        if (!terminate_tree) {
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
            static_cast<void>(::SetInformationJobObject(
                state->job, JobObjectExtendedLimitInformation, &limits, sizeof limits));
        }
        ::CloseHandle(state->job);
        state->job = nullptr;
    }
    restoreAcls(state);
    if (state->profile_name[0] != L'\0') {
        static_cast<void>(::DeleteAppContainerProfile(state->profile_name));
    }
    ::HeapFree(::GetProcessHeap(), 0, state);
}

bool grantPath(ProcessState* state, const wchar_t* path, PSID appcontainer_sid,
               std::uint32_t access, char* error, std::size_t error_cap) {
    if (!isCanonicalAbsolutePath(path)) {
        setError(error, error_cap, "path is not canonical and absolute");
        return false;
    }
    const DWORD attributes = ::GetFileAttributesW(path);
    if (attributes == INVALID_FILE_ATTRIBUTES) {
        setWindowsError(error, error_cap, "GetFileAttributesW for policy path", ::GetLastError());
        return false;
    }
    PSECURITY_DESCRIPTOR descriptor = nullptr;
    PACL old_dacl = nullptr;
    const DWORD security_error = ::GetNamedSecurityInfoW(
        const_cast<wchar_t*>(path), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
        nullptr, nullptr, &old_dacl, nullptr, &descriptor);
    if (security_error != ERROR_SUCCESS) {
        setWindowsError(error, error_cap, "GetNamedSecurityInfoW", security_error);
        return false;
    }
    SECURITY_DESCRIPTOR_CONTROL control = 0;
    DWORD revision = 0;
    if (::GetSecurityDescriptorControl(descriptor, &control, &revision) == FALSE) {
        setWindowsError(error, error_cap, "GetSecurityDescriptorControl", ::GetLastError());
        ::LocalFree(descriptor);
        return false;
    }
    const std::size_t path_length = std::wcslen(path) + 1U;
    auto* path_copy = static_cast<wchar_t*>(
        ::HeapAlloc(::GetProcessHeap(), 0, path_length * sizeof(wchar_t)));
    if (path_copy == nullptr) {
        setError(error, error_cap, "out of memory saving policy ACL");
        ::LocalFree(descriptor);
        return false;
    }
    std::memcpy(path_copy, path, path_length * sizeof(wchar_t));

    EXPLICIT_ACCESSW explicit_access{};
    explicit_access.grfAccessPermissions = accessMask(access);
    explicit_access.grfAccessMode = GRANT_ACCESS;
    explicit_access.grfInheritance =
        (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0U
            ? SUB_CONTAINERS_AND_OBJECTS_INHERIT
            : NO_INHERITANCE;
    explicit_access.Trustee.TrusteeForm = TRUSTEE_IS_SID;
    explicit_access.Trustee.TrusteeType = TRUSTEE_IS_WELL_KNOWN_GROUP;
    explicit_access.Trustee.ptstrName = static_cast<LPWSTR>(appcontainer_sid);

    PACL new_dacl = nullptr;
    const DWORD acl_error = ::SetEntriesInAclW(1, &explicit_access, old_dacl, &new_dacl);
    if (acl_error != ERROR_SUCCESS) {
        setWindowsError(error, error_cap, "SetEntriesInAclW", acl_error);
        freeWide(path_copy);
        ::LocalFree(descriptor);
        return false;
    }
    const DWORD set_error = ::SetNamedSecurityInfoW(
        path_copy, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
        nullptr, nullptr, new_dacl, nullptr);
    ::LocalFree(new_dacl);
    if (set_error != ERROR_SUCCESS) {
        setWindowsError(error, error_cap, "SetNamedSecurityInfoW", set_error);
        freeWide(path_copy);
        ::LocalFree(descriptor);
        return false;
    }
    AclBackup& backup = state->backups[state->backup_count++];
    backup.path = path_copy;
    backup.descriptor = descriptor;
    backup.dacl = old_dacl;
    backup.protected_dacl = (control & SE_DACL_PROTECTED) != 0U;
    return true;
}

bool duplicateStandardHandle(int descriptor, bool input, HANDLE* out,
                             char* error, std::size_t error_cap) {
    HANDLE source = INVALID_HANDLE_VALUE;
    bool close_source = false;
    if (descriptor >= 0) {
        const intptr_t raw = ::_get_osfhandle(descriptor);
        if (raw == -1) {
            setError(error, error_cap, "stdio descriptor is not a valid CRT descriptor");
            return false;
        }
        source = reinterpret_cast<HANDLE>(raw);
    } else {
        SECURITY_ATTRIBUTES security{sizeof security, nullptr, TRUE};
        source = ::CreateFileW(L"NUL", input ? GENERIC_READ : GENERIC_WRITE,
                               FILE_SHARE_READ | FILE_SHARE_WRITE, &security, OPEN_EXISTING,
                               FILE_ATTRIBUTE_NORMAL, nullptr);
        close_source = true;
    }
    if (source == INVALID_HANDLE_VALUE || source == nullptr ||
        ::DuplicateHandle(::GetCurrentProcess(), source, ::GetCurrentProcess(), out,
                          0, TRUE, DUPLICATE_SAME_ACCESS) == FALSE) {
        const DWORD win_error = ::GetLastError();
        if (close_source && source != INVALID_HANDLE_VALUE && source != nullptr) {
            ::CloseHandle(source);
        }
        setWindowsError(error, error_cap, "DuplicateHandle for stdio", win_error);
        return false;
    }
    if (close_source) {
        ::CloseHandle(source);
    }
    return true;
}

void closeHandles(HANDLE* handles, std::size_t count) {
    for (std::size_t i = 0; i < count; ++i) {
        if (handles[i] != nullptr && handles[i] != INVALID_HANDLE_VALUE) {
            ::CloseHandle(handles[i]);
            handles[i] = nullptr;
        }
    }
}

bool configureJob(HANDLE job, const supra_sandbox_policy& policy,
                  char* error, std::size_t error_cap) {
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if (policy.max_processes > 0U) {
        limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        limits.BasicLimitInformation.ActiveProcessLimit = policy.max_processes;
    }
    if (policy.max_address_space > 0U) {
        if (policy.max_address_space > std::numeric_limits<SIZE_T>::max()) {
            setError(error, error_cap, "address-space limit exceeds platform size");
            return false;
        }
        limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        limits.ProcessMemoryLimit = static_cast<SIZE_T>(policy.max_address_space);
    }
    if (policy.max_cpu_seconds > 0U) {
        if (policy.max_cpu_seconds >
            static_cast<std::uint64_t>(std::numeric_limits<LONGLONG>::max() / 10000000LL)) {
            setError(error, error_cap, "CPU limit exceeds Windows job-object range");
            return false;
        }
        limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_TIME;
        limits.BasicLimitInformation.PerJobUserTimeLimit.QuadPart =
            static_cast<LONGLONG>(policy.max_cpu_seconds) * 10000000LL;
    }
    if (::SetInformationJobObject(job, JobObjectExtendedLimitInformation,
                                  &limits, sizeof limits) == FALSE) {
        setWindowsError(error, error_cap, "SetInformationJobObject limits", ::GetLastError());
        return false;
    }
    return true;
}

bool makeProfile(ProcessState* state, PSID* sid, char* error, std::size_t error_cap) {
    const LONG serial = ::InterlockedIncrement(&g_profile_counter);
    std::swprintf(state->profile_name,
                  sizeof state->profile_name / sizeof state->profile_name[0],
                  L"SupraSandbox.%lu.%ld.%llu",
                  static_cast<unsigned long>(::GetCurrentProcessId()),
                  static_cast<long>(serial),
                  static_cast<unsigned long long>(::GetTickCount64()));
    const HRESULT created = ::CreateAppContainerProfile(
        state->profile_name, state->profile_name, L"Ephemeral supra sandbox",
        nullptr, 0, sid);
    if (FAILED(created)) {
        setWindowsError(error, error_cap, "CreateAppContainerProfile",
                        static_cast<DWORD>(HRESULT_CODE(created)));
        return false;
    }
    return true;
}

int statusFromProcess(HANDLE process, int* out_status) {
    DWORD code = 0;
    if (::GetExitCodeProcess(process, &code) == FALSE || code == STILL_ACTIVE) {
        return -1;
    }
    if (out_status != nullptr) {
        *out_status = code > static_cast<DWORD>(std::numeric_limits<int>::max())
                          ? -1
                          : static_cast<int>(code);
    }
    return 1;
}

void releaseOwned(supra_sandbox_process* process, bool terminate_tree) {
    if (process == nullptr) {
        return;
    }
    HANDLE native_process = reinterpret_cast<HANDLE>(process->native_process);
    auto* state = reinterpret_cast<ProcessState*>(process->native_job);
    process->native_process = 0U;
    process->native_job = 0U;
    if (native_process != nullptr && native_process != INVALID_HANDLE_VALUE) {
        ::CloseHandle(native_process);
    }
    destroyState(state, terminate_tree);
}

bool requiredTierAvailable(const supra_sandbox_policy& policy,
                           const supra_sandbox_capabilities& caps,
                           char* error, std::size_t error_cap) {
    if (caps.tier == SUPRA_SANDBOX_TIER_NONE) {
        setError(error, error_cap, caps.detail);
        return false;
    }
    if (policy.required_tier != 0U && caps.tier < policy.required_tier) {
        setError(error, error_cap, "policy requires tier above available: ");
        appendTruncated(error, error_cap, caps.detail);
        return false;
    }
    if (policy.isolate_processes != 0U || policy.isolate_ipc != 0U) {
        setError(error, error_cap,
                 "Windows AppContainer cannot enforce process or IPC namespace isolation");
        return false;
    }
    if (policy.network == SUPRA_SANDBOX_NET_PORTS) {
        setError(error, error_cap,
                 "policy requests per-port network but AppContainer cannot enforce port lists");
        return false;
    }
    if (policy.max_file_size != 0U) {
        setError(error, error_cap,
                 "Windows AppContainer cannot enforce maximum file size");
        return false;
    }
    return true;
}

}  // namespace

extern "C" {

void supra_sandbox_probe(supra_sandbox_capabilities* out) {
    if (out == nullptr) {
        return;
    }
    std::lock_guard<std::mutex> lock(g_probe_mutex);
    if (!g_caps_ready) {
        supra_sandbox_capabilities caps{};
        auto* state = static_cast<ProcessState*>(
            ::HeapAlloc(::GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(ProcessState)));
        PSID sid = nullptr;
        char error[SUPRA_SANDBOX_ERROR_LEN]{};
        if (state != nullptr && makeProfile(state, &sid, error, sizeof error)) {
            caps.tier = SUPRA_SANDBOX_TIER_APPCONTAINER;
            caps.network_isolation = 1;
            caps.port_granular_network = 0;
            setError(caps.detail, sizeof caps.detail,
                     "AppContainer available; per-port network policy is unsupported");
        } else {
            caps.tier = SUPRA_SANDBOX_TIER_NONE;
            setError(caps.detail, sizeof caps.detail,
                     state == nullptr ? "cannot allocate AppContainer probe state" : error);
        }
        if (sid != nullptr) {
            ::FreeSid(sid);
        }
        destroyState(state, false);
        g_caps = caps;
        g_caps_ready = true;
    }
    *out = g_caps;
    if (g_forced_tier != SUPRA_SANDBOX_TIER_UNSET) {
        out->tier = g_forced_tier;
        if (out->tier == SUPRA_SANDBOX_TIER_NONE) {
            out->network_isolation = 0;
            setError(out->detail, sizeof out->detail,
                     "tier forced to NONE for testing; sandbox unavailable");
        }
    }
}

void supra_sandbox_force_tier_for_testing(std::uint8_t tier) {
    std::lock_guard<std::mutex> lock(g_probe_mutex);
    g_forced_tier = tier;
}

int supra_sandbox_spawn(const supra_sandbox_policy* policy, const supra_sandbox_command* command,
                        supra_sandbox_process* out) {
    std::lock_guard<std::mutex> spawn_lock(g_spawn_mutex);
    if (out == nullptr) {
        return 0;
    }
    *out = supra_sandbox_process{};
    out->pid = -1;
    if (command == nullptr || command->program == nullptr || command->argv == nullptr) {
        setError(out->error, sizeof out->error, "command, program, and argv are required");
        return 0;
    }
    if (!supra::sandbox::detail::validatePolicy(policy, out->error, sizeof out->error)) {
        return 0;
    }
    supra_sandbox_capabilities caps{};
    supra_sandbox_probe(&caps);
    if (!requiredTierAvailable(*policy, caps, out->error, sizeof out->error)) {
        return 0;
    }

    auto* state = static_cast<ProcessState*>(
        ::HeapAlloc(::GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(ProcessState)));
    if (state == nullptr) {
        setError(out->error, sizeof out->error, "cannot allocate Windows sandbox state");
        return 0;
    }
    PSID appcontainer_sid = nullptr;
    if (!makeProfile(state, &appcontainer_sid, out->error, sizeof out->error)) {
        destroyState(state, false);
        return 0;
    }

    bool setup_ok = true;
    for (std::size_t i = 0; i < policy->path_count && setup_ok; ++i) {
        wchar_t* path = nullptr;
        if (!utf8ToWide(policy->paths[i].path, &path)) {
            setError(out->error, sizeof out->error, "policy path is not strict UTF-8");
            setup_ok = false;
        } else {
            setup_ok = grantPath(state, path, appcontainer_sid, policy->paths[i].access,
                                 out->error, sizeof out->error);
        }
        freeWide(path);
    }

    wchar_t* program = nullptr;
    wchar_t* working_dir = nullptr;
    WideBuffer command_line{};
    WideBuffer environment{};
    if (setup_ok && (!utf8ToWide(command->program, &program) ||
                     !utf8ToWide(command->working_dir, &working_dir) ||
                     !buildCommandLine(command->argv, &command_line) ||
                     !buildEnvironment(command->envp, &environment))) {
        setError(out->error, sizeof out->error,
                 "command contains invalid UTF-8 or cannot be represented safely");
        setup_ok = false;
    }
    if (setup_ok && !isCanonicalAbsolutePath(program)) {
        setError(out->error, sizeof out->error, "program path is not canonical and absolute");
        setup_ok = false;
    }
    if (setup_ok && working_dir != nullptr && !isCanonicalAbsolutePath(working_dir)) {
        setError(out->error, sizeof out->error,
                 "working directory is not canonical and absolute");
        setup_ok = false;
    }

    HANDLE stdio[3]{};
    if (setup_ok) {
        setup_ok = duplicateStandardHandle(command->stdin_fd, true, &stdio[0],
                                           out->error, sizeof out->error) &&
                   duplicateStandardHandle(command->stdout_fd, false, &stdio[1],
                                           out->error, sizeof out->error) &&
                   duplicateStandardHandle(command->stderr_fd, false, &stdio[2],
                                           out->error, sizeof out->error);
    }

    state->job = setup_ok ? ::CreateJobObjectW(nullptr, nullptr) : nullptr;
    if (setup_ok && state->job == nullptr) {
        setWindowsError(out->error, sizeof out->error, "CreateJobObjectW", ::GetLastError());
        setup_ok = false;
    }
    if (setup_ok && !configureJob(state->job, *policy, out->error, sizeof out->error)) {
        setup_ok = false;
    }

    BYTE internet_sid[SECURITY_MAX_SID_SIZE]{};
    DWORD internet_sid_size = sizeof internet_sid;
    SID_AND_ATTRIBUTES capability{};
    SECURITY_CAPABILITIES security_capabilities{};
    security_capabilities.AppContainerSid = appcontainer_sid;
    if (setup_ok && policy->network == SUPRA_SANDBOX_NET_FULL) {
        if (::CreateWellKnownSid(WinCapabilityInternetClientSid, nullptr,
                                 internet_sid, &internet_sid_size) == FALSE) {
            setWindowsError(out->error, sizeof out->error,
                            "CreateWellKnownSid internetClient", ::GetLastError());
            setup_ok = false;
        } else {
            capability.Sid = internet_sid;
            capability.Attributes = SE_GROUP_ENABLED;
            security_capabilities.Capabilities = &capability;
            security_capabilities.CapabilityCount = 1;
        }
    }

    SIZE_T attribute_bytes = 0;
    LPPROC_THREAD_ATTRIBUTE_LIST attributes = nullptr;
    if (setup_ok) {
        static_cast<void>(::InitializeProcThreadAttributeList(nullptr, 2, 0, &attribute_bytes));
        attributes = static_cast<LPPROC_THREAD_ATTRIBUTE_LIST>(
            ::HeapAlloc(::GetProcessHeap(), 0, attribute_bytes));
        if (attributes == nullptr ||
            ::InitializeProcThreadAttributeList(attributes, 2, 0, &attribute_bytes) == FALSE ||
            ::UpdateProcThreadAttribute(attributes, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                                        stdio, sizeof stdio, nullptr, nullptr) == FALSE ||
            ::UpdateProcThreadAttribute(attributes, 0,
                                        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
                                        &security_capabilities, sizeof security_capabilities,
                                        nullptr, nullptr) == FALSE) {
            setWindowsError(out->error, sizeof out->error,
                            "configure process attribute list", ::GetLastError());
            setup_ok = false;
        }
    }

    PROCESS_INFORMATION info{};
    STARTUPINFOEXW startup{};
    startup.StartupInfo.cb = sizeof startup;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdio[0];
    startup.StartupInfo.hStdOutput = stdio[1];
    startup.StartupInfo.hStdError = stdio[2];
    startup.lpAttributeList = attributes;
    bool child_started = false;
    if (setup_ok && ::CreateProcessW(
                        program, command_line.data, nullptr, nullptr, TRUE,
                        EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED |
                            CREATE_UNICODE_ENVIRONMENT | CREATE_NEW_PROCESS_GROUP,
                        environment.data, working_dir, &startup.StartupInfo, &info) == FALSE) {
        setWindowsError(out->error, sizeof out->error,
                        "CreateProcessW AppContainer", ::GetLastError());
        setup_ok = false;
    } else if (setup_ok) {
        child_started = true;
    }
    if (setup_ok && ::AssignProcessToJobObject(state->job, info.hProcess) == FALSE) {
        setWindowsError(out->error, sizeof out->error,
                        "AssignProcessToJobObject", ::GetLastError());
        static_cast<void>(::TerminateProcess(info.hProcess, 127));
        static_cast<void>(::WaitForSingleObject(info.hProcess, 5000));
        setup_ok = false;
    }
    if (setup_ok && ::ResumeThread(info.hThread) == static_cast<DWORD>(-1)) {
        setWindowsError(out->error, sizeof out->error, "ResumeThread", ::GetLastError());
        static_cast<void>(::TerminateJobObject(state->job, 127));
        static_cast<void>(::WaitForSingleObject(info.hProcess, 5000));
        setup_ok = false;
    }

    if (info.hThread != nullptr) {
        ::CloseHandle(info.hThread);
    }
    closeHandles(stdio, 3);
    if (attributes != nullptr) {
        ::DeleteProcThreadAttributeList(attributes);
        ::HeapFree(::GetProcessHeap(), 0, attributes);
    }
    freeBuffer(&command_line);
    freeBuffer(&environment);
    freeWide(program);
    freeWide(working_dir);
    if (appcontainer_sid != nullptr) {
        ::FreeSid(appcontainer_sid);
    }

    if (!setup_ok) {
        if (child_started && info.hProcess != nullptr) {
            static_cast<void>(::TerminateProcess(info.hProcess, 127));
            static_cast<void>(::WaitForSingleObject(info.hProcess, 5000));
        }
        if (info.hProcess != nullptr) {
            ::CloseHandle(info.hProcess);
        }
        destroyState(state, true);
        return 0;
    }

    out->pid = static_cast<std::int64_t>(info.dwProcessId);
    out->tier = SUPRA_SANDBOX_TIER_APPCONTAINER;
    state->process = info.hProcess;
    out->native_process = reinterpret_cast<std::uintptr_t>(info.hProcess);
    out->native_job = reinterpret_cast<std::uintptr_t>(state);
    return 1;
}

int supra_sandbox_wait(const supra_sandbox_process* process, std::uint32_t timeout_ms,
                       int* out_status) {
    if (process == nullptr || process->pid < 0 || process->native_process == 0U) {
        return -1;
    }
    HANDLE native_process = reinterpret_cast<HANDLE>(process->native_process);
    const DWORD timeout = timeout_ms == 0U ? INFINITE : timeout_ms;
    const DWORD wait = ::WaitForSingleObject(native_process, timeout);
    if (wait == WAIT_TIMEOUT) {
        return 0;
    }
    if (wait != WAIT_OBJECT_0) {
        return -1;
    }
    const int result = statusFromProcess(native_process, out_status);
    if (result == 1) {
        // The leader may exit while descendants still hold sandbox ACL access.
        // Closing a kill-on-close job first removes the whole tree, then ACLs can
        // be restored without a surviving descendant retaining that authority.
        releaseOwned(const_cast<supra_sandbox_process*>(process), true);
    }
    return result;
}

int supra_sandbox_kill(const supra_sandbox_process* process, std::uint32_t grace_ms) {
    if (process == nullptr || process->pid < 0 || process->native_job == 0U) {
        return 0;
    }
    int status = 0;
    if (grace_ms > 0U && supra_sandbox_wait(process, grace_ms, &status) == 1) {
        return 1;
    }
    auto* state = reinterpret_cast<ProcessState*>(process->native_job);
    if (state == nullptr || state->job == nullptr ||
        ::TerminateJobObject(state->job, 137) == FALSE) {
        return 0;
    }
    return supra_sandbox_wait(process, 5000U, &status) == 1 ? 1 : 0;
}

void supra_sandbox_release(supra_sandbox_process* process) {
    // A detached Windows process cannot safely outlive the ACL grants, job limits,
    // and AppContainer profile owned by this opaque handle. Refuse to weaken the
    // running boundary: terminate and clean it just as a dropped owned process.
    releaseOwned(process, true);
}

}  // extern "C"

#endif  // _WIN32
