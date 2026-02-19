//! Minimal Windows clipboard helpers.
//!
//! Current scope: CF_UNICODETEXT + CF_DIB/CF_DIBV5 + a custom applied-marker format.

#![cfg(windows)]

use anyhow::Context;
use std::ptr;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_UNICODETEXT};
use windows_sys::Win32::System::Ole::CF_HDROP;
use windows_sys::Win32::UI::Shell::DragQueryFileW;

use node::consts::APPLIED_MARKER_MIME;

fn open_clipboard_retry(max_tries: usize) -> anyhow::Result<()> {
    // OpenClipboard fails if another process currently holds it.
    for _ in 0..max_tries {
        let ok = unsafe { OpenClipboard(0 as HWND) };
        if ok != 0 {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    anyhow::bail!("OpenClipboard failed")
}

pub fn clipboard_sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

fn wide0(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

fn alloc_hglobal(bytes: &[u8]) -> anyhow::Result<windows_sys::Win32::Foundation::HGLOBAL> {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if h.is_null() {
            anyhow::bail!("GlobalAlloc failed ({} bytes)", bytes.len());
        }
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            anyhow::bail!("GlobalLock failed");
        }
        ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        GlobalUnlock(h);
        Ok(h)
    }
}

fn get_hglobal_bytes(format: u32) -> anyhow::Result<Option<Vec<u8>>> {
    let available = unsafe { IsClipboardFormatAvailable(format) };
    if available == 0 {
        return Ok(None);
    }

    open_clipboard_retry(50)?;

    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe { CloseClipboard() };
        }
    }
    let _g = Guard;

    let h = unsafe { GetClipboardData(format) };
    if h.is_null() {
        return Ok(None);
    }

    let size = unsafe { GlobalSize(h) };
    if size == 0 {
        return Ok(None);
    }
    let p = unsafe { GlobalLock(h) } as *const u8;
    if p.is_null() {
        return Ok(None);
    }
    unsafe {
        let slice = std::slice::from_raw_parts(p, size);
        let out = slice.to_vec();
        GlobalUnlock(h);
        Ok(Some(out))
    }
}

fn set_clipboard_multi(items: &[(u32, Vec<u8>)]) -> anyhow::Result<()> {
    open_clipboard_retry(80)?;

    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe { CloseClipboard() };
        }
    }
    let _g = Guard;

    unsafe {
        if EmptyClipboard() == 0 {
            anyhow::bail!("EmptyClipboard failed");
        }
    }

    for (fmt, bytes) in items.iter() {
        let h = alloc_hglobal(bytes)?;
        let rc = unsafe { SetClipboardData(*fmt, h) };
        if rc.is_null() {
            anyhow::bail!("SetClipboardData failed for fmt={}", fmt);
        }
    }

    Ok(())
}

fn register_format_cached(name: &str, slot: &'static OnceLock<u32>) -> u32 {
    *slot.get_or_init(|| {
        let w = wide0(name);
        unsafe { RegisterClipboardFormatW(w.as_ptr()) }
    })
}

static APPLIED_MARKER_FORMAT: OnceLock<u32> = OnceLock::new();
static PREFERRED_DROPEFFECT_FORMAT: OnceLock<u32> = OnceLock::new();

pub fn applied_marker_format() -> u32 {
    register_format_cached(APPLIED_MARKER_MIME, &APPLIED_MARKER_FORMAT)
}

pub fn preferred_dropeffect_format() -> u32 {
    register_format_cached("Preferred DropEffect", &PREFERRED_DROPEFFECT_FORMAT)
}

pub fn has_applied_marker() -> bool {
    let fmt = applied_marker_format();
    if fmt == 0 {
        return false;
    }
    (unsafe { IsClipboardFormatAvailable(fmt) }) != 0
}

pub fn get_dib_bytes() -> anyhow::Result<Option<Vec<u8>>> {
    // Prefer DIBV5.
    if let Some(b) = get_hglobal_bytes(CF_DIBV5.into())? {
        return Ok(Some(b));
    }
    get_hglobal_bytes(CF_DIB.into())
}

pub fn get_unicode_text() -> anyhow::Result<Option<String>> {
    // Quick pre-check.
    let available = unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.into()) };
    if available == 0 {
        return Ok(None);
    }

    open_clipboard_retry(50)?;

    // Ensure CloseClipboard is always called.
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    let _g = Guard;

    let h = unsafe { GetClipboardData(CF_UNICODETEXT.into()) };
    if h.is_null() {
        return Ok(None);
    }

    let p = unsafe { GlobalLock(h) } as *const u16;
    if p.is_null() {
        return Ok(None);
    }
    unsafe {
        // read until NUL
        let mut len = 0usize;
        loop {
            let v = ptr::read(p.add(len));
            if v == 0 {
                break;
            }
            len += 1;
            // defensive cap (avoid runaway if clipboard data is malformed)
            if len > 16 * 1024 * 1024 {
                break;
            }
        }
        let slice = std::slice::from_raw_parts(p, len);
        let s = String::from_utf16_lossy(slice);
        GlobalUnlock(h);
        Ok(Some(s))
    }
}

