// SPDX-License-Identifier: GPL-3.0-or-later
//! ConPTY session wrapper. Owns the pseudoconsole, the input pipe, the child
//! process, and a background reader thread that appends output bytes to a
//! shared buffer and pokes the UI thread with `WM_PTY_DATA`.
//!
//! See `src/bin/conpty_spike.rs` for the validated minimal reference and the
//! two gotchas (HPCON-as-lpValue; needs a real console for input).

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::null_mut;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use windows::core::{PWSTR, Result};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, ERROR_BROKEN_PIPE, DUPLICATE_SAME_ACCESS, HANDLE, HWND, LPARAM,
    WPARAM,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcess, InitializeProcThreadAttributeList, UpdateProcThreadAttribute,
    WaitForSingleObject, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, STARTUPINFOEXW,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Posted (no payload) to the terminal window whenever new PTY output is ready.
pub const WM_PTY_DATA: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;
/// Posted with `wParam = session id` when a session's shell process exits
/// (e.g. the user typed `exit`); the UI closes that tab.
pub const WM_PTY_EXIT: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct Pty {
    pub id: u64,
    hpc: HPCON,
    input_write: HANDLE,
    child_process: HANDLE,
    child_thread: HANDLE,
    reader: Option<JoinHandle<()>>,
    pending: Arc<Mutex<Vec<u8>>>,
}

impl Pty {
    /// Spawn `cmdline` under a fresh pseudoconsole sized `cols`x`rows`. Output
    /// is collected into a shared buffer; `hwnd` is notified via `WM_PTY_DATA`,
    /// and `WM_PTY_EXIT` (with `id`) when the child exits.
    pub fn spawn(id: u64, cmdline: &str, cols: u16, rows: u16, hwnd: HWND) -> Result<Pty> {
        unsafe {
            let sa = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: null_mut(),
                bInheritHandle: true.into(),
            };
            let mut input_read = HANDLE::default();
            let mut input_write = HANDLE::default();
            let mut output_read = HANDLE::default();
            let mut output_write = HANDLE::default();
            CreatePipe(&mut input_read, &mut input_write, Some(&sa), 0)?;
            CreatePipe(&mut output_read, &mut output_write, Some(&sa), 0)?;

            let size = COORD { X: cols as i16, Y: rows as i16 };
            let hpc = CreatePseudoConsole(size, input_read, output_write, 0)?;

            // Proc-thread attribute list carrying the pseudoconsole.
            let mut attr_size: usize = 0;
            let _ = InitializeProcThreadAttributeList(
                LPPROC_THREAD_ATTRIBUTE_LIST(null_mut()),
                1,
                0,
                &mut attr_size,
            );
            let mut attr_buf = vec![0u8; attr_size];
            let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut c_void);
            InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size)?;
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                Some(hpc.0 as *const c_void), // the HPCON value itself
                size_of::<HPCON>(),
                None,
                None,
            )?;

            let mut si = STARTUPINFOEXW::default();
            si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            si.lpAttributeList = attr_list;

            let mut cmd = wide(cmdline);
            let mut pi = PROCESS_INFORMATION::default();
            let res = CreateProcessW(
                None,
                PWSTR(cmd.as_mut_ptr()),
                None,
                None,
                true,
                EXTENDED_STARTUPINFO_PRESENT,
                None,
                None,
                &si.StartupInfo,
                &mut pi,
            );
            DeleteProcThreadAttributeList(attr_list);
            res?;

            // The pseudoconsole holds duplicates of the child-facing ends now.
            let _ = CloseHandle(input_read);
            let _ = CloseHandle(output_write);

            // Reader thread. HANDLE/HWND aren't Send; ferry as integers.
            let pending = Arc::new(Mutex::new(Vec::<u8>::new()));
            let pending_r = Arc::clone(&pending);
            let out_raw = output_read.0 as usize;
            let hwnd_raw = hwnd.0 as isize;
            let exit_id = id;
            let reader = std::thread::spawn(move || {
                let output_read = HANDLE(out_raw as *mut c_void);
                let hwnd = HWND(hwnd_raw as *mut c_void);
                let mut buf = [0u8; 8192];
                loop {
                    let mut read: u32 = 0;
                    match ReadFile(output_read, Some(&mut buf), Some(&mut read), None) {
                        Ok(()) if read > 0 => {
                            pending_r.lock().unwrap().extend_from_slice(&buf[..read as usize]);
                            let _ = PostMessageW(hwnd, WM_PTY_DATA, WPARAM(0), LPARAM(0));
                        }
                        Ok(()) => break,
                        Err(e) if e.code() == ERROR_BROKEN_PIPE.to_hresult() => break,
                        Err(_) => break,
                    }
                }
                let _ = CloseHandle(output_read);
            });
            let _ = exit_id; // reader no longer signals exit; the watcher below does

            // Watcher thread: post WM_PTY_EXIT when the shell *process* exits. We
            // can't rely on the output pipe — ConPTY keeps it open as long as we
            // hold the HPCON — so wait on a duplicate of the child handle.
            let mut watch = HANDLE::default();
            let _ = DuplicateHandle(
                GetCurrentProcess(),
                pi.hProcess,
                GetCurrentProcess(),
                &mut watch,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            );
            let watch_raw = watch.0 as isize;
            let hwnd_raw2 = hwnd.0 as isize;
            let watch_id = id;
            std::thread::spawn(move || {
                let h = HANDLE(watch_raw as *mut c_void);
                WaitForSingleObject(h, INFINITE);
                let hwnd = HWND(hwnd_raw2 as *mut c_void);
                let _ = PostMessageW(hwnd, WM_PTY_EXIT, WPARAM(watch_id as usize), LPARAM(0));
                let _ = CloseHandle(h);
            });

            Ok(Pty {
                id,
                hpc,
                input_write,
                child_process: pi.hProcess,
                child_thread: pi.hThread,
                reader: Some(reader),
                pending,
            })
        }
    }

    /// Drain and return all output bytes received since the last call.
    pub fn take_pending(&self) -> Vec<u8> {
        std::mem::take(&mut *self.pending.lock().unwrap())
    }

    /// Write raw bytes to the child's input.
    pub fn write(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        unsafe {
            let mut written: u32 = 0;
            let _ = WriteFile(self.input_write, Some(bytes), Some(&mut written), None);
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        unsafe {
            let _ = ResizePseudoConsole(self.hpc, COORD { X: cols as i16, Y: rows as i16 });
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        unsafe {
            // Closing the pseudoconsole breaks the output pipe, ending the reader.
            ClosePseudoConsole(self.hpc);
            let _ = CloseHandle(self.input_write);
            let _ = CloseHandle(self.child_thread);
            let _ = CloseHandle(self.child_process);
        }
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}
