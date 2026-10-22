//! Windows SDK ABI used by read-only process observations.
#![allow(non_snake_case)]
use std::ffi::c_void;
pub type Handle = *mut c_void;

#[repr(C)]
#[derive(Default)]
pub struct FileTime {
    pub low: u32,
    pub high: u32,
}
#[repr(C)]
#[derive(Default)]
pub struct Luid {
    pub low: u32,
    pub high: i32,
}
#[repr(C)]
pub struct TokenPrivileges {
    pub count: u32,
    pub luid: Luid,
    pub attributes: u32,
}
#[repr(C)]
#[derive(Default)]
pub struct ModuleInfo {
    pub base: Handle,
    pub size: u32,
    pub entry: Handle,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UnicodeString {
    pub length: u16,
    pub maximum_length: u16,
    pub buffer: *const u16,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessHeader {
    pub next: u32,
    pub threads: u32,
    pub private_working_set: i64,
    pub hard_faults: u32,
    pub thread_high_water: u32,
    pub cycles: u64,
    pub created: i64,
    pub user: i64,
    pub kernel: i64,
    pub name: UnicodeString,
    pub priority: i32,
    pub pid: usize,
    pub parent: usize,
    pub handle_count: u32,
    pub session: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn GetCurrentProcess() -> Handle;
    pub fn GetCurrentThread() -> Handle;
    pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    pub fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
    pub fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    pub fn GetProcessTimes(
        process: Handle,
        created: *mut FileTime,
        exit: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    pub fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
    pub fn IsWow64Process(process: Handle, result: *mut i32) -> i32;
    pub fn DuplicateHandle(
        source: Handle,
        handle: Handle,
        target: Handle,
        output: *mut Handle,
        access: u32,
        inherit: i32,
        flags: u32,
    ) -> i32;
    pub fn CompareStringOrdinal(
        left: *const u16,
        left_length: i32,
        right: *const u16,
        right_length: i32,
        ignore_case: i32,
    ) -> i32;
    pub fn K32EnumProcesses(pids: *mut u32, bytes: u32, needed: *mut u32) -> i32;
    pub fn K32EnumProcessModulesEx(
        process: Handle,
        modules: *mut Handle,
        bytes: u32,
        needed: *mut u32,
        filter: u32,
    ) -> i32;
    pub fn K32GetModuleInformation(
        process: Handle,
        module: Handle,
        info: *mut ModuleInfo,
        size: u32,
    ) -> i32;
    pub fn K32GetModuleBaseNameW(
        process: Handle,
        module: Handle,
        text: *mut u16,
        length: u32,
    ) -> u32;
    pub fn K32GetModuleFileNameExW(
        process: Handle,
        module: Handle,
        text: *mut u16,
        length: u32,
    ) -> u32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    pub fn OpenProcessToken(process: Handle, access: u32, output: *mut Handle) -> i32;
    pub fn OpenThreadToken(
        thread: Handle,
        access: u32,
        open_as_self: i32,
        output: *mut Handle,
    ) -> i32;
    pub fn GetTokenInformation(
        token: Handle,
        class: u32,
        data: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> i32;
    pub fn LookupPrivilegeValueW(system: *const u16, name: *const u16, luid: *mut Luid) -> i32;
    pub fn AdjustTokenPrivileges(
        token: Handle,
        disable_all: i32,
        state: *const TokenPrivileges,
        length: u32,
        previous: *mut c_void,
        needed: *mut u32,
    ) -> i32;
}
#[link(name = "user32")]
unsafe extern "system" {
    pub fn EnumWindows(
        callback: unsafe extern "system" fn(Handle, isize) -> i32,
        parameter: isize,
    ) -> i32;
    pub fn GetWindowThreadProcessId(window: Handle, pid: *mut u32) -> u32;
    pub fn GetWindow(window: Handle, command: u32) -> Handle;
    pub fn IsWindowVisible(window: Handle) -> i32;
}
#[link(name = "ntdll")]
unsafe extern "system" {
    pub fn NtQuerySystemInformation(
        class: u32,
        data: *mut c_void,
        size: u32,
        needed: *mut u32,
    ) -> i32;
}
