// Shared subprocess helpers. Windows flashes a console window for every
// child process that the launcher inherits stdio from, unless we set the
// CREATE_NO_WINDOW creation flag. Every Command spawned from the host
// (git, gh, claude, codex, pnpm, the sidecar, cx detect, …) must route
// through `hide_console_window` so users don't see a black flash when BAT
// shells out.

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(windows)]
pub fn hide_console_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_console_window(_command: &mut std::process::Command) {}

/// A child process that ends together with what it started. On Windows it starts suspended in
/// a kill-on-close job object, so `kill` or drop also ends its descendants (for example the real
/// git behind the `git.exe` shim), even suspended ones; without a job only the child ends.
pub struct ProcessTree {
    child: std::process::Child,
    #[cfg(all(windows, feature = "process-tree"))]
    job: Option<job::Job>,
}

impl ProcessTree {
    /// Spawns `command` with its console window hidden; on Windows this replaces the
    /// command's creation flags.
    pub fn spawn(command: &mut std::process::Command) -> std::io::Result<Self> {
        spawn_tree(command)
    }

    pub fn child(&mut self) -> &mut std::process::Child {
        &mut self.child
    }

    pub fn kill(&mut self) {
        self.terminate_descendants();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn wait_with_output(self) -> std::io::Result<std::process::Output> {
        self.child.wait_with_output()
    }

    #[cfg(all(windows, feature = "process-tree"))]
    fn terminate_descendants(&self) {
        if let Some(job) = &self.job {
            job.terminate();
        }
    }

    #[cfg(not(all(windows, feature = "process-tree")))]
    fn terminate_descendants(&self) {}
}

#[cfg(all(windows, feature = "process-tree"))]
fn spawn_tree(command: &mut std::process::Command) -> std::io::Result<ProcessTree> {
    job::spawn_in_job(command)
}

#[cfg(not(all(windows, feature = "process-tree")))]
fn spawn_tree(command: &mut std::process::Command) -> std::io::Result<ProcessTree> {
    hide_console_window(command);
    Ok(ProcessTree {
        child: command.spawn()?,
    })
}

#[cfg(all(windows, feature = "process-tree"))]
mod job {
    use super::ProcessTree;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

    // Undocumented ntdll export, stable since Windows XP. It resumes every thread of one
    // process in a single call; finding the thread through a snapshot costs ~200 ms per spawn.
    #[link(name = "ntdll")]
    extern "system" {
        fn NtResumeProcess(process: HANDLE) -> i32;
    }

    pub(super) struct Job(HANDLE);

    // A job handle may be used and closed from any thread.
    unsafe impl Send for Job {}

    impl Job {
        fn kill_on_close() -> Option<Job> {
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return None;
            }
            let job = Job(handle);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            (configured != 0).then_some(job)
        }

        pub(super) fn terminate(&self) {
            unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    // The child starts suspended so nothing it runs, such as a shim's own child, can start
    // before the job owns it. A child that cannot be resumed is killed; if the job cannot
    // be created or cannot take the child, it runs without one and only it is killed.
    pub(super) fn spawn_in_job(command: &mut Command) -> std::io::Result<ProcessTree> {
        let Some(job) = Job::kill_on_close() else {
            command.creation_flags(CREATE_NO_WINDOW);
            return Ok(ProcessTree { child: command.spawn()?, job: None });
        };
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        let process = child.as_raw_handle() as HANDLE;
        let assigned = unsafe { AssignProcessToJobObject(job.0, process) } != 0;
        if unsafe { NtResumeProcess(process) } < 0 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("failed to resume the suspended child process"));
        }
        Ok(ProcessTree {
            child,
            job: assigned.then_some(job),
        })
    }
}

#[cfg(all(test, windows, feature = "process-tree"))]
mod tests {
    use super::ProcessTree;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    struct ProcessInfo {
        pid: u32,
        parent: u32,
        exe: String,
    }

    fn processes() -> Vec<ProcessInfo> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        assert_ne!(snapshot, INVALID_HANDLE_VALUE, "process snapshot");
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut list = Vec::new();
        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            list.push(ProcessInfo {
                pid: entry.th32ProcessID,
                parent: entry.th32ParentProcessID,
                exe: String::from_utf16_lossy(&entry.szExeFile[..len]).to_ascii_lowercase(),
            });
            more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        unsafe { CloseHandle(snapshot) };
        list
    }

    fn find_child(parent: u32, exe: &str, wait: Duration) -> Option<u32> {
        let deadline = Instant::now() + wait;
        loop {
            let found = processes().into_iter().find(|p| p.parent == parent && p.exe == exe);
            if let Some(process) = found {
                return Some(process.pid);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn ends_within(pid: u32, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if !processes().iter().any(|p| p.pid == pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn cmd_running_ping() -> Command {
        let mut command = Command::new("cmd");
        command
            .args(["/c", "ping -n 60 127.0.0.1 >nul"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    #[test]
    fn spawned_tree_runs_to_completion_with_piped_output() {
        let mut command = Command::new("cmd");
        command
            .args(["/c", "echo tree-ok"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let output = ProcessTree::spawn(&mut command)
            .expect("spawn")
            .wait_with_output()
            .expect("output");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "tree-ok");
    }

    #[test]
    fn kill_ends_grandchildren() {
        let mut tree = ProcessTree::spawn(&mut cmd_running_ping()).expect("spawn");
        let ping = find_child(tree.child().id(), "ping.exe", Duration::from_secs(20))
            .expect("cmd starts ping");
        tree.kill();
        assert!(ends_within(ping, Duration::from_secs(10)), "ping {ping} outlived kill()");
    }

    #[test]
    fn dropping_the_tree_ends_grandchildren() {
        let mut tree = ProcessTree::spawn(&mut cmd_running_ping()).expect("spawn");
        let ping = find_child(tree.child().id(), "ping.exe", Duration::from_secs(20))
            .expect("cmd starts ping");
        drop(tree);
        assert!(ends_within(ping, Duration::from_secs(10)), "ping {ping} outlived the drop");
    }

    #[test]
    fn kill_ends_the_process_behind_the_git_shim() {
        let mut command = Command::new("git");
        command
            .args(["hash-object", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let Ok(mut tree) = ProcessTree::spawn(&mut command) else {
            eprintln!("skipping: git is not installed on this machine");
            return;
        };
        // Only a `git.exe` shim has a git child; a real git on PATH has none to check.
        let Some(real_git) = find_child(tree.child().id(), "git.exe", Duration::from_secs(20)) else {
            tree.kill();
            eprintln!("skipping: git on PATH is not a shim");
            return;
        };
        tree.kill();
        assert!(ends_within(real_git, Duration::from_secs(10)), "git {real_git} outlived kill()");
    }
}
