#define WIN32_LEAN_AND_MEAN
#define _WIN32_WINNT 0x0601
#include <windows.h>
#include <winnt.h>

#define MAX_IMPORT_DESCRIPTORS 4096u
#define MAX_IMPORTS_PER_LIBRARY 65536u
#define MAX_IMPORT_STRING 32768u

typedef struct PE_VIEW {
    const BYTE *base;
    SIZE_T size;
    BOOL is64;
    ULONGLONG image_base;
    DWORD size_of_headers;
    const IMAGE_SECTION_HEADER *sections;
    WORD section_count;
    IMAGE_DATA_DIRECTORY imports;
    IMAGE_DATA_DIRECTORY delay_imports;
} PE_VIEW;

typedef struct AUDIT_STATE {
    HANDLE log;
    DWORD libraries;
    DWORD procedures;
    DWORD missing;
} AUDIT_STATE;

static SIZE_T ascii_len(const char *value) {
    SIZE_T length = 0;
    while (value[length] != 0) ++length;
    return length;
}

static SIZE_T wide_len(const WCHAR *value) {
    SIZE_T length = 0;
    while (value[length] != 0) ++length;
    return length;
}

static void write_bytes(HANDLE file, const void *data, DWORD size) {
    DWORD written;
    if (file != INVALID_HANDLE_VALUE && size != 0) WriteFile(file, data, size, &written, NULL);
}

static void write_ascii(HANDLE file, const char *value) {
    SIZE_T length = ascii_len(value);
    if (length <= MAXDWORD) write_bytes(file, value, (DWORD)length);
}

static void write_wide_utf8(HANDLE file, const WCHAR *value) {
    char buffer[1024];
    const WCHAR *cursor = value;
    while (*cursor != 0) {
        int count = (int)wide_len(cursor);
        int consumed;
        int output;
        if (count > 250) count = 250;
        consumed = count;
        output = WideCharToMultiByte(CP_UTF8, 0, cursor, consumed, buffer, (int)sizeof(buffer), NULL, NULL);
        if (output <= 0) return;
        write_bytes(file, buffer, (DWORD)output);
        cursor += consumed;
    }
}

static void write_u32(HANDLE file, DWORD value) {
    char buffer[16];
    DWORD cursor = sizeof(buffer);
    do {
        buffer[--cursor] = (char)('0' + (value % 10));
        value /= 10;
    } while (value != 0);
    write_bytes(file, buffer + cursor, (DWORD)(sizeof(buffer) - cursor));
}

static BOOL range_ok(SIZE_T offset, SIZE_T needed, SIZE_T size) {
    return offset <= size && needed <= size - offset;
}

static const void *rva_ptr(const PE_VIEW *view, DWORD rva, SIZE_T needed) {
    WORD index;
    if (rva < view->size_of_headers && range_ok((SIZE_T)rva, needed, view->size)) {
        return view->base + rva;
    }
    for (index = 0; index < view->section_count; ++index) {
        const IMAGE_SECTION_HEADER *section = &view->sections[index];
        DWORD span = section->Misc.VirtualSize > section->SizeOfRawData
            ? section->Misc.VirtualSize : section->SizeOfRawData;
        ULONGLONG end = (ULONGLONG)section->VirtualAddress + span;
        if ((ULONGLONG)rva >= section->VirtualAddress && (ULONGLONG)rva < end) {
            DWORD delta = rva - section->VirtualAddress;
            ULONGLONG offset;
            if (delta >= section->SizeOfRawData) return NULL;
            offset = (ULONGLONG)section->PointerToRawData + delta;
            if (offset > (ULONGLONG)(SIZE_T)-1 || !range_ok((SIZE_T)offset, needed, view->size)) return NULL;
            return view->base + (SIZE_T)offset;
        }
    }
    return NULL;
}

static const char *rva_string(const PE_VIEW *view, DWORD rva) {
    DWORD index;
    const char *value = (const char *)rva_ptr(view, rva, 1);
    SIZE_T start;
    if (value == NULL) return NULL;
    start = (SIZE_T)((const BYTE *)value - view->base);
    for (index = 0; index < MAX_IMPORT_STRING; ++index) {
        if ((SIZE_T)index >= view->size - start) return NULL;
        if (value[index] == 0) return value;
    }
    return NULL;
}

