//! Ending a worker's whole process tree. A stop must end what the vendor's
//! CLI started too, not only the CLI.

use tokio::process::{Child, Command};

/// Puts the worker in a group of its own, so the stop has something to aim at.
#[cfg(unix)]
pub(super) fn configure(cmd: &mut Command) {
    cmd.process_group(0);
}

#[cfg(windows)]
pub(super) fn configure(cmd: &mut Command) {
    use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

/// What reaches every process a worker started: the group's pid on Unix, a
/// Job Object on Windows. The job kills its members when the last handle
/// closes, so on Windows the tree also ends if the supervisor itself dies.
pub(super) struct Tree(Option<Handle>);

#[cfg(unix)]
type Handle = i32;
#[cfg(windows)]
type Handle = Job;

#[cfg(windows)]
struct Job(windows_sys::Win32::Foundation::HANDLE);

// A job handle is a kernel object reference, usable from any thread.
#[cfg(windows)]
unsafe impl Send for Job {}
#[cfg(windows)]
unsafe impl Sync for Job {}

impl Tree {
    #[cfg(unix)]
    pub(super) fn attach(child: &Child) -> Self {
        Self(child.id().and_then(|p| i32::try_from(p).ok()))
    }

    #[cfg(windows)]
    pub(super) fn attach(child: &Child) -> Self {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        let Some(process) = child.raw_handle() else {
            return Self(None);
        };
        // SAFETY: plain Win32 calls on a job this function creates and a
        // process handle the child owns; the job is closed on every failure.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Self(None);
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set == 0 || AssignProcessToJobObject(job, process.cast()) == 0 {
                CloseHandle(job);
                return Self(None);
            }
            Self(Some(Job(job)))
        }
    }

    /// Ends every process in the tree, grandchildren included.
    pub(super) fn kill(self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // SAFETY: kill(2) on a group this process made; it touches no memory.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        // On Windows, dropping terminates the job and closes it.
        #[cfg(windows)]
        drop(self);
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        // SAFETY: the handle is ours and is closed exactly once.
        unsafe {
            TerminateJobObject(self.0, 1);
            CloseHandle(self.0);
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_stop_ends_what_the_worker_started() {
        // cmd starts a ping that outlives a plain kill of cmd.
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "ping -n 60 127.0.0.1 > NUL"])
            .kill_on_drop(true);
        configure(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let tree = Tree::attach(&child);
        assert!(tree.0.is_some());
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        tree.kill();
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
            .await
            .expect("the worker ended")
            .unwrap();
        assert!(!status.success());
    }
}
