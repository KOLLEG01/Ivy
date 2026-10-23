#include <Windows.h>
#include <appmodel.h>
#include <tlhelp32.h>
#include <cstddef>
#include <type_traits>
static_assert(sizeof(PROCESSENTRY32W) == 568);
static_assert(offsetof(PROCESSENTRY32W, szExeFile) == 44);
static_assert(sizeof(TOKEN_USER) == 16);
static_assert(offsetof(TOKEN_USER, User) == 0);
static_assert(offsetof(SID_AND_ATTRIBUTES, Sid) == 0);
static_assert(std::is_same_v<decltype(&GetApplicationUserModelId), LONG (WINAPI *)(HANDLE, UINT32*, PWSTR)>);
static_assert(std::is_same_v<decltype(&GetTokenInformation), BOOL (WINAPI *)(HANDLE, TOKEN_INFORMATION_CLASS, LPVOID, DWORD, PDWORD)>);
static_assert(std::is_same_v<decltype(&OpenProcessToken), BOOL (WINAPI *)(HANDLE, DWORD, PHANDLE)>);
static_assert(std::is_same_v<decltype(&EqualSid), BOOL (WINAPI *)(PSID, PSID)>);
static_assert(std::is_same_v<decltype(&Process32FirstW), BOOL (WINAPI *)(HANDLE, LPPROCESSENTRY32W)>);
static_assert(std::is_same_v<decltype(&Process32NextW), BOOL (WINAPI *)(HANDLE, LPPROCESSENTRY32W)>);
HANDLE effective_token() { return GetCurrentThreadEffectiveToken(); }
