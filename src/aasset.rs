use crate::cpp_string::{ResourceLocation, StackString};
use libc::{off64_t, off_t};
use ndk_sys::{AAsset, AAssetManager};
use std::{
    collections::HashMap,
    ffi::{CStr, CString, OsStr},
    fs,
    io::{self, Cursor, Read, Seek, Write},
    os::unix::ffi::OsStrExt,
    path::Path,
    pin::Pin,
    sync::{atomic::Ordering, Mutex, OnceLock},
};
use cxx::CxxString;

#[derive(PartialEq, Eq, Hash)]
struct AAssetPtr(*const ndk_sys::AAsset);
unsafe impl Send for AAssetPtr {}



static WANTED_ASSETS: OnceLock<Mutex<HashMap<AAssetPtr, Cursor<Vec<u8>>>>> = OnceLock::new();

fn get_wanted_assets() -> &'static Mutex<HashMap<AAssetPtr, Cursor<Vec<u8>>>> {
    WANTED_ASSETS.get_or_init(|| Mutex::new(HashMap::new()))
}



static CUSTOM_HBUI_ASSETS: OnceLock<Option<HashMap<String, Vec<u8>>>> = OnceLock::new();

fn get_custom_hbui_assets() -> &'static Option<HashMap<String, Vec<u8>>> {
    CUSTOM_HBUI_ASSETS.get_or_init(|| load_custom_hbui_folder())
}

macro_rules! folder_list {
    ($( apk: $apk_folder:literal -> pack: $pack_folder:expr),*,) => {
        [$(($apk_folder, $pack_folder)),*,]
    }
}



fn load_custom_hbui_folder() -> Option<HashMap<String, Vec<u8>>> {
    let hbui_path = Path::new("/storage/emulated/0/games/org.levimc/gui");
    
    if !hbui_path.exists() || !hbui_path.is_dir() {
        return None;
    }
    
    let mut assets = HashMap::new();
    
    if let Err(_) = load_directory_recursive(hbui_path, hbui_path, &mut assets) {
        return None;
    }
    
    Some(assets)
}

fn load_directory_recursive(
    base_path: &Path,
    current_path: &Path,
    assets: &mut HashMap<String, Vec<u8>>,
) -> io::Result<()> {
    let entries = fs::read_dir(current_path)?;
    
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        
        if path.is_file() {
            if let Ok(relative_path) = path.strip_prefix(base_path) {
                let key = relative_path.to_string_lossy().replace("\\", "/");
                match fs::read(&path) {
                    Ok(data) => {
                        assets.insert(key, data);
                    }
                    Err(_) => {}
                }
            }
        } else if path.is_dir() {
            load_directory_recursive(base_path, &path, assets)?;
        }
    }
    
    Ok(())
}

