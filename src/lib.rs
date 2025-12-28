mod aasset;
mod cpp_string;
mod plthook;
mod preloader;

use std::{
    pin::Pin,
    ptr::null_mut,
    sync::{atomic::AtomicPtr, OnceLock},
};
use crate::plthook::replace_plt_functions;
use cpp_string::ResourceLocation;
use core::mem::transmute;
use cxx::CxxString;
use libc::c_void;
use plt_rs::DynamicLibrary;

fn resolve_pl_signature(signature: &str, module_name: &str) -> Option<*const u8> {
    unsafe {
        let sig_cstr = std::ffi::CString::new(signature).unwrap();
        let mod_cstr = std::ffi::CString::new(module_name).unwrap();
 
        let result = preloader::pl_resolve_signature(sig_cstr.as_ptr(), mod_cstr.as_ptr());
        if result == 0 {
            None
        } else {
            Some(result as *const u8)
        }
    }
}

#[cfg(target_arch = "aarch64")]
const RPMC_SIGNATURES: [&str; 1] = [
    "FF ?? 02 D1 FD 7B ?? A9 ?? ?? ?? ?? FA 67 ?? A9 F8 5F ?? A9 F6 57 ?? A9 F4 4F ?? A9 FD ?? 01 91 ?? D0 3B D5 ?? 03 03 2A ?? 03 02 AA ?? 17 40 F9 F3 03 00 AA A8 83 1F F8",
];

#[ctor::ctor]
fn safe_setup() {
    std::panic::set_hook(Box::new(|_| {}));
    main();
}

fn main() {
    let addr = find_signatures_using_pl_lib().expect("No signature was found");
    rpm_ctor::hook_address(addr as *mut u8);
    hook_aaset();
}

fn find_signatures_using_pl_lib() -> Option<*const u8> {
    for &signature in &RPMC_SIGNATURES {
        if let Some(addr) = resolve_pl_signature(signature, "libminecraftpe.so") {
            #[cfg(target_arch = "arm")]
            let addr = unsafe { addr.offset(1) };
            return Some(addr);
        }
    }
    None
}

macro_rules! cast_array {
    ($($func_name:literal -> $hook:expr),*,) => {
        [$(($func_name, $hook as *const u8)),*,]
    }
}

pub fn hook_aaset() {
    let lib_entry = find_lib("libminecraftpe").expect("Cannot find minecraftpe");
    let dyn_lib = DynamicLibrary::initialize(lib_entry).expect("Failed to find mc info");
    let asset_fn_list = cast_array! {
        "AAssetManager_open" -> aasset::open,
        "AAsset_read" -> aasset::read,
        "AAsset_close" -> aasset::close,
        "AAsset_seek" -> aasset::seek,
        "AAsset_seek64" -> aasset::seek64,
        "AAsset_getLength" -> aasset::len,
        "AAsset_getLength64" -> aasset::len64,
        "AAsset_getRemainingLength" -> aasset::rem,
        "AAsset_getRemainingLength64" -> aasset::rem64,
        "AAsset_openFileDescriptor" -> aasset::fd_dummy,
        "AAsset_openFileDescriptor64" -> aasset::fd_dummy64,
        "AAsset_getBuffer" -> aasset::get_buffer,
        "AAsset_isAllocated" -> aasset::is_alloc,
    };
    replace_plt_functions(&dyn_lib, asset_fn_list);
}

fn find_lib<'a>(target_name: &str) -> Option<plt_rs::LoadedLibrary<'a>> {
    let loaded_modules = plt_rs::collect_modules();
    loaded_modules
        .into_iter()
        .find(|lib| lib.name().contains(target_name))
}

pub static PACKM_OBJ: AtomicPtr<libc::c_void> = AtomicPtr::new(null_mut());
pub static RPM_LOAD: OnceLock<RpmLoadFn> = OnceLock::new();

hook_fn! {
    fn rpm_ctor(this: *mut libc::c_void,unk1: usize,unk2: usize,needs_init: bool) -> *mut libc::c_void = {
        use std::sync::atomic::Ordering;
        let result = call_original(this, unk1, unk2, needs_init);
        crate::PACKM_OBJ.store(this, Ordering::Release);
        crate::RPM_LOAD.set(crate::get_load(this)).expect("Load function is only hooked once");
        self_disable();
        result
    },
    priority = 14400
}

type RpmLoadFn = unsafe extern "C" fn(*mut c_void, ResourceLocation, Pin<&mut CxxString>) -> bool;

unsafe fn get_load(packm_ptr: *mut c_void) -> RpmLoadFn {
    let vptr = *transmute::<*mut c_void, *mut *mut *const u8>(packm_ptr);
    transmute::<*const u8, RpmLoadFn>(*vptr.offset(2))
}