static BOOL safe_library_name(const char *name) {
    SIZE_T index;
    SIZE_T length = ascii_len(name);
    if (length == 0 || length >= MAX_PATH) return FALSE;
    for (index = 0; index < length; ++index) {
        BYTE ch = (BYTE)name[index];
        if (ch < 0x20 || ch > 0x7e || ch == '/' || ch == '\\' || ch == ':') return FALSE;
    }
    return TRUE;
}

static BOOL parse_pe(const BYTE *base, SIZE_T size, PE_VIEW *view) {
    const IMAGE_DOS_HEADER *dos;
    const DWORD *signature;
    const IMAGE_FILE_HEADER *file;
    const BYTE *optional;
    WORD magic;
    SIZE_T nt_offset;
    SIZE_T optional_offset;
    SIZE_T sections_offset;
    DWORD directory_count;
    const IMAGE_DATA_DIRECTORY *directories;

    if (!range_ok(0, sizeof(IMAGE_DOS_HEADER), size)) return FALSE;
    dos = (const IMAGE_DOS_HEADER *)base;
    if (dos->e_magic != IMAGE_DOS_SIGNATURE || dos->e_lfanew < 0) return FALSE;
    nt_offset = (SIZE_T)dos->e_lfanew;
    if (!range_ok(nt_offset, sizeof(DWORD) + sizeof(IMAGE_FILE_HEADER), size)) return FALSE;
    signature = (const DWORD *)(base + nt_offset);
    if (*signature != IMAGE_NT_SIGNATURE) return FALSE;
    file = (const IMAGE_FILE_HEADER *)(base + nt_offset + sizeof(DWORD));
    optional_offset = nt_offset + sizeof(DWORD) + sizeof(IMAGE_FILE_HEADER);
    if (!range_ok(optional_offset, file->SizeOfOptionalHeader, size) || file->SizeOfOptionalHeader < sizeof(WORD)) return FALSE;
    optional = base + optional_offset;
    magic = *(const WORD *)optional;
    if (magic == IMAGE_NT_OPTIONAL_HDR64_MAGIC) {
        const IMAGE_OPTIONAL_HEADER64 *header;
        if (file->SizeOfOptionalHeader < sizeof(IMAGE_OPTIONAL_HEADER64)) return FALSE;
        header = (const IMAGE_OPTIONAL_HEADER64 *)optional;
        view->is64 = TRUE;
        view->image_base = header->ImageBase;
        view->size_of_headers = header->SizeOfHeaders;
        directory_count = header->NumberOfRvaAndSizes;
        directories = header->DataDirectory;
    } else if (magic == IMAGE_NT_OPTIONAL_HDR32_MAGIC) {
        const IMAGE_OPTIONAL_HEADER32 *header;
        if (file->SizeOfOptionalHeader < sizeof(IMAGE_OPTIONAL_HEADER32)) return FALSE;
        header = (const IMAGE_OPTIONAL_HEADER32 *)optional;
        view->is64 = FALSE;
        view->image_base = header->ImageBase;
        view->size_of_headers = header->SizeOfHeaders;
        directory_count = header->NumberOfRvaAndSizes;
        directories = header->DataDirectory;
    } else {
        return FALSE;
    }
    sections_offset = optional_offset + file->SizeOfOptionalHeader;
    if (!range_ok(sections_offset, (SIZE_T)file->NumberOfSections * sizeof(IMAGE_SECTION_HEADER), size)) return FALSE;
    view->base = base;
    view->size = size;
    view->sections = (const IMAGE_SECTION_HEADER *)(base + sections_offset);
    view->section_count = file->NumberOfSections;
    view->imports.VirtualAddress = 0;
    view->imports.Size = 0;
    view->delay_imports.VirtualAddress = 0;
    view->delay_imports.Size = 0;
    if (directory_count > IMAGE_DIRECTORY_ENTRY_IMPORT)
        view->imports = directories[IMAGE_DIRECTORY_ENTRY_IMPORT];
    if (directory_count > IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT)
        view->delay_imports = directories[IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT];
    return TRUE;
}

