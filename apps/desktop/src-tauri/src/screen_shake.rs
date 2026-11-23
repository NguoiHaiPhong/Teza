//! Native full-screen damped-oscillation shake effect for urgent consecutive BUZZ alerts.

use std::sync::atomic::{AtomicBool, Ordering};

static SCREEN_SHAKE_ACTIVE: AtomicBool = AtomicBool::new(false);

struct ActiveGuard;
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        SCREEN_SHAKE_ACTIVE.store(false, Ordering::SeqCst);
    }
}

/// Compute a sequence of damped `(dx, dy)` pixel offsets ending strictly at `(0, 0)`.
pub fn compute_screen_shake_offsets(frames: usize, max_amp_px: i32) -> Vec<(i32, i32)> {
    if frames == 0 {
        return Vec::new();
    }
    if frames == 1 || max_amp_px <= 0 {
        return vec![(0, 0); frames];
    }

    let mut out = Vec::with_capacity(frames);
    let denom = (frames - 1) as f64;
    let max_f = max_amp_px as f64;
    for i in 0..(frames - 1) {
        let t = i as f64 / denom;
        let decay = (1.0 - t).powf(0.85);
        let phase = i as f64;
        let raw_x = ((phase * 1.65).sin() * 0.72 + (phase * 3.1).cos() * 0.28) * max_f * decay;
        let raw_y =
            ((phase * 2.15 + 0.6).cos() * 0.65 + (phase * 3.7).sin() * 0.35) * (max_f * 0.65) * decay;
        let dx = (raw_x.round() as i32).clamp(-max_amp_px, max_amp_px);
        let dy = (raw_y.round() as i32).clamp(-max_amp_px, max_amp_px);
        out.push((dx, dy));
    }
    out.push((0, 0));
    out
}

/// Trigger a short non-blocking full-screen shake on a background thread.
pub fn trigger_screen_shake() -> Result<(), String> {
    if SCREEN_SHAKE_ACTIVE
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(());
    }

    std::thread::Builder::new()
        .name("teza-screen-shake".into())
        .spawn(|| {
            let _guard = ActiveGuard;
            #[cfg(target_os = "windows")]
            {
                let _ = win_impl::run_screen_shake();
            }
        })
        .map_err(|e| {
            SCREEN_SHAKE_ACTIVE.store(false, Ordering::SeqCst);
            format!("Không thể khởi chạy hiệu ứng rung màn hình: {e}")
        })?;

    Ok(())
}

