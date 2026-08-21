use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::process::{Child, Command};
use anyhow::{Result, anyhow};
use log::debug;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    CreatePseudoConsole, ClosePseudoConsole, ResizePseudoConsole,
    GetConsoleMode, SetConsoleMode,
    GetConsoleScreenBufferInfo,
    COORD, CONSOLE_SCREEN_BUFFER_INFO,
    HPCON,
    ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    DISABLE_NEWLINE_AUTO_RETURN,
};
use windows_sys::Win32::System::Pipes::CreatePipe;

pub struct PtyProcess {
    child: Option<Child>,
    hpc: Option<HPCON>,
    stdin_handle: Option<RawHandle>,
    stdout_handle: Option<RawHandle>,
    original_in_mode: Option<u32>,
    original_out_mode: Option<u32>,
}

unsafe impl Send for PtyProcess {}

impl Default for PtyProcess {
    fn default() -> Self {
        Self::new()
    }
}

impl PtyProcess {
    pub fn new() -> Self {
        Self {
            child: None,
            hpc: None,
            stdin_handle: None,
            stdout_handle: None,
            original_in_mode: None,
            original_out_mode: None,
        }
    }

    pub fn spawn(&mut self, command: &str, args: &[String]) -> Result<()> {
        let (width, height) = get_console_size().unwrap_or((80, 24));

        // Create pipes for stdin/stdout of the pseudo console
        let mut h_stdin_read = INVALID_HANDLE_VALUE;
        let mut h_stdin_write = INVALID_HANDLE_VALUE;
        let mut h_stdout_read = INVALID_HANDLE_VALUE;
        let mut h_stdout_write = INVALID_HANDLE_VALUE;

        unsafe {
            let mut sa = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: std::ptr::null_mut(),
                bInheritHandle: 1, // TRUE — inheritable for child
            };

            if CreatePipe(&mut h_stdin_read, &mut h_stdin_write, &mut sa) == 0 {
                return Err(anyhow!("Failed to create stdin pipe"));
            }
            if CreatePipe(&mut h_stdout_read, &mut h_stdout_write, &mut sa) == 0 {
                CloseHandle(h_stdin_read);
                CloseHandle(h_stdin_write);
                return Err(anyhow!("Failed to create stdout pipe"));
            }
        }

        // Create the pseudo console
        let mut hpc: HPCON = 0;
        let coord = COORD { X: width as i16, Y: height as i16 };
        let result = unsafe {
            CreatePseudoConsole(coord, h_stdin_read, h_stdout_write, 0, &mut hpc)
        };
        if result != 0 {
            // S_OK is 0
            unsafe {
                CloseHandle(h_stdin_read);
                CloseHandle(h_stdin_write);
                CloseHandle(h_stdout_read);
                CloseHandle(h_stdout_write);
            }
            return Err(anyhow!("Failed to create pseudo console: HRESULT 0x{:08X}", result));
        }

        // Close the pipe ends that the pseudo console owns
        unsafe {
            CloseHandle(h_stdin_read);
            CloseHandle(h_stdout_write);
        }

        // Build command line
        let mut cmd_line = String::new();
        cmd_line.push_str(command);
        for arg in args {
            cmd_line.push(' ');
            cmd_line.push_str(&shell_escape_arg(arg));
        }

        // Spawn the process with the pseudo console attached
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/C", &cmd_line]);

        // We need to pass the pseudo console handle via PROC_THREAD_ATTRIBUTE_LIST
        // Use the `conpty` approach: create process with PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE
        let child = spawn_with_pty(hpc, &cmd_line)?;

        // Enable VT100 on our console handles
        let mut in_mode: u32 = 0;
        let mut out_mode: u32 = 0;
        unsafe {
            GetConsoleMode(h_stdin_write, &mut in_mode);
            GetConsoleMode(h_stdout_read, &mut out_mode);
        }
        self.original_in_mode = Some(in_mode);
        self.original_out_mode = Some(out_mode);
        unsafe {
            let _ = SetConsoleMode(h_stdin_write, in_mode | ENABLE_VIRTUAL_TERMINAL_INPUT);
            let _ = SetConsoleMode(h_stdout_read, out_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN);
        }

        self.child = Some(child);
        self.hpc = Some(hpc);
        self.stdin_handle = Some(h_stdin_write as RawHandle);
        self.stdout_handle = Some(h_stdout_read as RawHandle);

