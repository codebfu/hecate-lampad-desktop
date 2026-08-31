//! Copyright (C) 2026 Gaultier HUBERT
//! SPDX-License-Identifier: GPL-3.0-or-later

//! Windows screen capture: DXGI Desktop Duplication first, then GDI, then PrintWindow.

use super::BackendError;
use std::mem::size_of;
use windows::core::Interface;
use windows::Win32::Foundation::{BOOL, GetLastError, HANDLE, HWND};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetDeviceCaps,
    GetWindowDC, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
    DIB_RGB_COLORS, HBITMAP, HDC, HORZRES, ROP_CODE, SRCCOPY, VERTRES,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetThreadDesktop, OpenInputDesktop, SetThreadDesktop, DESKTOP_ACCESS_FLAGS,
    DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, DESKTOP_WRITEOBJECTS, HDESK,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    GetDesktopWindow, GetShellWindow, GetSystemMetrics, PW_RENDERFULLCONTENT, SM_CXSCREEN,
    SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

#[link(name = "user32")]
unsafe extern "system" {
    fn PrintWindow(hwnd: HWND, hdc_blt: HDC, n_flags: u32) -> BOOL;
}

/// Capture the interactive desktop as RGBA8.
pub fn capture_rgba() -> Result<(u32, u32, Vec<u8>), BackendError> {
    let _desktop_guard = InputDesktopGuard::attach();

    let mut errors = Vec::new();
    // DXGI Desktop Duplication can "succeed" with a solid-black frame on some
    // Hyper-V / WDDM setups while GDI BitBlt still sees the real desktop.
    // Treat blank frames as failure so we fall through.
    for (name, capture) in [
        ("dxgi", capture_dxgi as fn() -> Result<(u32, u32, Vec<u8>), BackendError>),
        ("gdi", capture_gdi_screen),
        ("gdi-desktop", capture_gdi_desktop_window),
        ("printwindow", capture_printwindow_shell),
    ] {
        match capture() {
            Ok(frame) if !frame_looks_blank(&frame.2) => return Ok(frame),
            Ok(_) => errors.push(format!("{name}: blank frame")),
            Err(error) => errors.push(format!("{name}: {error}")),
        }
    }
    Err(BackendError::Other(format!(
        "screen capture failed ({})",
        errors.join("; ")
    )))
}

/// True when sampled pixels are almost entirely near-black.
///
/// Used to reject DXGI frames that succeed API-wise but contain no desktop
/// content (common on headless / virtual GPU paths).
fn frame_looks_blank(rgba: &[u8]) -> bool {
    let pixels = rgba.len() / 4;
    if pixels == 0 {
        return true;
    }
    // Evenly sample up to 4096 pixels across the buffer.
    let step = (pixels / 4096).max(1);
    let mut lit = 0usize;
    let mut checked = 0usize;
    let mut i = 0usize;
    while i < pixels {
        let o = i * 4;
        let r = rgba[o];
        let g = rgba[o + 1];
        let b = rgba[o + 2];
        if r > 8 || g > 8 || b > 8 {
            lit += 1;
        }
        checked += 1;
        i = i.saturating_add(step);
    }
    // Blank if fewer than 0.5% of samples have any visible luminance.
    lit.saturating_mul(200) < checked
}

struct InputDesktopGuard {
    previous: Option<HDESK>,
    attached: Option<HDESK>,
}

impl InputDesktopGuard {
    fn attach() -> Self {
        unsafe {
            let previous = GetThreadDesktop(GetCurrentThreadId()).ok();
            let access = DESKTOP_ACCESS_FLAGS(DESKTOP_READOBJECTS.0 | DESKTOP_WRITEOBJECTS.0);
            let attached = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, access).ok();
            if let Some(desk) = attached {
                if SetThreadDesktop(desk).is_ok() {
                    return Self {
                        previous,
                        attached: Some(desk),
                    };
                }
                let _ = CloseDesktop(desk);
            }
            Self {
                previous: None,
                attached: None,
            }
        }
    }
}

impl Drop for InputDesktopGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = self.previous.take() {
                let _ = SetThreadDesktop(previous);
            }
            if let Some(attached) = self.attached.take() {
                let _ = CloseDesktop(attached);
            }
        }
    }
}

