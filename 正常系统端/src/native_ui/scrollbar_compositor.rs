//! Optional DirectComposition publisher for the advanced-page scrollbar.
//!
//! The normal Win32 fallback remains authoritative for input and accessibility. This module only
//! publishes already-rendered opaque BGRA frames through a single DirectComposition transaction.
//! All optional graphics DLLs are resolved at runtime so reduced WinPE images can safely fall back
//! without acquiring a load-time dependency on DirectComposition or D3D11.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr::{null, null_mut};

use windows::core::{Interface, HRESULT};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::{FreeLibrary, HMODULE, HWND, POINT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, ID2D1Device, ID2D1DeviceContext, ID2D1Image, D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
    D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    IDCompositionDevice, IDCompositionSurface, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGISurface};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

type D3D11CreateDeviceFn = unsafe extern "system" fn(
    *mut c_void,
    D3D_DRIVER_TYPE,
    HMODULE,
    u32,
    *const i32,
    u32,
    u32,
    *mut *mut c_void,
    *mut i32,
    *mut *mut c_void,
) -> HRESULT;

type DCompositionCreateDeviceFn =
    unsafe extern "system" fn(*mut c_void, *const windows::core::GUID, *mut *mut c_void) -> HRESULT;

type D2D1CreateDeviceFn =
    unsafe extern "system" fn(*mut c_void, *const c_void, *mut *mut c_void) -> HRESULT;

thread_local! {
    static SCROLLBAR_COMPOSITORS: RefCell<HashMap<isize, CompositorSlot>> =
        RefCell::new(HashMap::new());
}

enum CompositorSlot {
    Active(DirectCompositionScrollbar),
    Unavailable,
}

/// Publishes one complete opaque scrollbar frame.
///
/// Returns `true` only when DirectComposition accepted the frame. A `false` result tells the
/// caller to invalidate the ordinary `WM_PAINT` fallback instead.
pub(crate) unsafe fn publish(hwnd: HWND, width: i32, height: i32, dpi: u32, pixels: &[u8]) -> bool {
    if hwnd.0.is_null() || width <= 0 || height <= 0 {
        return false;
    }
    let expected = width as usize * height as usize * 4;
    if pixels.len() != expected {
        return false;
    }

    SCROLLBAR_COMPOSITORS.with(|slots| {
        let mut slots = slots.borrow_mut();
        let key = hwnd.0 as isize;
        let slot = slots.entry(key).or_insert_with(|| {
            DirectCompositionScrollbar::create(hwnd)
                .map(CompositorSlot::Active)
                .unwrap_or_else(|error| {
                    log::info!(
                        "高级选项滚动条 DirectComposition 不可用，使用 WM_PAINT 回退: {error}"
                    );
                    CompositorSlot::Unavailable
                })
        });
        let CompositorSlot::Active(compositor) = slot else {
            return false;
        };
        if let Err(error) = compositor.publish(width as u32, height as u32, dpi, pixels) {
            log::warn!("高级选项滚动条 DirectComposition 提交失败，切换到 WM_PAINT 回退: {error}");
            *slot = CompositorSlot::Unavailable;
            false
        } else {
            true
        }
    })
}

pub(crate) fn is_active(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    SCROLLBAR_COMPOSITORS.with(|slots| {
        matches!(
            slots.borrow().get(&(hwnd.0 as isize)),
            Some(CompositorSlot::Active(_))
        )
    })
}

pub(crate) fn remove(hwnd: HWND) {
    if hwnd.0.is_null() {
        return;
    }
    SCROLLBAR_COMPOSITORS.with(|slots| {
        slots.borrow_mut().remove(&(hwnd.0 as isize));
    });
}

struct SystemLibrary(HMODULE);

impl SystemLibrary {
    unsafe fn load(name: windows::core::PCWSTR) -> windows::core::Result<Self> {
        LoadLibraryExW(name, None, LOAD_LIBRARY_SEARCH_SYSTEM32).map(Self)
    }

    unsafe fn procedure<T>(&self, name: &[u8]) -> windows::core::Result<T>
    where
        T: Copy,
    {
        let Some(procedure) = GetProcAddress(self.0, windows::core::PCSTR(name.as_ptr())) else {
            return Err(windows::core::Error::from_win32());
        };
        Ok(std::mem::transmute_copy(&procedure))
    }
}

impl Drop for SystemLibrary {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.0);
        }
    }
}

struct DirectCompositionScrollbar {
    _d3d_device: ID3D11Device,
    _d2d_device: ID2D1Device,
    context: ID2D1DeviceContext,
    device: IDCompositionDevice,
    _target: IDCompositionTarget,
    visual: IDCompositionVisual,
    surface: Option<IDCompositionSurface>,
    bitmap: Option<ID2D1Bitmap>,
    size: (u32, u32),
    announced: bool,
    // Rust drops fields in declaration order, so the DLLs stay loaded until after every COM
    // interface whose vtable resides in them has been released.
    _d3d11_library: SystemLibrary,
    _d2d_library: SystemLibrary,
    _dcomp_library: SystemLibrary,
}