static void log_missing(AUDIT_STATE *state, BOOL delay, const char *library,
                        const char *procedure, WORD ordinal, BOOL by_ordinal,
                        const char *reason, DWORD error) {
    write_ascii(state->log, "MISSING [");
    write_ascii(state->log, delay ? "delay" : "static");
    write_ascii(state->log, "] ");
    write_ascii(state->log, library);
    write_ascii(state->log, "!");
    if (by_ordinal) {
        write_ascii(state->log, "ordinal #");
        write_u32(state->log, ordinal);
    } else {
        write_ascii(state->log, procedure);
    }
    write_ascii(state->log, " (");
    write_ascii(state->log, reason);
    if (error != 0) {
        write_ascii(state->log, ", GetLastError=");
        write_u32(state->log, error);
    }
    write_ascii(state->log, ")\r\n");
    ++state->missing;
}

static HMODULE load_library_for_audit(const char *name, DWORD *error) {
    WCHAR wide[MAX_PATH];
    SIZE_T index;
    HMODULE module;
    SIZE_T length = ascii_len(name);
    if (length >= MAX_PATH) {
        *error = ERROR_FILENAME_EXCED_RANGE;
        return NULL;
    }
    for (index = 0; index <= length; ++index) wide[index] = (WCHAR)(BYTE)name[index];
    SetLastError(ERROR_SUCCESS);
    module = LoadLibraryExW(wide, NULL, DONT_RESOLVE_DLL_REFERENCES);
    if (module == NULL) *error = GetLastError();
    return module;
}

static BOOL audit_thunks(const PE_VIEW *view, DWORD thunk_rva, BOOL delay,
                         const char *library, HMODULE module, DWORD load_error,
                         AUDIT_STATE *state) {
    DWORD index;
    for (index = 0; index < MAX_IMPORTS_PER_LIBRARY; ++index) {
        ULONGLONG entry_rva;
        ULONGLONG value;
        BOOL by_ordinal;
        WORD ordinal = 0;
        const char *procedure = NULL;
        FARPROC address = NULL;
        entry_rva = (ULONGLONG)thunk_rva + (ULONGLONG)index * (view->is64 ? 8u : 4u);
        if (entry_rva > MAXDWORD) return FALSE;
        if (view->is64) {
            const ULONGLONG *entry = (const ULONGLONG *)rva_ptr(view, (DWORD)entry_rva, sizeof(ULONGLONG));
            if (entry == NULL) return FALSE;
            value = *entry;
            by_ordinal = (value & IMAGE_ORDINAL_FLAG64) != 0;
        } else {
            const DWORD *entry = (const DWORD *)rva_ptr(view, (DWORD)entry_rva, sizeof(DWORD));
            if (entry == NULL) return FALSE;
            value = *entry;
            by_ordinal = (value & IMAGE_ORDINAL_FLAG32) != 0;
        }
        if (value == 0) return TRUE;
        ++state->procedures;
        if (by_ordinal) {
            ordinal = (WORD)(value & 0xffffu);
            if (module != NULL) address = GetProcAddress(module, (LPCSTR)(ULONG_PTR)ordinal);
        } else {
            if (value > MAXDWORD) return FALSE;
            procedure = rva_string(view, (DWORD)value + sizeof(WORD));
            if (procedure == NULL) return FALSE;
            if (module != NULL) address = GetProcAddress(module, procedure);
        }
        if (module == NULL) {
            log_missing(state, delay, library, procedure, ordinal, by_ordinal,
                        "DLL unavailable", load_error);
        } else if (address == NULL) {
            log_missing(state, delay, library, procedure, ordinal, by_ordinal,
                        "procedure unavailable", 0);
        }
    }
    return FALSE;
}

static BOOL audit_library(const PE_VIEW *view, DWORD name_rva, DWORD thunk_rva,
                          BOOL delay, AUDIT_STATE *state) {
    const char *library = rva_string(view, name_rva);
    DWORD load_error = 0;
    HMODULE module;
    BOOL result;
    if (library == NULL || !safe_library_name(library) || thunk_rva == 0) return FALSE;
    ++state->libraries;
    module = load_library_for_audit(library, &load_error);
    result = audit_thunks(view, thunk_rva, delay, library, module, load_error, state);
    if (module != NULL) FreeLibrary(module);
    return result;
}

