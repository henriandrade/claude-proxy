//! The Win32 calls claude-proxy needs where Unix has signals, process groups and
//! `getppid`.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
};
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, OpenJobObjectW,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
};

/// winnt.h; windows-sys keeps it behind the SystemServices feature.
const JOB_OBJECT_TERMINATE: u32 = 8;

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and closed once.
        unsafe { CloseHandle(self.0) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(Some(0))
        .collect()
}

/// Named after the run, and gone once the drainer that holds it exits: `kill`
/// reaches the run's Claude by this name, never by a pid that may be reused.
fn job_name(run: &str) -> Vec<u16> {
    wide(&format!("Local\\claude-proxy-run-{run}"))
}

/// The Job Object holding a run's Claude and every process it starts. Closing
/// it (the drainer exiting) ends them too.
pub struct Job(Handle);

impl Job {
    pub fn for_run(run: &str, child: &std::process::Child) -> Option<Job> {
        let name = job_name(run);
        // SAFETY: plain Win32 calls on handles this function owns.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), name.as_ptr());
            if job.is_null() {
                return None;
            }
            let job = Handle(job);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const c_void,
                std::mem::size_of_val(&limits) as u32,
            );
            // ponytail: assigned just after spawn; Claude starts no tool that fast.
            // CREATE_SUSPENDED + ResumeThread closes the gap if it ever matters.
            if AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) == 0 {
                return None;
            }
            Some(Job(job))
        }
    }

    pub fn terminate(&self) {
        // SAFETY: the job handle is live while self is.
        unsafe { TerminateJobObject((self.0).0, 1) };
    }
}

/// Ends a run's Claude tree from another process. False when no drainer holds
/// the run's job.
pub fn terminate_run(run: &str) -> bool {
    let name = job_name(run);
    // SAFETY: the opened handle is closed by `Handle`.
    unsafe {
        let job = OpenJobObjectW(JOB_OBJECT_TERMINATE, 0, name.as_ptr());
        if job.is_null() {
            return false;
        }
        let job = Handle(job);
        TerminateJobObject(job.0, 1) != 0
    }
}

/// This process's parent, to notice when it exits. Windows does not re-parent
/// orphans, so the parent pid alone cannot tell.
pub struct Parent(Option<Handle>);

impl Parent {
    pub fn current() -> Parent {
        Parent(parent_pid().and_then(|pid| {
            // SAFETY: OpenProcess returns null or an owned handle.
            let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
            (!h.is_null()).then_some(Handle(h))
        }))
    }

    pub fn gone(&self) -> bool {
        // SAFETY: a zero-timeout wait on a live handle.
        self.0
            .as_ref()
            .is_some_and(|h| unsafe { WaitForSingleObject(h.0, 0) } != WAIT_TIMEOUT)
    }
}

fn parent_pid() -> Option<u32> {
    // SAFETY: the snapshot handle is closed by `Handle`; the entry is sized.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot as isize == -1 {
            return None;
        }
        let snapshot = Handle(snapshot);
        let me = GetCurrentProcessId();
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot.0, &mut entry) != 0;
        while more {
            if entry.th32ProcessID == me {
                return Some(entry.th32ParentProcessID);
            }
            more = Process32NextW(snapshot.0, &mut entry) != 0;
        }
        None
    }
}

unsafe extern "system" fn ignore(_event: u32) -> i32 {
    1
}

/// While an interactive Claude runs in this console, Ctrl-C is Claude's alone.
/// A handler, not `SetConsoleCtrlHandler(NULL)`, whose ignore flag children inherit.
pub fn leave_ctrl_c_to_child() {
    // SAFETY: registers a handler that only returns.
    unsafe { SetConsoleCtrlHandler(Some(ignore), 1) };
}

/// Whether two paths are one file (a hardlink).
pub fn same_file(a: &Path, b: &Path) -> bool {
    fn id(p: &Path) -> Option<(u32, u32, u32)> {
        let f = std::fs::File::open(p).ok()?;
        // SAFETY: the info struct is written by the call for a live handle.
        unsafe {
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            (GetFileInformationByHandle(f.as_raw_handle() as HANDLE, &mut info) != 0).then_some((
                info.dwVolumeSerialNumber,
                info.nFileIndexHigh,
                info.nFileIndexLow,
            ))
        }
    }
    matches!((id(a), id(b)), (Some(x), Some(y)) if x == y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn waiting() -> std::process::Child {
        Command::new("cmd")
            .args(["/d", "/c", "ping -n 30 127.0.0.1 >nul"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    fn a_run_job_ends_its_tree_by_name_only_while_held() {
        let mut child = waiting();
        let job = Job::for_run("test01", &child).expect("a job");
        assert!(terminate_run("test01"));
        assert!(!child.wait().unwrap().success());
        drop(job);
        assert!(!terminate_run("test01"), "the name goes with the drainer");
        assert!(!terminate_run("never1"));
    }

    #[test]
    fn closing_the_job_ends_the_tree() {
        let mut child = waiting();
        drop(Job::for_run("test02", &child).expect("a job"));
        assert!(!child.wait().unwrap().success());
    }

    #[test]
    fn this_process_has_a_live_parent_and_files_compare_by_identity() {
        assert!(!Parent::current().gone());
        let t = tempfile::tempdir().unwrap();
        let (a, b, c) = (t.path().join("a"), t.path().join("b"), t.path().join("c"));
        std::fs::write(&a, "x").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        std::fs::write(&c, "x").unwrap();
        assert!(same_file(&a, &b));
        assert!(!same_file(&a, &c));
    }
}