pub(crate) unsafe fn open(
    man: *mut AAssetManager,
    fname: *const libc::c_char,
    mode: libc::c_int,
) -> *mut ndk_sys::AAsset {
    let c_str = unsafe { CStr::from_ptr(fname) };
    let raw_cstr = c_str.to_bytes();
    let os_str = OsStr::from_bytes(raw_cstr);
    let c_path: &Path = Path::new(os_str);

    let stripped = match c_path.strip_prefix("assets/") {
        Ok(yay) => yay,
        Err(_) => c_path,
    };
    


    if let Some(custom_hbui) = get_custom_hbui_assets().as_ref() {
        if let Ok(hbui_file) = stripped.strip_prefix("gui") {
            let hbui_key = hbui_file.to_string_lossy().replace("\\", "/");
            if let Some(custom_data) = custom_hbui.get(&hbui_key) {
                let dummy_asset = unsafe { ndk_sys::AAssetManager_open(man, fname, mode) };
                let aasset = if dummy_asset.is_null() {
                    let dummy_path = CString::new("assets/resource_packs/vanilla/manifest.json").unwrap();
                    unsafe { ndk_sys::AAssetManager_open(man, dummy_path.as_ptr(), mode) }
                } else {
                    dummy_asset
                };
                
                if !aasset.is_null() {
                    let mut wanted_lock = get_wanted_assets().lock().unwrap();
                    wanted_lock.insert(AAssetPtr(aasset), Cursor::new(custom_data.clone()));
                    return aasset;
                }
            }
        }
    }

    let aasset = unsafe { ndk_sys::AAssetManager_open(man, fname, mode) };



    let Some(_os_filename) = c_path.file_name() else {
        return aasset;
    };

    let replacement_list = folder_list! {
        apk: "gui/dist/hbui/" -> pack: "hbui/",
    };

    for replacement in replacement_list {
        if let Ok(file) = stripped.strip_prefix(replacement.0) {
            let mut cxx_storage = StackString::new();
            let mut cxx_ptr = unsafe { cxx_storage.init("") };
            
            let loadfn = match crate::RPM_LOAD.get() {
                Some(ptr) => ptr,
                None => return aasset,
            };
            
            let mut resource_loc = ResourceLocation::new();
            let mut cpppath = ResourceLocation::get_path(&mut resource_loc);
            
            opt_path_join(cpppath.as_mut(), &[Path::new(replacement.1), file]);
            
            let packm_ptr = crate::PACKM_OBJ.load(Ordering::Acquire);
            if packm_ptr.is_null() {
                return aasset;
            }
            
            unsafe {
                loadfn(packm_ptr, resource_loc, cxx_ptr.as_mut());
            }
            
            if cxx_ptr.is_empty() {
                return aasset;
            }
            
            let buffer = cxx_ptr.as_bytes().to_vec();
            let mut wanted_lock = get_wanted_assets().lock().unwrap();
            wanted_lock.insert(AAssetPtr(aasset), Cursor::new(buffer));
            return aasset;
        }
    }
    return aasset;
}

fn opt_path_join(mut bytes: Pin<&mut CxxString>, paths: &[&Path]) {
    let total_len: usize = paths.iter().map(|p| p.as_os_str().len()).sum();
    bytes.as_mut().reserve(total_len);
    let mut writer = bytes;
    for path in paths {
        let osstr = path.as_os_str().as_bytes();
        let _ = writer.write(osstr);
    }
}

pub(crate) unsafe fn seek64(aasset: *mut AAsset, off: off64_t, whence: libc::c_int) -> off64_t {
    let mut wanted_assets = get_wanted_assets().lock().unwrap();
    let file = match wanted_assets.get_mut(&AAssetPtr(aasset)) {
        Some(file) => file,
        None => return ndk_sys::AAsset_seek64(aasset, off, whence),
    };
    seek_facade(off, whence, file) as off64_t
}

pub(crate) unsafe fn seek(aasset: *mut AAsset, off: off_t, whence: libc::c_int) -> off_t {
    let mut wanted_assets = get_wanted_assets().lock().unwrap();
    let file = match wanted_assets.get_mut(&AAssetPtr(aasset)) {
        Some(file) => file,
        None => return ndk_sys::AAsset_seek(aasset, off, whence),
    };
    seek_facade(off.into(), whence, file) as off_t
}

pub(crate) unsafe fn read(
    aasset: *mut AAsset,
    buf: *mut libc::c_void,
    count: libc::size_t,
) -> libc::c_int {
    let mut wanted_assets = get_wanted_assets().lock().unwrap();
    let file = match wanted_assets.get_mut(&AAssetPtr(aasset)) {
        Some(file) => file,
        None => return ndk_sys::AAsset_read(aasset, buf, count),
    };
    let rs_buffer = core::slice::from_raw_parts_mut(buf as *mut u8, count);
    let read_total = match file.read(rs_buffer) {
        Ok(n) => n,
        Err(_) => return -1 as libc::c_int,
    };
    read_total as libc::c_int
}

pub(crate) unsafe fn len(aasset: *mut AAsset) -> off_t {
    let wanted_assets = get_wanted_assets().lock().unwrap();
    let file = match wanted_assets.get(&AAssetPtr(aasset)) {
        Some(file) => file,
        None => return ndk_sys::AAsset_getLength(aasset),
    };
    file.get_ref().len() as off_t
}