impl DirectCompositionScrollbar {
    unsafe fn create(hwnd: HWND) -> windows::core::Result<Self> {
        let d3d11_library = SystemLibrary::load(windows::core::w!("d3d11.dll"))?;
        let d2d_library = SystemLibrary::load(windows::core::w!("d2d1.dll"))?;
        let dcomp_library = SystemLibrary::load(windows::core::w!("dcomp.dll"))?;
        let create_d3d: D3D11CreateDeviceFn = d3d11_library.procedure(b"D3D11CreateDevice\0")?;
        let create_d2d: D2D1CreateDeviceFn = d2d_library.procedure(b"D2D1CreateDevice\0")?;
        let create_dcomp: DCompositionCreateDeviceFn =
            dcomp_library.procedure(b"DCompositionCreateDevice\0")?;

        let mut raw_d3d = null_mut();
        let mut result = create_d3d(
            null_mut(),
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT.0,
            null(),
            0,
            D3D11_SDK_VERSION,
            &mut raw_d3d,
            null_mut(),
            null_mut(),
        );
        if result.is_err() {
            result = create_d3d(
                null_mut(),
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT.0,
                null(),
                0,
                D3D11_SDK_VERSION,
                &mut raw_d3d,
                null_mut(),
                null_mut(),
            );
        }
        result.ok()?;
        let d3d_device = ID3D11Device::from_raw(raw_d3d);
        let dxgi_device: IDXGIDevice = d3d_device.cast()?;

        let mut raw_d2d = null_mut();
        create_d2d(dxgi_device.as_raw(), null(), &mut raw_d2d).ok()?;
        let d2d_device = ID2D1Device::from_raw(raw_d2d);
        let context = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;

        let mut raw_dcomp = null_mut();
        create_dcomp(
            dxgi_device.as_raw(),
            &IDCompositionDevice::IID,
            &mut raw_dcomp,
        )
        .ok()?;
        let device = IDCompositionDevice::from_raw(raw_dcomp);
        let target = device.CreateTargetForHwnd(hwnd, true)?;
        let visual = device.CreateVisual()?;
        target.SetRoot(&visual)?;
        device.Commit()?;

        Ok(Self {
            _d3d_device: d3d_device,
            _d2d_device: d2d_device,
            context,
            device,
            _target: target,
            visual,
            surface: None,
            bitmap: None,
            size: (0, 0),
            announced: false,
            _d3d11_library: d3d11_library,
            _d2d_library: d2d_library,
            _dcomp_library: dcomp_library,
        })
    }

    unsafe fn publish(
        &mut self,
        width: u32,
        height: u32,
        dpi: u32,
        pixels: &[u8],
    ) -> windows::core::Result<()> {
        if !self
            .device
            .CheckDeviceState()
            .map_err(|error| composition_error("CheckDeviceState", error))?
            .as_bool()
        {
            return Err(windows::core::Error::new(
                HRESULT(0x887A0005_u32 as i32),
                "DirectComposition device was lost",
            ));
        }
        if self.size != (width, height) || self.surface.is_none() {
            let surface = self
                .device
                .CreateSurface(
                    width,
                    height,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_ALPHA_MODE_PREMULTIPLIED,
                )
                .map_err(|error| composition_error("CreateSurface", error))?;
            self.visual
                .SetContent(&surface)
                .map_err(|error| composition_error("SetContent", error))?;
            self.surface = Some(surface);
            self.bitmap = None;
            self.size = (width, height);
        }

        let surface = self.surface.as_ref().expect("surface created above");
        let mut update_offset = POINT::default();
        let dxgi_surface: IDXGISurface = surface
            .BeginDraw(None, &mut update_offset)
            .map_err(|error| composition_error("BeginDraw", error))?;
        let target_properties = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi.max(1) as f32,
            dpiY: dpi.max(1) as f32,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: ManuallyDrop::new(None),
        };
        let target = self
            .context
            .CreateBitmapFromDxgiSurface(&dxgi_surface, Some(&target_properties))
            .map_err(|error| composition_error("CreateTargetBitmap", error))?;
        self.context.SetTarget(&target);
        self.context.BeginDraw();
        let bitmap = if let Some(bitmap) = self.bitmap.as_ref() {
            bitmap
                .CopyFromMemory(None, pixels.as_ptr().cast(), width * 4)
                .map_err(|error| composition_error("CopyFromMemory", error))?;
            bitmap.clone()
        } else {
            let properties = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: dpi.max(1) as f32,
                dpiY: dpi.max(1) as f32,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: ManuallyDrop::new(None),
            };
            let bitmap = self
                .context
                .CreateBitmap(
                    D2D_SIZE_U { width, height },
                    Some(pixels.as_ptr().cast()),
                    width * 4,
                    &properties,
                )
                .map_err(|error| composition_error("CreateBitmap", error))?
                .cast::<ID2D1Bitmap>()
                .map_err(|error| composition_error("CastBitmap", error))?;
            self.bitmap = Some(bitmap.clone());
            bitmap
        };

        self.context.SetTransform(&Matrix3x2 {
            M11: 1.0,
            M12: 0.0,
            M21: 0.0,
            M22: 1.0,
            M31: update_offset.x as f32,
            M32: update_offset.y as f32,
        });
        let destination = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: width as f32,
            bottom: height as f32,
        };
        self.context.DrawBitmap(
            &bitmap,
            Some(&destination),
            1.0,
            D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
            None,
            None,
        );
        self.context
            .EndDraw(None, None)
            .map_err(|error| composition_error("D2DEndDraw", error))?;
        self.context.SetTarget(None::<&ID2D1Image>);
        surface
            .EndDraw()
            .map_err(|error| composition_error("EndDraw", error))?;
        self.device
            .Commit()
            .map_err(|error| composition_error("Commit", error))?;
        if !self.announced {
            log::info!("高级选项滚动条 DirectComposition 已启用");
            self.announced = true;
        }
        Ok(())
    }
}

fn composition_error(stage: &str, error: windows::core::Error) -> windows::core::Error {
    windows::core::Error::new(error.code(), format!("{stage}: {error}"))
}