pub fn set_unicode_text(s: &str) -> anyhow::Result<()> {
    // UTF-16 with trailing NUL.
    let mut wide: Vec<u16> = s.encode_utf16().collect();
    wide.push(0);
    let bytes = wide.len() * 2;

    open_clipboard_retry(80)?;

    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    let _g = Guard;

    unsafe {
        if EmptyClipboard() == 0 {
            anyhow::bail!("EmptyClipboard failed");
        }

        let h = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if h.is_null() {
            anyhow::bail!("GlobalAlloc failed ({} bytes)", bytes);
        }

        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            anyhow::bail!("GlobalLock failed");
        }

        ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, p, bytes);
        GlobalUnlock(h);

        // On success, the system owns the memory; do not free.
        let rc = SetClipboardData(CF_UNICODETEXT.into(), h);
        if rc.is_null() {
            anyhow::bail!("SetClipboardData failed");
        }
    }

    Ok(())
}

pub fn set_unicode_text_best_effort(s: &str) {
    let _ = set_unicode_text(s).context("set_unicode_text");
}

pub fn set_unicode_text_with_applied_marker(s: &str, marker_payload: &[u8]) -> anyhow::Result<()> {
    let mut wide: Vec<u16> = s.encode_utf16().collect();
    wide.push(0);
    let text_bytes: Vec<u8> = wide.iter().flat_map(|u| u.to_le_bytes()).collect();
    let fmt = applied_marker_format();
    if fmt == 0 {
        // Fallback: just set text.
        return set_unicode_text(s);
    }
    set_clipboard_multi(&[(CF_UNICODETEXT.into(), text_bytes), (fmt, marker_payload.to_vec())])
}

pub fn set_dibv5_with_applied_marker(dibv5: &[u8], marker_payload: &[u8]) -> anyhow::Result<()> {
    let fmt = applied_marker_format();
    if fmt == 0 {
        return set_clipboard_multi(&[(CF_DIBV5.into(), dibv5.to_vec())]);
    }
    set_clipboard_multi(&[(CF_DIBV5.into(), dibv5.to_vec()), (fmt, marker_payload.to_vec())])
}

pub fn get_hdrop_paths() -> anyhow::Result<Option<Vec<std::path::PathBuf>>> {
    let available = unsafe { IsClipboardFormatAvailable(CF_HDROP.into()) };
    if available == 0 {
        return Ok(None);
    }

    open_clipboard_retry(50)?;
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe { CloseClipboard() };
        }
    }
    let _g = Guard;

    let h = unsafe { GetClipboardData(CF_HDROP.into()) };
    if h.is_null() {
        return Ok(None);
    }

    // CF_HDROP handle is used with DragQueryFileW.
    let count = unsafe { DragQueryFileW(h as _, 0xFFFF_FFFF, std::ptr::null_mut(), 0) };
    if count == 0 {
        return Ok(Some(Vec::new()));
    }

    let mut out: Vec<std::path::PathBuf> = Vec::with_capacity(count as usize);
    for i in 0..count {
        let len = unsafe { DragQueryFileW(h as _, i, std::ptr::null_mut(), 0) };
        if len == 0 {
            continue;
        }
        // len excludes NUL.
        let mut buf: Vec<u16> = vec![0u16; (len as usize) + 1];
        let rc = unsafe { DragQueryFileW(h as _, i, buf.as_mut_ptr(), (len + 1) as u32) };
        if rc == 0 {
            continue;
        }
        // Trim trailing NUL.
        if let Some(pos) = buf.iter().position(|&c| c == 0) {
            buf.truncate(pos);
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let os = std::ffi::OsString::from_wide(&buf);
            out.push(std::path::PathBuf::from(os));
        }
    }

    Ok(Some(out))
}

pub fn set_hdrop_paths_with_applied_marker(
    paths: &[std::path::PathBuf],
    marker_payload: &[u8],
) -> anyhow::Result<()> {
    if paths.is_empty() {
        anyhow::bail!("no paths for CF_HDROP");
    }

    // DROPFILES (20 bytes) + double-NUL terminated UTF-16 file list.
    // https://learn.microsoft.com/windows/win32/shell/clipboard
    let mut bytes: Vec<u8> = Vec::new();
    bytes.resize(20, 0);
    // pFiles offset.
    bytes[0..4].copy_from_slice(&(20u32).to_le_bytes());
    // fWide = TRUE.
    bytes[16..20].copy_from_slice(&(1u32).to_le_bytes());

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for p in paths {
            let w: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
            for u in w {
                bytes.extend_from_slice(&u.to_le_bytes());
            }
        }
        // Extra NUL terminator to end the list.
        bytes.extend_from_slice(&0u16.to_le_bytes());
    }

    let fmt_marker = applied_marker_format();
    let fmt_drop = preferred_dropeffect_format();

    let mut items: Vec<(u32, Vec<u8>)> = Vec::new();
    items.push((CF_HDROP.into(), bytes));
    if fmt_drop != 0 {
        // DROPEFFECT_COPY = 1 (DWORD).
        items.push((fmt_drop, 1u32.to_le_bytes().to_vec()));
    }
    if fmt_marker != 0 {
        items.push((fmt_marker, marker_payload.to_vec()));
    }

    set_clipboard_multi(&items)
}
