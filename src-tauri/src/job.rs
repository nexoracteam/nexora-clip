//! Everything the app starts (FFmpeg, yt-dlp, the Tailscale helper) must stop when the app stops, even if the app is
//! killed or crashes. Windows does that with a job object that is told to end its processes when it is closed:
//! the app puts itself in one, and its children join it.

#[cfg(windows)]
pub fn init() {
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32);
        if set == 0 || AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            // Already inside another job that forbids this: carry on, the app still cleans up on a normal quit.
            return;
        }
        // The handle stays open for as long as the app runs; closing it (at exit) ends whatever is left.
    }
}

#[cfg(not(windows))]
pub fn init() {}
