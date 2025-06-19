use std::ffi::{CString, CStr};
use std::os::raw::c_char;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use async_ffi::FfiFuture;


static FAKE_SESSIONS: once_cell::sync::Lazy<Arc<Mutex<HashMap<String, String>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

pub extern "C" fn fake_get_session(
    plugin_name: *const c_char,
    key: *const c_char,
) -> FfiFuture<*mut c_char> {
    let plugin = unsafe { CStr::from_ptr(plugin_name).to_string_lossy().into_owned() };
    let key = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
    let id = format!("{plugin}:{key}");

    FfiFuture::new(async move {
        let guard = FAKE_SESSIONS.lock().unwrap();
        if let Some(sid) = guard.get(&id) {
            CString::new(sid.clone()).unwrap().into_raw()
        } else {
            std::ptr::null_mut()
        }
    })
}

pub extern "C" fn fake_get_or_create_session(
    plugin_name: *const c_char,
    key: *const c_char,
) -> FfiFuture<*mut c_char> {
    let plugin = unsafe { CStr::from_ptr(plugin_name).to_string_lossy().into_owned() };
    let key = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
    let id = format!("{plugin}:{key}");

    FfiFuture::new(async move {
        let mut guard = FAKE_SESSIONS.lock().unwrap();
        let session_id = guard.entry(id.clone()).or_insert_with(|| format!("session-{}", id));
        CString::new(session_id.clone()).unwrap().into_raw()
    })
}

pub extern "C" fn fake_invalidate_session(session_id: *const c_char) -> FfiFuture<()> {
    let id = unsafe { CStr::from_ptr(session_id).to_string_lossy().into_owned() };
    FfiFuture::new(async move {
        let mut guard = FAKE_SESSIONS.lock().unwrap();
        guard.retain(|_, v| v != &id);
    })
}