fn capture_dxgi() -> Result<(u32, u32, Vec<u8>), BackendError> {
    unsafe {
        let (device, context) = create_d3d_device()?;
        let dxgi_device: IDXGIDevice = device
            .cast()
            .map_err(|e| BackendError::Other(format!("IDXGIDevice cast failed: {e}")))?;
        let adapter = dxgi_device
            .GetAdapter()
            .map_err(|e| BackendError::Other(format!("GetAdapter failed: {e}")))?;
        let output = adapter
            .EnumOutputs(0)
            .map_err(|e| BackendError::Other(format!("EnumOutputs failed: {e}")))?;
        let output1: IDXGIOutput1 = output
            .cast()
            .map_err(|e| BackendError::Other(format!("IDXGIOutput1 cast failed: {e}")))?;
        let duplication: IDXGIOutputDuplication = output1
            .DuplicateOutput(&device)
            .map_err(|e| BackendError::Other(format!("DuplicateOutput failed: {e}")))?;

        let desc = duplication.GetDesc();
        let width = desc.ModeDesc.Width;
        let height = desc.ModeDesc.Height;
        if width == 0 || height == 0 {
            return Err(BackendError::Other("DXGI output has zero size".into()));
        }

        let staging = create_staging_texture(&device, width, height)?;
        let texture = acquire_frame(&duplication)?;
        context.CopyResource(&staging, &texture);
        let _ = duplication.ReleaseFrame();

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|e| BackendError::Other(format!("Map staging texture failed: {e}")))?;

        let pitch = mapped.RowPitch as usize;
        let src = mapped.pData as *const u8;
        let mut bgra = vec![0u8; (width as usize) * (height as usize) * 4];
        for y in 0..(height as usize) {
            let row = std::slice::from_raw_parts(src.add(y * pitch), (width as usize) * 4);
            let dest_start = y * (width as usize) * 4;
            bgra[dest_start..dest_start + row.len()].copy_from_slice(row);
        }
        context.Unmap(&staging, 0);

        Ok((width, height, bgra_to_rgba(&bgra)))
    }
}

unsafe fn create_d3d_device() -> Result<(ID3D11Device, ID3D11DeviceContext), BackendError> {
    let mut device = None;
    let mut context = None;
    let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
    let hr = D3D11CreateDevice(
        None,
        D3D_DRIVER_TYPE_HARDWARE,
        None,
        flags,
        None,
        D3D11_SDK_VERSION,
        Some(&mut device),
        None,
        Some(&mut context),
    );
    if hr.is_err() {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_WARP,
            None,
            flags,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(|e| BackendError::Other(format!("D3D11CreateDevice failed: {e}")))?;
    }
    let device = device.ok_or_else(|| BackendError::Other("D3D11 device missing".into()))?;
    let context = context.ok_or_else(|| BackendError::Other("D3D11 context missing".into()))?;
    Ok((device, context))
}

unsafe fn create_staging_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<ID3D11Texture2D, BackendError> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut texture = None;
    device
        .CreateTexture2D(&desc, None, Some(&mut texture))
        .map_err(|e| BackendError::Other(format!("CreateTexture2D failed: {e}")))?;
    texture.ok_or_else(|| BackendError::Other("staging texture missing".into()))
}

unsafe fn acquire_frame(
    duplication: &IDXGIOutputDuplication,
) -> Result<ID3D11Texture2D, BackendError> {
    // Desktop Duplication may need a few tries before the first useful frame.
    // Frames with AccumulatedFrames == 0 are often empty/black right after
    // DuplicateOutput — keep waiting for a real present.
    let mut last_error = BackendError::Other("AcquireNextFrame produced no frame".into());
    for _ in 0..40 {
        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match duplication.AcquireNextFrame(100, &mut frame_info, &mut resource) {
            Ok(()) => {
                if frame_info.AccumulatedFrames == 0 && frame_info.LastPresentTime == 0 {
                    let _ = duplication.ReleaseFrame();
                    last_error = BackendError::Other("DXGI empty frame".into());
                    continue;
                }
                let resource = resource
                    .ok_or_else(|| BackendError::Other("AcquireNextFrame resource missing".into()))?;
                let texture: ID3D11Texture2D = resource.cast().map_err(|e| {
                    BackendError::Other(format!("desktop frame texture cast failed: {e}"))
                })?;
                return Ok(texture);
            }
            Err(error) => {
                let code = error.code();
                if code == DXGI_ERROR_WAIT_TIMEOUT {
                    last_error = BackendError::Other("DXGI frame timeout".into());
                    continue;
                }
                if code == DXGI_ERROR_ACCESS_LOST {
                    return Err(BackendError::Other(
                        "DXGI access lost (display mode changed)".into(),
                    ));
                }
                return Err(BackendError::Other(format!(
                    "AcquireNextFrame failed: {error}"
                )));
            }
        }
    }
    Err(last_error)
}