static BOOL audit_imports(const PE_VIEW *view, AUDIT_STATE *state) {
    DWORD index;
    DWORD limit;
    if (view->imports.VirtualAddress == 0 || view->imports.Size == 0) return TRUE;
    limit = view->imports.Size / sizeof(IMAGE_IMPORT_DESCRIPTOR);
    if (limit == 0 || limit > MAX_IMPORT_DESCRIPTORS) return FALSE;
    for (index = 0; index < limit; ++index) {
        ULONGLONG descriptor_rva = (ULONGLONG)view->imports.VirtualAddress +
            (ULONGLONG)index * sizeof(IMAGE_IMPORT_DESCRIPTOR);
        const IMAGE_IMPORT_DESCRIPTOR *descriptor = (const IMAGE_IMPORT_DESCRIPTOR *)rva_ptr(
            view, descriptor_rva <= MAXDWORD ? (DWORD)descriptor_rva : 0,
            sizeof(IMAGE_IMPORT_DESCRIPTOR));
        DWORD thunk;
        if (descriptor_rva > MAXDWORD || descriptor == NULL) return FALSE;
        if (descriptor->OriginalFirstThunk == 0 && descriptor->FirstThunk == 0 && descriptor->Name == 0) return TRUE;
        thunk = descriptor->OriginalFirstThunk != 0 ? descriptor->OriginalFirstThunk : descriptor->FirstThunk;
        if (!audit_library(view, descriptor->Name, thunk, FALSE, state)) return FALSE;
    }
    return FALSE;
}

static BOOL delay_value_to_rva(const PE_VIEW *view, DWORD attributes, DWORD value, DWORD *rva) {
    ULONGLONG converted;
    if ((attributes & 1u) != 0) {
        *rva = value;
        return TRUE;
    }
    if ((ULONGLONG)value < view->image_base) return FALSE;
    converted = (ULONGLONG)value - view->image_base;
    if (converted > MAXDWORD) return FALSE;
    *rva = (DWORD)converted;
    return TRUE;
}

static BOOL audit_delay_imports(const PE_VIEW *view, AUDIT_STATE *state) {
    DWORD index;
    DWORD limit;
    if (view->delay_imports.VirtualAddress == 0 || view->delay_imports.Size == 0) return TRUE;
    limit = view->delay_imports.Size / 32u;
    if (limit == 0 || limit > MAX_IMPORT_DESCRIPTORS) return FALSE;
    for (index = 0; index < limit; ++index) {
        ULONGLONG descriptor_rva = (ULONGLONG)view->delay_imports.VirtualAddress +
            (ULONGLONG)index * 32u;
        const DWORD *descriptor = (const DWORD *)rva_ptr(
            view, descriptor_rva <= MAXDWORD ? (DWORD)descriptor_rva : 0, 32u);
        DWORD name_rva;
        DWORD thunk_rva;
        DWORD thunk_value;
        if (descriptor_rva > MAXDWORD || descriptor == NULL) return FALSE;
        if (descriptor[0] == 0 && descriptor[1] == 0 && descriptor[3] == 0 && descriptor[4] == 0) return TRUE;
        thunk_value = descriptor[4] != 0 ? descriptor[4] : descriptor[3];
        if (!delay_value_to_rva(view, descriptor[0], descriptor[1], &name_rva) ||
            !delay_value_to_rva(view, descriptor[0], thunk_value, &thunk_rva)) return FALSE;
        if (!audit_library(view, name_rva, thunk_rva, TRUE, state)) return FALSE;
    }
    return FALSE;
}

static BOOL append_component(WCHAR *path, DWORD capacity, const WCHAR *component) {
    DWORD length = (DWORD)wide_len(path);
    DWORD component_length = (DWORD)wide_len(component);
    if (length != 0 && path[length - 1] != L'\\') {
        if (length + 1 >= capacity) return FALSE;
        path[length++] = L'\\';
    }
    if (component_length >= capacity - length) return FALSE;
    CopyMemory(path + length, component, (component_length + 1) * sizeof(WCHAR));
    return TRUE;
}

static BOOL executable_directory(WCHAR *path, DWORD capacity) {
    DWORD length = GetModuleFileNameW(NULL, path, capacity);
    if (length == 0 || length >= capacity) return FALSE;
    while (length != 0 && path[length - 1] != L'\\') --length;
    if (length == 0) return FALSE;
    path[length - 1] = 0;
    return TRUE;
}

