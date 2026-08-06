//! Process-loader compatibility shims for the universal Windows 7-11 desktop binary.
//!
//! Current Windows SDK import libraries can redirect `CoTaskMemFree` through `combase.dll` even
//! though the documented implementation has always been exported by `ole32.dll`. Windows 7 has no
//! `combase.dll`, so a single static reference from a dependency prevents the process from starting
//! before Rust can run. Publish the x64 import-address-table symbol locally and forward it to the
//! real `ole32.dll` export. This keeps the exact COM allocator/free pairing on every supported
//! Windows version without shipping or spoofing a system DLL.

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::core::{w, PCSTR};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

type CoTaskMemFreeFn = unsafe extern "system" fn(*const c_void);

unsafe extern "system" fn co_task_mem_free_ole32(pointer: *const c_void) {
    if pointer.is_null() {
        return;
    }

    static FUNCTION: OnceLock<CoTaskMemFreeFn> = OnceLock::new();
    let function = FUNCTION.get_or_init(|| {
        let module = GetModuleHandleW(w!("ole32.dll"))
            .expect("ole32.dll must be loaded before COM memory is released");
        let address = GetProcAddress(module, PCSTR(c"CoTaskMemFree".as_ptr().cast()))
            .expect("ole32.dll must export CoTaskMemFree on Windows 7-11");
        std::mem::transmute::<unsafe extern "system" fn() -> isize, CoTaskMemFreeFn>(address)
    });
    function(pointer);
}

// External crates such as `dirs-sys` and `rfd` contain an SDK-generated indirect call through
// `__imp_CoTaskMemFree`. Defining that pointer in the executable satisfies those references before
// the linker considers combase.lib, so combase.dll is absent from the final import table.
//
// This symbol spelling is specific to the supported x64 MSVC build. The normal desktop release is
// x64-only; PE remains a separate Windows 10/11-targeted binary and does not compile this module.
#[used]
#[export_name = "__imp_CoTaskMemFree"]
static CO_TASK_MEM_FREE_IMPORT: CoTaskMemFreeFn = co_task_mem_free_ole32;

#[cfg(test)]
mod tests {
    use super::{co_task_mem_free_ole32, CO_TASK_MEM_FREE_IMPORT};

    #[test]
    fn import_slot_targets_the_ole32_forwarder() {
        assert_eq!(
            CO_TASK_MEM_FREE_IMPORT as usize,
            co_task_mem_free_ole32 as usize
        );
    }
}