pub(crate) unsafe fn len64(aasset: *mut AAsset) -> off64_t {
    let wanted_assets = get_wanted_assets().lock().unwrap();
    let file = match wanted_assets.get(&AAssetPtr(aasset)) {
        Some(file) => file,
        None => return ndk_sys::AAsset_getLength64(aasset),
    };
    file.get_ref().len() as off64_t
}

pub(crate) unsafe fn rem(aasset: *mut AAsset) -> off_t {
    let wanted_assets = get_wanted_assets().lock().unwrap();
    let file = match wanted_assets.get(&AAssetPtr(aasset)) {
        Some(file) => file,
        None => return ndk_sys::AAsset_getRemainingLength(aasset),
    };
    (file.get_ref().len() - file.position() as usize) as off_t
}

pub(crate) unsafe fn rem64(aasset: *mut AAsset) -> off64_t {
    let wanted_assets = get_wanted_assets().lock().unwrap();
    let file = match wanted_assets.get(&AAssetPtr(aasset)) {
        Some(file) => file,
        None => return ndk_sys::AAsset_getRemainingLength64(aasset),
    };
    (file.get_ref().len() - file.position() as usize) as off64_t
}

pub(crate) unsafe fn close(aasset: *mut AAsset) {
    let mut wanted_assets = get_wanted_assets().lock().unwrap();
    if wanted_assets.remove(&AAssetPtr(aasset)).is_none() {
        ndk_sys::AAsset_close(aasset);
    }
}

pub(crate) unsafe fn get_buffer(aasset: *mut AAsset) -> *const libc::c_void {
    let mut wanted_assets = get_wanted_assets().lock().unwrap();
    let file = match wanted_assets.get_mut(&AAssetPtr(aasset)) {
        Some(file) => file,
        None => return ndk_sys::AAsset_getBuffer(aasset),
    };
    file.get_mut().as_mut_ptr().cast()
}

pub(crate) unsafe fn fd_dummy(
    aasset: *mut AAsset,
    out_start: *mut off_t,
    out_len: *mut off_t,
) -> libc::c_int {
    let wanted_assets = get_wanted_assets().lock().unwrap();
    match wanted_assets.get(&AAssetPtr(aasset)) {
        Some(_) => -1,
        None => ndk_sys::AAsset_openFileDescriptor(aasset, out_start, out_len),
    }
}

pub(crate) unsafe fn fd_dummy64(
    aasset: *mut AAsset,
    out_start: *mut off64_t,
    out_len: *mut off64_t,
) -> libc::c_int {
    let wanted_assets = get_wanted_assets().lock().unwrap();
    match wanted_assets.get(&AAssetPtr(aasset)) {
        Some(_) => -1,
        None => ndk_sys::AAsset_openFileDescriptor64(aasset, out_start, out_len),
    }
}

pub(crate) unsafe fn is_alloc(aasset: *mut AAsset) -> libc::c_int {
    let wanted_assets = get_wanted_assets().lock().unwrap();
    match wanted_assets.get(&AAssetPtr(aasset)) {
        Some(_) => false as libc::c_int,
        None => ndk_sys::AAsset_isAllocated(aasset),
    }
}

fn seek_facade(offset: i64, whence: libc::c_int, file: &mut Cursor<Vec<u8>>) -> i64 {
    let offset = match whence {
        libc::SEEK_SET => {
            let u64_off = match u64::try_from(offset) {
                Ok(uoff) => uoff,
                Err(_) => return -1,
            };
            io::SeekFrom::Start(u64_off)
        }
        libc::SEEK_CUR => io::SeekFrom::Current(offset),
        libc::SEEK_END => io::SeekFrom::End(offset),
        _ => return -1,
    };
    match file.seek(offset) {
        Ok(new_offset) => match new_offset.try_into() {
            Ok(int) => int,
            Err(_) => -1,
        },
        Err(_) => -1,
    }
}
