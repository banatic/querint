//! Thin wrappers over the Windows print spooler (winspool).

use std::collections::HashMap;
use std::net::Ipv4Addr;

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::Graphics::Printing::*;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
use windows::Win32::System::Time::SystemTimeToTzSpecificLocalTime;

const PRINTER_STATUS_PAUSED: u32 = 0x1;

pub const JOB_PAUSED: u32 = 0x1;
pub const JOB_ERROR: u32 = 0x2;
pub const JOB_DELETING: u32 = 0x4;
pub const JOB_PRINTING: u32 = 0x10;
pub const JOB_OFFLINE: u32 = 0x20;
pub const JOB_PAPEROUT: u32 = 0x40;
pub const JOB_PRINTED: u32 = 0x80;
pub const JOB_DELETED: u32 = 0x100;
pub const JOB_BLOCKED: u32 = 0x200;
pub const JOB_USER_INTERVENTION: u32 = 0x400;
pub const JOB_COMPLETE: u32 = 0x1000;

pub struct Printer {
    pub name: String,
    pub port: String,
    pub paused: bool,
}

#[derive(Clone)]
pub struct Job {
    pub id: u32,
    pub document: String,
    pub status: u32,
    pub pages: u32,
    /// Local time the job was submitted.
    pub submitted: SYSTEMTIME,
}

/// Win32 structs need pointer alignment, so enumeration buffers are backed by u64.
fn aligned_buf(bytes: u32) -> Vec<u64> {
    vec![0u64; (bytes as usize).div_ceil(8)]
}

fn as_bytes(buf: &mut [u64]) -> &mut [u8] {
    unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) }
}

unsafe fn read_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        String::new()
    } else {
        p.to_string().unwrap_or_default()
    }
}

pub fn list_printers() -> Vec<Printer> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let mut needed = 0u32;
    let mut count = 0u32;
    unsafe {
        let _ = EnumPrintersW(flags, PCWSTR::null(), 2, None, &mut needed, &mut count);
        if needed == 0 {
            return Vec::new();
        }
        let mut buf = aligned_buf(needed);
        if EnumPrintersW(flags, PCWSTR::null(), 2, Some(as_bytes(&mut buf)), &mut needed, &mut count).is_err() {
            return Vec::new();
        }
        let infos = std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_2W, count as usize);
        infos
            .iter()
            .map(|i| Printer {
                name: read_pwstr(i.pPrinterName),
                port: read_pwstr(i.pPortName),
                paused: i.Status & PRINTER_STATUS_PAUSED != 0,
            })
            .collect()
    }
}

struct Handle(PRINTER_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = ClosePrinter(self.0);
        }
    }
}

fn open(name: &str, admin: bool) -> Option<Handle> {
    let wide = HSTRING::from(name);
    let access = if admin { PRINTER_ACCESS_ADMINISTER | PRINTER_ACCESS_USE } else { PRINTER_ACCESS_USE };
    let defaults = PRINTER_DEFAULTSW {
        pDatatype: PWSTR::null(),
        pDevMode: std::ptr::null_mut(),
        DesiredAccess: access,
    };
    let mut handle = PRINTER_HANDLE::default();
    unsafe { OpenPrinterW(PCWSTR(wide.as_ptr()), &mut handle, Some(&defaults)).ok()? };
    Some(Handle(handle))
}

pub fn set_paused(printer: &str, paused: bool) -> bool {
    let Some(h) = open(printer, true) else { return false };
    let command = if paused { PRINTER_CONTROL_PAUSE } else { PRINTER_CONTROL_RESUME };
    unsafe { SetPrinterW(h.0, 0, None, command).is_ok() }
}

pub enum JobCommand {
    Pause,
    Resume,
    Restart,
    Delete,
}

pub fn control_job(printer: &str, job: u32, cmd: JobCommand) -> bool {
    let Some(h) = open(printer, true) else { return false };
    let command = match cmd {
        JobCommand::Pause => JOB_CONTROL_PAUSE,
        JobCommand::Resume => JOB_CONTROL_RESUME,
        JobCommand::Restart => JOB_CONTROL_RESTART,
        JobCommand::Delete => JOB_CONTROL_DELETE,
    };
    unsafe { SetJobW(h.0, job, 0, None, command).as_bool() }
}

pub fn list_jobs(printer: &str) -> Vec<Job> {
    let Some(h) = open(printer, false) else { return Vec::new() };
    let mut needed = 0u32;
    let mut count = 0u32;
    unsafe {
        let _ = EnumJobsW(h.0, 0, 512, 2, None, &mut needed, &mut count);
        if needed == 0 {
            return Vec::new();
        }
        let mut buf = aligned_buf(needed);
        if EnumJobsW(h.0, 0, 512, 2, Some(as_bytes(&mut buf)), &mut needed, &mut count).is_err() {
            return Vec::new();
        }
        let infos = std::slice::from_raw_parts(buf.as_ptr() as *const JOB_INFO_2W, count as usize);
        infos
            .iter()
            .map(|j| {
                let mut local = SYSTEMTIME::default();
                if SystemTimeToTzSpecificLocalTime(None, &j.Submitted, &mut local).is_err() {
                    local = j.Submitted;
                }
                Job {
                    id: j.JobId,
                    document: read_pwstr(j.pDocument),
                    status: j.Status,
                    pages: j.TotalPages,
                    submitted: local,
                }
            })
            .collect()
    }
}

fn reg_string(key: &HSTRING, value: &str) -> Option<String> {
    let value = HSTRING::from(value);
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if err.is_err() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..len]).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn reg_dword(key: &HSTRING, value: &str) -> Option<u32> {
    let value = HSTRING::from(value);
    let mut data = 0u32;
    let mut size = 4u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    err.is_ok().then_some(data)
}

/// Port names like "IP_10_122_1_211" or "10.122.1.203_1".
fn ip_from_port_name(port: &str) -> Option<String> {
    let s = port.strip_prefix("IP_").unwrap_or(port).replace('_', ".");
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() < 4 {
        return None;
    }
    let ip = parts[..4].join(".");
    ip.parse::<Ipv4Addr>().ok().map(|_| ip)
}

/// Host and raw port of a Standard TCP/IP port; None for virtual/USB/file ports.
pub fn port_host(port: &str, cache: &mut HashMap<String, Option<(String, u16)>>) -> Option<(String, u16)> {
    if let Some(hit) = cache.get(port) {
        return hit.clone();
    }
    let key = HSTRING::from(format!(
        "SYSTEM\\CurrentControlSet\\Control\\Print\\Monitors\\Standard TCP/IP Port\\Ports\\{port}"
    ));
    let host = reg_string(&key, "HostName")
        .or_else(|| reg_string(&key, "IPAddress"))
        .or_else(|| ip_from_port_name(port));
    let found = host.map(|h| (h, reg_dword(&key, "PortNumber").filter(|&p| p > 0).unwrap_or(9100) as u16));
    cache.insert(port.to_string(), found.clone());
    found
}