static HANDLE open_log(const WCHAR *directory, WCHAR *log_path, DWORD capacity) {
    CopyMemory(log_path, directory, (wide_len(directory) + 1) * sizeof(WCHAR));
    if (!append_component(log_path, capacity, L"log")) return INVALID_HANDLE_VALUE;
    if (!CreateDirectoryW(log_path, NULL) && GetLastError() != ERROR_ALREADY_EXISTS) return INVALID_HANDLE_VALUE;
    if (!append_component(log_path, capacity, L"LetRecovery.WinAPI.log")) return INVALID_HANDLE_VALUE;
    return CreateFileW(log_path, FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL,
                       OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
}

int wmain(int argc, WCHAR **argv) {
    WCHAR directory[MAX_PATH];
    WCHAR target[MAX_PATH];
    WCHAR log_path[MAX_PATH];
    HANDLE log = INVALID_HANDLE_VALUE;
    HANDLE file = INVALID_HANDLE_VALUE;
    HANDLE mapping = NULL;
    const BYTE *mapped = NULL;
    LARGE_INTEGER file_size;
    PE_VIEW view;
    AUDIT_STATE state;
    SYSTEMTIME time;
    int result = 1;

    if (!executable_directory(directory, MAX_PATH)) return 1;
    if (argc > 1) {
        if (wide_len(argv[1]) >= MAX_PATH) return 1;
        CopyMemory(target, argv[1], (wide_len(argv[1]) + 1) * sizeof(WCHAR));
    } else {
        CopyMemory(target, directory, (wide_len(directory) + 1) * sizeof(WCHAR));
        if (!append_component(target, MAX_PATH, L"LetRecovery.exe")) return 1;
    }
    log = open_log(directory, log_path, MAX_PATH);
    if (log == INVALID_HANDLE_VALUE) return 1;
    GetLocalTime(&time);
    write_ascii(log, "=== LetRecovery WinAPI compatibility audit ===\r\nTarget: ");
    write_wide_utf8(log, target);
    write_ascii(log, "\r\nLocal time: ");
    write_u32(log, time.wYear); write_ascii(log, "-"); write_u32(log, time.wMonth);
    write_ascii(log, "-"); write_u32(log, time.wDay); write_ascii(log, " ");
    write_u32(log, time.wHour); write_ascii(log, ":"); write_u32(log, time.wMinute);
    write_ascii(log, ":"); write_u32(log, time.wSecond); write_ascii(log, "\r\n");

    file = CreateFileW(target, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_DELETE, NULL,
                       OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN, NULL);
    if (file == INVALID_HANDLE_VALUE || !GetFileSizeEx(file, &file_size) ||
        file_size.QuadPart <= 0 || (ULONGLONG)file_size.QuadPart > (ULONGLONG)(SIZE_T)-1) {
        write_ascii(log, "FATAL: cannot open or size the target executable.\r\n\r\n");
        goto cleanup;
    }
    mapping = CreateFileMappingW(file, NULL, PAGE_READONLY, 0, 0, NULL);
    if (mapping == NULL) {
        write_ascii(log, "FATAL: cannot map the target executable.\r\n\r\n");
        goto cleanup;
    }
    mapped = (const BYTE *)MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
    if (mapped == NULL || !parse_pe(mapped, (SIZE_T)file_size.QuadPart, &view)) {
        write_ascii(log, "FATAL: the target is not a valid, supported PE32/PE32+ image.\r\n\r\n");
        goto cleanup;
    }
    SetDllDirectoryW(L"");
    state.log = log;
    state.libraries = 0;
    state.procedures = 0;
    state.missing = 0;
    if (!audit_imports(&view, &state) || !audit_delay_imports(&view, &state)) {
        write_ascii(log, "FATAL: malformed or out-of-range import data was rejected.\r\n\r\n");
        goto cleanup;
    }
    write_ascii(log, "Imported libraries: "); write_u32(log, state.libraries);
    write_ascii(log, "\r\nImported procedures: "); write_u32(log, state.procedures);
    write_ascii(log, "\r\nMissing procedures: "); write_u32(log, state.missing);
    write_ascii(log, state.missing == 0
        ? "\r\nResult: compatible on this Windows installation.\r\n\r\n"
        : "\r\nResult: incompatible imports were found.\r\n\r\n");
    result = state.missing == 0 ? 0 : 2;

cleanup:
    if (mapped != NULL) UnmapViewOfFile(mapped);
    if (mapping != NULL) CloseHandle(mapping);
    if (file != INVALID_HANDLE_VALUE) CloseHandle(file);
    if (log != INVALID_HANDLE_VALUE) CloseHandle(log);
    return result;
}