        debug!("ConPTY process spawned: {}", command);
        Ok(())
    }

    pub fn read_output(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if let Some(h) = self.stdout_handle {
            let mut n_read: u32 = 0;
            let result = unsafe {
                windows_sys::Win32::System::Pipes::ReadFile(
                    h as *mut _,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    &mut n_read,
                    std::ptr::null_mut(),
                )
            };
            if result == 0 {
                let err = io::Error::last_os_error();
                return Err(anyhow!("ReadFile failed: {}", err));
            }
            Ok(n_read as usize)
        } else {
            Err(anyhow!("ConPTY not initialized"))
        }
    }

    pub fn write_input(&mut self, data: &[u8]) -> Result<()> {
        if let Some(h) = self.stdin_handle {
            let mut n_written: u32 = 0;
            let result = unsafe {
                windows_sys::Win32::System::Pipes::WriteFile(
                    h as *mut _,
                    data.as_ptr(),
                    data.len() as u32,
                    &mut n_written,
                    std::ptr::null_mut(),
                )
            };
            if result == 0 {
                let err = io::Error::last_os_error();
                return Err(anyhow!("WriteFile failed: {}", err));
            }
            Ok(())
        } else {
            Err(anyhow!("ConPTY not initialized"))
        }
    }

    pub fn resize(&self, width: u32, height: u32) -> Result<()> {
        if let Some(hpc) = self.hpc {
            let coord = COORD { X: width as i16, Y: height as i16 };
            let result = unsafe { ResizePseudoConsole(hpc, coord) };
            if result != 0 {
                return Err(anyhow!("ResizePseudoConsole failed: HRESULT 0x{:08X}", result));
            }
            Ok(())
        } else {
            Err(anyhow!("ConPTY not initialized"))
        }
    }

    pub fn wait(&mut self) -> Result<i32> {
        if let Some(ref mut child) = self.child {
            let status = child.wait()?;
            Ok(status.code().unwrap_or(1))
        } else {
            Ok(0)
        }
    }

    pub fn kill(&mut self) -> Result<()> {
        if let Some(ref mut child) = self.child {
            child.kill()?;
        }
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    pub fn close(&mut self) {
        // Restore original console modes
        if let (Some(h), Some(mode)) = (self.stdin_handle, self.original_in_mode) {
            unsafe { let _ = SetConsoleMode(h as *mut _, mode); }
        }
        if let (Some(h), Some(mode)) = (self.stdout_handle, self.original_out_mode) {
            unsafe { let _ = SetConsoleMode(h as *mut _, mode); }
        }
        // Close handles
        if let Some(h) = self.stdin_handle.take() {
            unsafe { CloseHandle(h as *mut _); }
        }
        if let Some(h) = self.stdout_handle.take() {
            unsafe { CloseHandle(h as *mut _); }
        }
        // Close pseudo console
        if let Some(hpc) = self.hpc.take() {
            unsafe { ClosePseudoConsole(hpc); }
        }
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        self.close();
    }
}

fn get_console_size() -> Result<(u32, u32)> {
    unsafe {
        use windows_sys::Win32::System::Console::GetConsoleScreenBufferInfo;
        let handle = windows_sys::Win32::System::Console::GetStdHandle(
            windows_sys::Win32::System::Console::STD_OUTPUT_HANDLE,
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(anyhow!("GetStdHandle failed"));
        }
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
            return Err(anyhow!("GetConsoleScreenBufferInfo failed"));
        }
        let width = (info.srWindow.Right - info.srWindow.Left + 1) as u32;
        let height = (info.srWindow.Bottom - info.srWindow.Top + 1) as u32;
        Ok((width, height))
    }
}

fn shell_escape_arg(arg: &str) -> String {
    if arg.contains(' ') || arg.contains('"') || arg.contains('\\') {
        let escaped = arg.replace('"', "\"\"");
        format!("\"{}\"", escaped)
    } else {
        arg.to_string()
    }
}

/// Spawn a process with a pseudo console attached using PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE.
fn spawn_with_pty(hpc: HPCON, cmd_line: &str) -> Result<Child> {
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, InitializeProcThreadAttributeList,
        UpdateProcThreadAttribute, DeleteProcThreadAttributeList,
        PROCESS_INFORMATION, STARTUPINFOEXW, EXTENDED_STARTUPINFO_PRESENT,
        PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    };
    use windows_sys::Win32::Foundation::HANDLE;

    unsafe {
        // Calculate the size needed for the attribute list
        let mut attr_size: usize = 0;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attr_size);

        let mut attr_list_buf = vec![0u8; attr_size];
        let attr_list = attr_list_buf.as_mut_ptr() as *mut _;

        if InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size) == 0 {
            return Err(anyhow!("InitializeProcThreadAttributeList failed"));
        }

        if UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            hpc as *const _,
            std::mem::size_of::<HPCON>(),
            std::ptr::null(),
            std::ptr::null(),
        ) == 0 {
            DeleteProcThreadAttributeList(attr_list);
            return Err(anyhow!("UpdateProcThreadAttribute failed"));
        }

        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = attr_list;

        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();

        // Convert cmd_line to wide string
        let wide_cmd: Vec<u16> = cmd_line.encode_utf16().chain(std::iter::once(0)).collect();

        let result = CreateProcessW(
            std::ptr::null(),
            wide_cmd.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0, // bInheritHandles = FALSE (pipes are inherited via attribute list)
            EXTENDED_STARTUPINFO_PRESENT,
            std::ptr::null(),
            std::ptr::null(),
            &mut si.StartupInfo,
            &mut pi,
        );

        DeleteProcThreadAttributeList(attr_list);

        if result == 0 {
            return Err(anyhow!("CreateProcessW failed: {}", io::Error::last_os_error()));
        }

        // Close thread handle, keep process handle for wait
        CloseHandle(pi.hThread);

        Ok(Child::from_raw_handle(pi.hProcess as RawHandle))
    }
}

pub fn enable_virtual_terminal() -> Result<()> {
    unsafe {
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };

        // Enable VT on stdin
        let in_handle = GetStdHandle(STD_INPUT_HANDLE);
        if in_handle != INVALID_HANDLE_VALUE {
            let mut mode: u32 = 0;
            GetConsoleMode(in_handle, &mut mode);
            let _ = SetConsoleMode(in_handle, mode | ENABLE_VIRTUAL_TERMINAL_INPUT);
        }

        // Enable VT on stdout
        let out_handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if out_handle != INVALID_HANDLE_VALUE {
            let mut mode: u32 = 0;
            GetConsoleMode(out_handle, &mut mode);
            let _ = SetConsoleMode(
                out_handle,
                mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN,
            );
        }
    }
    Ok(())
}