fn capture_gdi_screen() -> Result<(u32, u32, Vec<u8>), BackendError> {
    unsafe {
        let hdc = GetDC(HWND::default());
        if hdc.is_invalid() {
            return Err(BackendError::Other(win32_error("GetDC failed")));
        }
        let bounds = screen_capture_bounds(hdc);
        let result = blit_screen_to_rgba(hdc, bounds);
        ReleaseDC(HWND::default(), hdc);
        result
    }
}

fn capture_gdi_desktop_window() -> Result<(u32, u32, Vec<u8>), BackendError> {
    unsafe {
        let hwnd = GetDesktopWindow();
        let hdc = GetWindowDC(hwnd);
        if hdc.is_invalid() {
            return Err(BackendError::Other(win32_error("GetWindowDC failed")));
        }
        let width = GetDeviceCaps(hdc, HORZRES);
        let height = GetDeviceCaps(hdc, VERTRES);
        let bounds = if width > 0 && height > 0 {
            (0, 0, width, height)
        } else {
            (
                0,
                0,
                GetSystemMetrics(SM_CXSCREEN),
                GetSystemMetrics(SM_CYSCREEN),
            )
        };
        let result = blit_screen_to_rgba(hdc, bounds);
        ReleaseDC(hwnd, hdc);
        result
    }
}

fn capture_printwindow_shell() -> Result<(u32, u32, Vec<u8>), BackendError> {
    unsafe {
        let shell = GetShellWindow();
        if shell.0.is_null() {
            return Err(BackendError::Other("GetShellWindow returned null".into()));
        }
        let width = GetSystemMetrics(SM_CXSCREEN);
        let height = GetSystemMetrics(SM_CYSCREEN);
        if width <= 0 || height <= 0 {
            return Err(BackendError::NoSession("no screen metrics".into()));
        }

        let hdc_screen = GetDC(HWND::default());
        if hdc_screen.is_invalid() {
            return Err(BackendError::Other(win32_error("GetDC failed")));
        }
        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem.is_invalid() {
            ReleaseDC(HWND::default(), hdc_screen);
            return Err(BackendError::Other(win32_error("CreateCompatibleDC failed")));
        }

        let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
        let hbmp = match create_dib(hdc_mem, width, height, &mut bits) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = DeleteDC(hdc_mem);
                ReleaseDC(HWND::default(), hdc_screen);
                return Err(error);
            }
        };
        let old = SelectObject(hdc_mem, hbmp);
        let ok = PrintWindow(shell, hdc_mem, PW_RENDERFULLCONTENT);
        if !ok.as_bool() {
            SelectObject(hdc_mem, old);
            let _ = DeleteObject(hbmp);
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(HWND::default(), hdc_screen);
            return Err(BackendError::Other(win32_error("PrintWindow(shell) failed")));
        }

        let byte_len = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        let bgra = std::slice::from_raw_parts(bits as *const u8, byte_len).to_vec();
        SelectObject(hdc_mem, old);
        let _ = DeleteObject(hbmp);
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(HWND::default(), hdc_screen);
        Ok((width as u32, height as u32, bgra_to_rgba(&bgra)))
    }
}

