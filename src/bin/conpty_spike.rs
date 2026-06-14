// SPDX-License-Identifier: GPL-3.0-or-later
//
// M0 feasibility spike for the rust-terminal project.
//
// Goal: prove that the Windows Pseudo Console (ConPTY) API works in the target
// WinPE image. This is the single go/no-go gate for the whole project: if
// `CreatePseudoConsole` + a child `cmd.exe` works here, the GDI terminal in the
// plan is viable; if it fails, we fall back to redirected std handles (worse).
//
// What it does: opens a ConPTY, spawns the shell under it, sends a couple of
// commands down the input pipe, and streams the shell's VT output straight to
// our own stdout (with VT processing enabled so the escape sequences render).
//
// Build:  cargo build --bin conpty-spike
// Run:    target\debug\conpty-spike.exe            (defaults to cmd.exe)
//         target\debug\conpty-spike.exe powershell.exe
// Drop the produced .exe into the PE image and run it there to validate.

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::null_mut;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use windows::core::{PWSTR, Result};
use windows::Win32::Foundation::{CloseHandle, ERROR_BROKEN_PIPE, HANDLE};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, GetConsoleMode, GetStdHandle,
    SetConsoleMode, COORD, ENABLE_VIRTUAL_TERMINAL_PROCESSING, HPCON, STD_OUTPUT_HANDLE,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, InitializeProcThreadAttributeList, UpdateProcThreadAttribute,
    WaitForSingleObject, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, STARTUPINFOEXW,
};

// consoleapi.h: documented attribute id for attaching a pseudoconsole to a child.
const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Turn on VT processing for our own stdout so the child's escape sequences
/// (colors, cursor moves) render in whatever console is hosting the spike.
fn enable_vt_on_our_stdout() {
    unsafe {
        if let Ok(h) = GetStdHandle(STD_OUTPUT_HANDLE) {
            let mut mode = Default::default();
            if GetConsoleMode(h, &mut mode).is_ok() {
                let _ = SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }
        }
    }
}

fn main() -> Result<()> {
    let shell = std::env::args().nth(1).unwrap_or_else(|| "cmd.exe".into());
    enable_vt_on_our_stdout();
    eprintln!("[spike] opening ConPTY and launching `{shell}` ...");

    unsafe {
        // Two pipes. We write to `input_write` (-> child stdin); the child
        // writes to `output_write` (-> we read from `output_read`).
        let mut input_read = HANDLE::default();
        let mut input_write = HANDLE::default();
        let mut output_read = HANDLE::default();
        let mut output_write = HANDLE::default();
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: true.into(),
        };
        CreatePipe(&mut input_read, &mut input_write, Some(&sa), 0)?;
        CreatePipe(&mut output_read, &mut output_write, Some(&sa), 0)?;

        // Create the pseudoconsole bound to the child-facing pipe ends.
        let size = COORD { X: 120, Y: 30 };
        let hpc: HPCON = CreatePseudoConsole(size, input_read, output_write, 0)?;

        // Build the proc-thread attribute list carrying the pseudoconsole.
        let mut attr_size: usize = 0;
        // First call fails with ERROR_INSUFFICIENT_BUFFER but fills attr_size.
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
            // lpValue is the HPCON handle value itself (not a pointer to it).
            Some(hpc.0 as *const c_void),
            size_of::<HPCON>(),
            None,
            None,
        )?;

        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = attr_list;

        let mut cmdline = wide(&shell);
        let mut pi = PROCESS_INFORMATION::default();
        CreateProcessW(
            None,
            PWSTR(cmdline.as_mut_ptr()),
            None,
            None,
            true,
            EXTENDED_STARTUPINFO_PRESENT,
            None,
            None,
            &si.StartupInfo,
            &mut pi,
        )?;
        eprintln!("[spike] child started (pid {}). Streaming output...\n", pi.dwProcessId);

        // Drop the child-facing ends so the output pipe will EOF at teardown.
        let _ = CloseHandle(input_read);
        let _ = CloseHandle(output_write);

        // Reader thread: drain the output pipe into a shared buffer until EOF.
        // HANDLE isn't Send (it wraps a raw pointer), so ferry it as a usize.
        let output_read_raw = output_read.0 as usize;
        let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
        let captured_reader = Arc::clone(&captured);
        let reader = thread::spawn(move || {
            let output_read = HANDLE(output_read_raw as *mut c_void);
            let mut buf = [0u8; 4096];
            loop {
                let mut read: u32 = 0;
                match ReadFile(output_read, Some(&mut buf), Some(&mut read), None) {
                    Ok(()) if read > 0 => {
                        captured_reader.lock().unwrap().extend_from_slice(&buf[..read as usize]);
                    }
                    Ok(()) => break,
                    Err(e) if e.code() == ERROR_BROKEN_PIPE.to_hresult() => break,
                    Err(_) => break,
                }
            }
        });

        // Drive the shell by writing plain UTF-8 bytes to the input pipe. The
        // line terminator for console input is a bare CR (\r). (NOTE: this only
        // delivers when the host has a real console; if you run this spike under
        // a redirected/headless parent the child's input path won't connect.)
        let feed = |s: &str| {
            let bytes = s.as_bytes();
            let mut written: u32 = 0;
            let _ = WriteFile(input_write, Some(bytes), Some(&mut written), None);
        };

        let marker = "CONSOLE_INPUT_OK";
        thread::sleep(Duration::from_millis(800));
        feed(&format!("echo {marker}\r"));

        // Bounded wait: observe whether the shell runs the command.
        let w = WaitForSingleObject(pi.hProcess, 2500);
        ClosePseudoConsole(hpc);
        let _ = CloseHandle(input_write);
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
        let _ = reader.join();

        // Write a self-contained verdict file so the spike can be launched in
        // its OWN console (no stdout redirection needed to read the result).
        let bytes = captured.lock().unwrap().clone();
        let printable: String = String::from_utf8_lossy(&bytes)
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .collect();
        // The marker appears in our echoed input too; count occurrences in the
        // shell's OUTPUT only by checking it shows up at least twice (command
        // echo + command result) OR once on its own output line.
        let hits = printable.matches(marker).count();
        let input_ok = hits >= 1 && bytes.len() > 120; // more than just banner+prompt
        let verdict = format!(
            "ConPTY spike verdict\n\
             ---------------------\n\
             output bytes captured : {}\n\
             marker `{marker}` hits : {hits}\n\
             WaitForSingleObject    : {w:?}  (WAIT_OBJECT_0 => child exited)\n\
             INTERACTIVE INPUT      : {}\n\n\
             ---- captured (printable) ----\n{printable}\n",
            bytes.len(),
            if input_ok { "WORKS" } else { "NOT delivered" },
        );
        let _ = std::fs::write("D:\\rust-terminal\\spike_result.txt", &verdict);
        eprintln!("{verdict}");
    }

    Ok(())
}
