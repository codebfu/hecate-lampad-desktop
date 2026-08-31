/*
 * Tiny MSI custom-action helper: register/unregister the desktop logon task
 * via schtasks without loading hecate-lampad-desktop.exe (avoids MSI 1722 when
 * the helper PE imports APIs missing on older Windows).
 *
 * Usage:
 *   register-logon-task.exe install
 *   register-logon-task.exe uninstall
 *   register-logon-task.exe start
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <windows.h>

static int run_cmdline(char *cmd) {
    STARTUPINFOA si;
    PROCESS_INFORMATION pi;
    SECURITY_ATTRIBUTES sa;
    HANDLE rd = NULL, wr = NULL;
    DWORD exit_code = 1;
    char buf[512];
    DWORD nread;

    ZeroMemory(&si, sizeof(si));
    si.cb = sizeof(si);
    ZeroMemory(&pi, sizeof(pi));
    ZeroMemory(&sa, sizeof(sa));
    sa.nLength = sizeof(sa);
    sa.bInheritHandle = TRUE;

    if (!CreatePipe(&rd, &wr, &sa, 0)) {
        fprintf(stderr, "CreatePipe failed: %lu\n", GetLastError());
        return 1;
    }
    SetHandleInformation(rd, HANDLE_FLAG_INHERIT, 0);
    si.dwFlags |= STARTF_USESTDHANDLES;
    si.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
    si.hStdOutput = wr;
    si.hStdError = wr;

    /*
     * lpApplicationName MUST be NULL when the command line contains quoted
     * arguments with spaces. Passing both breaks schtasks parsing
     * (ERROR: Invalid argument/option - 'Lampad').
     */
    if (!CreateProcessA(NULL, cmd, NULL, NULL, TRUE, CREATE_NO_WINDOW, NULL, NULL, &si, &pi)) {
        fprintf(stderr, "CreateProcess failed: %lu\n", GetLastError());
        CloseHandle(rd);
        CloseHandle(wr);
        return 1;
    }
    CloseHandle(wr);

    while (ReadFile(rd, buf, sizeof(buf) - 1, &nread, NULL) && nread > 0) {
        buf[nread] = '\0';
        fputs(buf, stderr);
    }
    CloseHandle(rd);

    WaitForSingleObject(pi.hProcess, INFINITE);
    GetExitCodeProcess(pi.hProcess, &exit_code);
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);
    return (int)exit_code;
}

static int schtasks_path(char *out, size_t out_len) {
    char system_dir[MAX_PATH];
    if (!GetSystemDirectoryA(system_dir, MAX_PATH)) {
        fprintf(stderr, "GetSystemDirectory failed: %lu\n", GetLastError());
        return 1;
    }
    if (snprintf(out, out_len, "%s\\schtasks.exe", system_dir) >= (int)out_len) {
        fprintf(stderr, "schtasks path too long\n");
        return 1;
    }
    return 0;
}

static int task_exists(void) {
    char schtasks[MAX_PATH];
    char cmd[512];
    if (schtasks_path(schtasks, sizeof(schtasks)) != 0) {
        return 0;
    }
    if (snprintf(cmd, sizeof(cmd),
                 "\"%s\" /Query /TN \"Hecate Lampad Desktop\"", schtasks) >= (int)sizeof(cmd)) {
        return 0;
    }
    return run_cmdline(cmd) == 0;
}

static int install_task(void) {
    char exe_path[MAX_PATH];
    char xml_path[MAX_PATH];
    char schtasks[MAX_PATH];
    char cmd[1536];
    char *slash;
    DWORD len;
    int rc;

    len = GetModuleFileNameA(NULL, exe_path, MAX_PATH);
    if (len == 0 || len >= MAX_PATH) {
        fprintf(stderr, "GetModuleFileName failed\n");
        return 1;
    }
    slash = strrchr(exe_path, '\\');
    if (!slash) {
        fprintf(stderr, "invalid module path\n");
        return 1;
    }
    *slash = '\0';
    if (snprintf(xml_path, sizeof(xml_path), "%s\\hecate-lampad-desktop-logon.xml", exe_path) >=
        (int)sizeof(xml_path)) {
        fprintf(stderr, "xml path too long\n");
        return 1;
    }
    if (GetFileAttributesA(xml_path) == INVALID_FILE_ATTRIBUTES) {
        fprintf(stderr, "logon XML not found: %s\n", xml_path);
        return 1;
    }
    if (schtasks_path(schtasks, sizeof(schtasks)) != 0) {
        return 1;
    }
    if (snprintf(cmd, sizeof(cmd),
                 "\"%s\" /Create /TN \"Hecate Lampad Desktop\" /XML \"%s\" /F", schtasks,
                 xml_path) >= (int)sizeof(cmd)) {
        fprintf(stderr, "schtasks args too long\n");
        return 1;
    }
    rc = run_cmdline(cmd);
    if (rc == 0 || task_exists()) {
        return 0;
    }
    fprintf(stderr, "schtasks /Create failed with %d\n", rc);
    return 1;
}

static int uninstall_task(void) {
    char schtasks[MAX_PATH];
    char cmd[512];
    if (schtasks_path(schtasks, sizeof(schtasks)) != 0) {
        return 0;
    }
    if (snprintf(cmd, sizeof(cmd),
                 "\"%s\" /Delete /TN \"Hecate Lampad Desktop\" /F", schtasks) >= (int)sizeof(cmd)) {
        return 0;
    }
    /* Non-zero is fine when the task is already absent. */
    (void)run_cmdline(cmd);
    return 0;
}

static int start_task(void) {
    char schtasks[MAX_PATH];
    char cmd[512];
    if (schtasks_path(schtasks, sizeof(schtasks)) != 0) {
        return 0;
    }
    if (snprintf(cmd, sizeof(cmd),
                 "\"%s\" /Run /TN \"Hecate Lampad Desktop\"", schtasks) >= (int)sizeof(cmd)) {
        return 0;
    }
    /* Best-effort; group-principal tasks may not attach to an interactive desktop. */
    (void)run_cmdline(cmd);
    return 0;
}

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "install") == 0) {
        return install_task();
    }
    if (argc >= 2 && strcmp(argv[1], "uninstall") == 0) {
        return uninstall_task();
    }
    if (argc >= 2 && strcmp(argv[1], "start") == 0) {
        return start_task();
    }
    fprintf(stderr, "usage: %s install|uninstall|start\n", argc > 0 ? argv[0] : "register-logon-task");
    return 1;
}