unsafe fn screen_capture_bounds(hdc_screen: HDC) -> (i32, i32, i32, i32) {
    let mut origin_x = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let mut origin_y = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let mut width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
    let mut height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
    if width <= 0 || height <= 0 {
        origin_x = 0;
        origin_y = 0;
        width = GetDeviceCaps(hdc_screen, HORZRES);
        height = GetDeviceCaps(hdc_screen, VERTRES);
    }
    if width <= 0 || height <= 0 {
        origin_x = 0;
        origin_y = 0;
        width = GetSystemMetrics(SM_CXSCREEN);
        height = GetSystemMetrics(SM_CYSCREEN);
    }
    (origin_x, origin_y, width, height)
}

unsafe fn blit_screen_to_rgba(
    hdc_screen: HDC,
    (origin_x, origin_y, width, height): (i32, i32, i32, i32),
) -> Result<(u32, u32, Vec<u8>), BackendError> {
    if width <= 0 || height <= 0 {
        return Err(BackendError::NoSession("no screen metrics".into()));
    }

    let hdc_mem = CreateCompatibleDC(hdc_screen);
    if hdc_mem.is_invalid() {
        return Err(BackendError::Other(win32_error("CreateCompatibleDC failed")));
    }

    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    let hbmp = match create_dib(hdc_mem, width, height, &mut bits) {
        Ok(handle) => handle,
        Err(error) => {
            let _ = DeleteDC(hdc_mem);
            return Err(error);
        }
    };

    let old = SelectObject(hdc_mem, hbmp);
    let capture_rop = ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0);
    let blit = BitBlt(
        hdc_mem,
        0,
        0,
        width,
        height,
        hdc_screen,
        origin_x,
        origin_y,
        capture_rop,
    )
    .or_else(|_| {
        BitBlt(
            hdc_mem,
            0,
            0,
            width,
            height,
            hdc_screen,
            origin_x,
            origin_y,
            SRCCOPY,
        )
    });

    if let Err(error) = blit {
        SelectObject(hdc_mem, old);
        let _ = DeleteObject(hbmp);
        let _ = DeleteDC(hdc_mem);
        return Err(BackendError::Other(format!(
            "BitBlt failed: {error}; {}",
            win32_error("screen capture")
        )));
    }

    let byte_len = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    let bgra = std::slice::from_raw_parts(bits as *const u8, byte_len).to_vec();
    SelectObject(hdc_mem, old);
    let _ = DeleteObject(hbmp);
    let _ = DeleteDC(hdc_mem);
    Ok((width as u32, height as u32, bgra_to_rgba(&bgra)))
}

unsafe fn create_dib(
    hdc_mem: HDC,
    width: i32,
    height: i32,
    bits: &mut *mut core::ffi::c_void,
) -> Result<HBITMAP, BackendError> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0 as u32,
            biSizeImage: (width as u32).saturating_mul(height as u32).saturating_mul(4),
            ..Default::default()
        },
        ..Default::default()
    };
    match CreateDIBSection(hdc_mem, &info, DIB_RGB_COLORS, bits, HANDLE::default(), 0) {
        Ok(handle) if !handle.is_invalid() && !bits.is_null() => Ok(handle),
        Ok(_) => Err(BackendError::Other(win32_error(
            "CreateDIBSection returned null",
        ))),
        Err(error) => Err(BackendError::Other(format!(
            "CreateDIBSection failed: {error}"
        ))),
    }
}

fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for chunk in bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[chunk[2], chunk[1], chunk[0], 255]);
    }
    rgba
}

fn win32_error(prefix: &str) -> String {
    let code = unsafe { GetLastError() };
    format!("{prefix} (win32={})", code.0)
}

#[cfg(test)]
mod tests {
    use super::frame_looks_blank;

    #[test]
    fn blank_frame_detects_solid_black() {
        let rgba = vec![0u8; 64 * 64 * 4];
        assert!(frame_looks_blank(&rgba));
    }

    #[test]
    fn blank_frame_rejects_visible_content() {
        let mut rgba = vec![0u8; 64 * 64 * 4];
        // Paint a bright region so sampling cannot miss it.
        for i in 0..(64 * 64) {
            let o = i * 4;
            rgba[o] = 40;
            rgba[o + 1] = 40;
            rgba[o + 2] = 40;
            rgba[o + 3] = 255;
        }
        assert!(!frame_looks_blank(&rgba));
    }

    #[test]
    fn blank_frame_empty_is_blank() {
        assert!(frame_looks_blank(&[]));
    }
}
