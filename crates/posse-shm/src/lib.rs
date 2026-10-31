//! The Windows side of MQ2Posse: the named shared memory, processes and the
//! shared clock.

use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt;
use std::ptr;
use std::sync::OnceLock;

use posse_core::{AttachError, Liveness, Owner, REGION_SIZE, Region};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, FILETIME, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT};
use windows_sys::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READWRITE, UnmapViewOfFile, VirtualQuery};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject};

/// Why a posse's shared memory can't be used.
#[derive(Debug)]
pub enum OpenError {
    /// Another client made it, with permissions this one doesn't have.
    AccessDenied,
    Windows(u32),
    Attach(AttachError),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::AccessDenied => write!(f, "Windows denied access to the shared memory. Run all clients as the same user, at the same elevation and outside any sandbox"),
            OpenError::Windows(code) => write!(f, "Windows couldn't open the shared memory (error {code})"),
            OpenError::Attach(e) => write!(f, "{e}. Unload MQ2Posse on every client to upgrade it"),
        }
    }
}

fn last_error() -> OpenError {
    match unsafe { GetLastError() } {
        ERROR_ACCESS_DENIED => OpenError::AccessDenied,
        code => OpenError::Windows(code),
    }
}

/// A posse's shared memory, mapped into this process.
pub struct Mapping {
    handle: HANDLE,
    view: *mut c_void,
    region: *const Region,
}

impl Mapping {
    /// Opens the mapping called `name`, making it if this is the first client.
    pub fn open(name: &str) -> Result<Mapping, OpenError> {
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let handle = unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, ptr::null(), PAGE_READWRITE, 0, REGION_SIZE as u32, wide.as_ptr()) };
        if handle.is_null() {
            return Err(last_error());
        }
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0) }.Value;
        if view.is_null() {
            let error = last_error();
            unsafe { CloseHandle(handle) };
            return Err(error);
        }
        // From here, dropping it unmaps the view and closes the handle.
        let mut mapping = Mapping { handle, view, region: ptr::null() };

        // The view is as big as whoever made the mapping asked for, which may
        // be a different build.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let got = unsafe { VirtualQuery(view, &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
        if got == 0 {
            return Err(last_error());
        }
        // Stays mapped until the mapping drops, which is also when the
        // reference from `region` ends.
        let region = unsafe { Region::attach(view.cast(), info.RegionSize) }.map_err(OpenError::Attach)?;
        mapping.region = region;
        Ok(mapping)
    }

    pub fn region(&self) -> &Region {
        unsafe { &*self.region }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.view });
            CloseHandle(self.handle);
        }
    }
}

/// When a process started, as a single number.
fn started(process: HANDLE) -> Option<u64> {
    let empty = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut created, mut exited, mut kernel, mut user) = (empty, empty, empty, empty);
    let ok = unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) };
    (ok != 0).then(|| u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime))
}

/// This process, as a slot's owner.
pub fn this_process() -> Option<Owner> {
    Some(Owner { pid: unsafe { GetCurrentProcessId() }, started: started(unsafe { GetCurrentProcess() })? })
}

enum Peer {
    /// Waits as signalled the moment the process ends, however it ended.
    Process(HANDLE),
    /// Windows won't let us look at it. It stays a member, and goes stale if
    /// it stops publishing.
    Unknown,
}

/// Open handles to the other clients' processes.
#[derive(Default)]
pub struct Processes {
    peers: HashMap<Owner, Peer>,
}

impl Liveness for Processes {
    #[allow(clippy::map_entry)] // returns early, without an entry, for a dead process
    fn is_alive(&mut self, owner: Owner) -> bool {
        if !self.peers.contains_key(&owner) {
            let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, owner.pid) };
            let peer = if !process.is_null() {
                // The ID may have gone to another process since the slot was claimed.
                if started(process) != Some(owner.started) {
                    unsafe { CloseHandle(process) };
                    return false;
                }
                Peer::Process(process)
            } else if unsafe { GetLastError() } == ERROR_ACCESS_DENIED {
                Peer::Unknown
            } else {
                return false;
            };
            self.peers.insert(owner, peer);
        }
        match self.peers[&owner] {
            Peer::Unknown => true,
            Peer::Process(process) => {
                let alive = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
                if !alive {
                    unsafe { CloseHandle(process) };
                    self.peers.remove(&owner);
                }
                alive
            }
        }
    }
}

impl Drop for Processes {
    fn drop(&mut self) {
        for peer in self.peers.values() {
            if let Peer::Process(process) = peer {
                unsafe { CloseHandle(*process) };
            }
        }
    }
}

/// Milliseconds on the clock every process on this PC shares.
pub fn now_ms() -> u64 {
    static FREQUENCY: OnceLock<u64> = OnceLock::new();
    let frequency = *FREQUENCY.get_or_init(|| {
        let mut frequency = 0;
        unsafe { QueryPerformanceFrequency(&mut frequency) };
        frequency.max(1) as u64
    });
    let mut counter = 0;
    unsafe { QueryPerformanceCounter(&mut counter) };
    (counter as u64 / frequency) * 1000 + (counter as u64 % frequency) * 1000 / frequency
}