#[cfg(target_os = "windows")]
mod win_impl {
    use super::compute_screen_shake_offsets;
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};
    use std::sync::Once;
    use std::time::{Duration, Instant};

    const SM_XVIRTUALSCREEN: i32 = 76;
    const SM_YVIRTUALSCREEN: i32 = 77;
    const SM_CXVIRTUALSCREEN: i32 = 78;
    const SM_CYVIRTUALSCREEN: i32 = 79;

    const WS_POPUP: u32 = 0x8000_0000;
    const WS_EX_TRANSPARENT: u32 = 0x0000_0020;
    const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    const WS_EX_TOPMOST: u32 = 0x0000_0008;
    const WS_EX_LAYERED: u32 = 0x0008_0000;
    const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
    const LWA_ALPHA: u32 = 0x0000_0002;

    const SW_SHOWNOACTIVATE: i32 = 4;
    const PM_REMOVE: u32 = 0x0001;
    const WM_ERASEBKGND: u32 = 0x0014;
    const WM_PAINT: u32 = 0x000F;
    const WM_NCHITTEST: u32 = 0x0084;
    const HTTRANSPARENT: isize = -1;

    const BLACKNESS: u32 = 0x0000_0042;
    const SRCCOPY: u32 = 0x00CC_0020;
    const CAPTUREBLT: u32 = 0x4000_0000;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct Msg {
        hwnd: *mut c_void,
        message: u32,
        w_param: usize,
        l_param: isize,
        time: u32,
        pt: Point,
        l_private: u32,
    }

    impl Default for Msg {
        fn default() -> Self {
            Self {
                hwnd: null_mut(),
                message: 0,
                w_param: 0,
                l_param: 0,
                time: 0,
                pt: Point::default(),
                l_private: 0,
            }
        }
    }

    #[repr(C)]
    struct PaintStruct {
        hdc: *mut c_void,
        f_erase: i32,
        rc_paint: Rect,
        f_restore: i32,
        f_inc_update: i32,
        rgb_reserved: [u8; 32],
    }

    impl Default for PaintStruct {
        fn default() -> Self {
            Self {
                hdc: null_mut(),
                f_erase: 0,
                rc_paint: Rect::default(),
                f_restore: 0,
                f_inc_update: 0,
                rgb_reserved: [0; 32],
            }
        }
    }

    type WndProc = extern "system" fn(
        hwnd: *mut c_void,
        msg: u32,
        w_param: usize,
        l_param: isize,
    ) -> isize;

    #[repr(C)]
    struct WndClassExW {
        cb_size: u32,
        style: u32,
        lpfn_wnd_proc: Option<WndProc>,
        cb_cls_extra: i32,
        cb_wnd_extra: i32,
        h_instance: *mut c_void,
        h_icon: *mut c_void,
        h_cursor: *mut c_void,
        hbr_background: *mut c_void,
        lpsz_menu_name: *const u16,
        lpsz_class_name: *const u16,
        h_icon_sm: *mut c_void,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(lp_module_name: *const u16) -> *mut c_void;
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetSystemMetrics(n_index: i32) -> i32;
        fn GetDC(hwnd: *mut c_void) -> *mut c_void;
        fn ReleaseDC(hwnd: *mut c_void, hdc: *mut c_void) -> i32;
        fn RegisterClassExW(lpwcx: *const WndClassExW) -> u16;
        fn CreateWindowExW(
            dw_ex_style: u32,
            lp_class_name: *const u16,
            lp_window_name: *const u16,
            dw_style: u32,
            x: i32,
            y: i32,
            n_width: i32,
            n_height: i32,
            hwnd_parent: *mut c_void,
            h_menu: *mut c_void,
            h_instance: *mut c_void,
            lp_param: *mut c_void,
        ) -> *mut c_void;
        fn DestroyWindow(hwnd: *mut c_void) -> i32;
        fn ShowWindow(hwnd: *mut c_void, n_cmd_show: i32) -> i32;
        fn PeekMessageW(
            lp_msg: *mut Msg,
            hwnd: *mut c_void,
            w_msg_filter_min: u32,
            w_msg_filter_max: u32,
            w_remove_msg: u32,
        ) -> i32;
        fn TranslateMessage(lp_msg: *const Msg) -> i32;
        fn DispatchMessageW(lp_msg: *const Msg) -> isize;
        fn DefWindowProcW(
            hwnd: *mut c_void,
            msg: u32,
            w_param: usize,
            l_param: isize,
        ) -> isize;
        fn BeginPaint(hwnd: *mut c_void, lp_paint: *mut PaintStruct) -> *mut c_void;
        fn EndPaint(hwnd: *mut c_void, lp_paint: *const PaintStruct) -> i32;
        fn SetLayeredWindowAttributes(
            hwnd: *mut c_void,
            cr_key: u32,
            b_alpha: u8,
            dw_flags: u32,
        ) -> i32;
    }

    #[link(name = "gdi32")]
    extern "system" {
        fn CreateCompatibleDC(hdc: *mut c_void) -> *mut c_void;
        fn DeleteDC(hdc: *mut c_void) -> i32;
        fn CreateCompatibleBitmap(hdc: *mut c_void, cx: i32, cy: i32) -> *mut c_void;
        fn SelectObject(hdc: *mut c_void, h: *mut c_void) -> *mut c_void;
        fn DeleteObject(ho: *mut c_void) -> i32;
        fn BitBlt(
            hdc: *mut c_void,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            hdc_src: *mut c_void,
            x1: i32,
            y1: i32,
            rop: u32,
        ) -> i32;
        fn PatBlt(hdc: *mut c_void, x: i32, y: i32, w: i32, h: i32, rop: u32) -> i32;
    }

    struct ScreenDcGuard(*mut c_void);
    impl Drop for ScreenDcGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    ReleaseDC(null_mut(), self.0);
                }
            }
        }
    }

    struct MemDcGuard(*mut c_void);
    impl Drop for MemDcGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    DeleteDC(self.0);
                }
            }
        }
    }

    struct BitmapGuard(*mut c_void);
    impl Drop for BitmapGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    DeleteObject(self.0);
                }
            }
        }
    }

    struct SelectGuard {
        dc: *mut c_void,
        prev: *mut c_void,
    }
    impl Drop for SelectGuard {
        fn drop(&mut self) {
            if !self.dc.is_null() && !self.prev.is_null() {
                unsafe {
                    SelectObject(self.dc, self.prev);
                }
            }
        }
    }

    struct HwndGuard(*mut c_void);
    impl Drop for HwndGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    DestroyWindow(self.0);
                }
            }
        }
    }

    extern "system" fn shake_wnd_proc(
        hwnd: *mut c_void,
        msg: u32,
        w_param: usize,
        l_param: isize,
    ) -> isize {
        match msg {
            WM_ERASEBKGND => 1,
            WM_NCHITTEST => HTTRANSPARENT,
            WM_PAINT => {
                let mut ps = PaintStruct::default();
                unsafe {
                    BeginPaint(hwnd, &mut ps);
                    EndPaint(hwnd, &ps);
                }
                0
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, w_param, l_param) },
        }
    }

    fn class_name_wide() -> Vec<u16> {
        "TezaScreenShakeOverlay\0".encode_utf16().collect()
    }

    fn ensure_class_registered(h_instance: *mut c_void) -> Vec<u16> {
        static REGISTER_ONCE: Once = Once::new();
        let class_name = class_name_wide();
        REGISTER_ONCE.call_once(|| {
            let wc = WndClassExW {
                cb_size: std::mem::size_of::<WndClassExW>() as u32,
                style: 0,
                lpfn_wnd_proc: Some(shake_wnd_proc),
                cb_cls_extra: 0,
                cb_wnd_extra: 0,
                h_instance,
                h_icon: null_mut(),
                h_cursor: null_mut(),
                hbr_background: null_mut(),
                lpsz_menu_name: null(),
                lpsz_class_name: class_name.as_ptr(),
                h_icon_sm: null_mut(),
            };
            unsafe {
                RegisterClassExW(&wc);
            }
        });
        class_name
    }

    pub(super) fn run_screen_shake() -> Result<(), String> {
        let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        if vw <= 0 || vh <= 0 {
            return Err("Không xác định được kích thước màn hình".into());
        }

        let screen_dc = ScreenDcGuard(unsafe { GetDC(null_mut()) });
        if screen_dc.0.is_null() {
            return Err("Không lấy được DC màn hình".into());
        }

        let snap_dc = MemDcGuard(unsafe { CreateCompatibleDC(screen_dc.0) });
        if snap_dc.0.is_null() {
            return Err("Không tạo được DC chụp màn hình".into());
        }
        let snap_bmp = BitmapGuard(unsafe { CreateCompatibleBitmap(screen_dc.0, vw, vh) });
        if snap_bmp.0.is_null() {
            return Err("Không tạo được bitmap chụp màn hình".into());
        }
        let _snap_sel = SelectGuard {
            dc: snap_dc.0,
            prev: unsafe { SelectObject(snap_dc.0, snap_bmp.0) },
        };

        let copied =
            unsafe { BitBlt(snap_dc.0, 0, 0, vw, vh, screen_dc.0, vx, vy, SRCCOPY | CAPTUREBLT) };
        if copied == 0 {
            return Err("BitBlt chụp toàn màn hình thất bại".into());
        }

        let back_dc = MemDcGuard(unsafe { CreateCompatibleDC(screen_dc.0) });
        if back_dc.0.is_null() {
            return Err("Không tạo được back buffer DC".into());
        }
        let back_bmp = BitmapGuard(unsafe { CreateCompatibleBitmap(screen_dc.0, vw, vh) });
        if back_bmp.0.is_null() {
            return Err("Không tạo được back buffer bitmap".into());
        }
        let _back_sel = SelectGuard {
            dc: back_dc.0,
            prev: unsafe { SelectObject(back_dc.0, back_bmp.0) },
        };

        let h_instance = unsafe { GetModuleHandleW(null()) };
        let class_name = ensure_class_registered(h_instance);
        let hwnd = HwndGuard(unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_NOACTIVATE,
                class_name.as_ptr(),
                class_name.as_ptr(),
                WS_POPUP,
                vx,
                vy,
                vw,
                vh,
                null_mut(),
                null_mut(),
                h_instance,
                null_mut(),
            )
        });
        if hwnd.0.is_null() {
            return Err("Không tạo được cửa sổ overlay rung màn hình".into());
        }

        let draw_offset = |dx: i32, dy: i32| {
            let mut msg = Msg::default();
            unsafe {
                while PeekMessageW(&mut msg, hwnd.0, 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                PatBlt(back_dc.0, 0, 0, vw, vh, BLACKNESS);
                BitBlt(back_dc.0, dx, dy, vw, vh, snap_dc.0, 0, 0, SRCCOPY);
                let wnd_dc = GetDC(hwnd.0);
                if !wnd_dc.is_null() {
                    BitBlt(wnd_dc, 0, 0, vw, vh, back_dc.0, 0, 0, SRCCOPY);
                    ReleaseDC(hwnd.0, wnd_dc);
                }
            }
        };

        unsafe {
            // Keep overlay transparent until initial frame is blitted, avoiding a black flash.
            SetLayeredWindowAttributes(hwnd.0, 0, 0, LWA_ALPHA);
            ShowWindow(hwnd.0, SW_SHOWNOACTIVATE);
        }
        draw_offset(0, 0);
        unsafe {
            SetLayeredWindowAttributes(hwnd.0, 0, 255, LWA_ALPHA);
        }

        let deadline = Instant::now() + Duration::from_millis(1200);
        let mut timed_out = false;
        for (dx, dy) in compute_screen_shake_offsets(62, 20) {
            if Instant::now() >= deadline {
                timed_out = true;
                break;
            }
            draw_offset(dx, dy);
            std::thread::sleep(Duration::from_millis(16));
        }
        if timed_out {
            draw_offset(0, 0);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::compute_screen_shake_offsets;

    #[test]
    fn compute_screen_shake_offsets_is_bounded_non_zero_early_and_ends_at_origin() {
        assert!(compute_screen_shake_offsets(0, 20).is_empty());
        assert_eq!(compute_screen_shake_offsets(1, 20), vec![(0, 0)]);
        assert_eq!(compute_screen_shake_offsets(5, 0), vec![(0, 0); 5]);

        let frames = compute_screen_shake_offsets(62, 20);
        assert_eq!(frames.len(), 62);
        assert_eq!(frames.last().copied(), Some((0, 0)));

        let max_seen_x = frames.iter().map(|(x, _)| x.abs()).max().unwrap_or(0);
        let max_seen_y = frames.iter().map(|(_, y)| y.abs()).max().unwrap_or(0);
        assert!(max_seen_x > 0 && max_seen_x <= 20);
        assert!(max_seen_y > 0 && max_seen_y <= 20);
        assert!(frames[..15].iter().any(|&(x, y)| x.abs() >= 10 || y.abs() >= 6));
    }
}
