extern crate hyper;
extern crate futures;
extern crate tokio_core;
extern crate tokio_io;
extern crate uuid;
extern crate chrono;
extern crate tera;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate serde_json;
/*
#[macro_use]
extern crate serde_derive;
*/
extern crate ansi_term;
extern crate etag;
extern crate sequence_trie;
extern crate byteorder;

#[cfg(feature = "cervus")]
extern crate llvm_sys;

mod ice_server;
mod delegates;
mod router;
pub mod glue;
mod config;
mod static_file;
mod session_storage;
mod time;
mod template;
mod logging;
mod stat;
pub mod streaming;

#[cfg(feature = "cervus")]
mod cervus;

use std::sync::{Arc, Mutex};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::borrow::BorrowMut;
use ice_server::IceServer;
use delegates::{ServerHandle, SessionHandle, ContextHandle};

#[no_mangle]
pub fn ice_create_server() -> ServerHandle {
    Arc::into_raw(Arc::new(Mutex::new(IceServer::new())))
}

#[no_mangle]
pub unsafe fn ice_server_listen(handle: ServerHandle, addr: *const c_char) -> *mut std::thread::JoinHandle<()> {
    let handle = &*handle;

    let server = handle.lock().unwrap();
    let thread_handle = Box::new(server.listen(CStr::from_ptr(addr).to_str().unwrap()));

    Box::into_raw(thread_handle)
}

#[no_mangle]
pub unsafe fn ice_server_router_add_endpoint(handle: ServerHandle, p: *const c_char) -> *mut router::Endpoint {
    let handle = &*handle;

    let server = handle.lock().unwrap();
    let mut router = server.prep.router.lock().unwrap();
    let ep = router.add_endpoint(CStr::from_ptr(p).to_str().unwrap());

    ep
}

#[no_mangle]
pub unsafe fn ice_server_set_session_cookie_name(handle: ServerHandle, name: *const c_char) {
    let handle = &*handle;

    let mut server = handle.lock().unwrap();
    *server.prep.session_cookie_name.lock().unwrap() = CStr::from_ptr(name).to_str().unwrap().to_string();
}

#[no_mangle]
pub unsafe fn ice_server_set_session_timeout_ms(handle: ServerHandle, t: u64) {
    let handle = &*handle;

    let mut server = handle.lock().unwrap();
    *server.prep.session_timeout_ms.write().unwrap() = t;
}

#[no_mangle]
pub unsafe fn ice_server_add_template(handle: ServerHandle, name: *const c_char, content: *const c_char) -> bool {
    let handle = &*handle;

    let server = handle.lock().unwrap();
    let ret = server.prep.templates.add(
        CStr::from_ptr(name).to_str().unwrap(),
        CStr::from_ptr(content).to_str().unwrap()
    );

    ret
}

#[no_mangle]
pub unsafe fn ice_server_set_max_request_body_size(handle: ServerHandle, size: u32) {
    let handle = &*handle;

    let mut server = handle.lock().unwrap();
    *server.prep.max_request_body_size.lock().unwrap() = size;
}

#[no_mangle]
pub unsafe fn ice_server_disable_request_logging(handle: ServerHandle) {
    let handle = &*handle;

    let mut server = handle.lock().unwrap();
    *server.prep.log_requests.lock().unwrap() = false;
}

#[no_mangle]
pub unsafe fn ice_server_set_async_endpoint_cb(handle: ServerHandle, cb: extern fn (i32, *mut delegates::CallInfo)) {
    let handle = &*handle;

    let mut server = handle.lock().unwrap();
    *server.prep.async_endpoint_cb.lock().unwrap() = Some(cb);
}

#[no_mangle]
pub unsafe fn ice_server_set_endpoint_timeout_ms(handle: ServerHandle, t: u64) {
    let handle = &*handle;

    let mut server = handle.lock().unwrap();
    *server.prep.endpoint_timeout_ms.lock().unwrap() = t;
}

#[no_mangle]
pub unsafe fn ice_server_set_custom_app_data(handle: ServerHandle, ptr: *const c_void) {
    let handle = &*handle;

    let server = handle.lock().unwrap();
    server.prep.custom_app_data.set_raw(ptr);
}

#[no_mangle]
pub unsafe fn ice_context_render_template(handle: ContextHandle, name: *const c_char, data: *const c_char) -> *mut c_char {
    let handle = &*handle;

    let ret = match handle.templates.render_json(
        CStr::from_ptr(name).to_str().unwrap(),
        CStr::from_ptr(data).to_str().unwrap()
    ) {
        Some(v) => CString::new(v).unwrap().into_raw(),
        None => std::ptr::null_mut()
    };

    ret
}

#[no_mangle]
pub unsafe fn ice_context_create_session(handle: ContextHandle) -> SessionHandle {
    let handle = &*handle;

    let ret = Arc::into_raw(handle.session_storage.create_session());

    ret
}

#[no_mangle]
pub unsafe fn ice_context_get_session_by_id(handle: ContextHandle, id: *const c_char) -> SessionHandle {
    let handle = &*handle;
    let id = CStr::from_ptr(id).to_str().unwrap();

    let ret = match handle.session_storage.get_session(id) {
        Some(v) => Arc::into_raw(v),
        None => std::ptr::null()
    };

    ret
}

#[no_mangle]
pub unsafe fn ice_context_get_stats(handle: ContextHandle) -> *mut c_char {
    let handle = &*handle;

    let ret = CString::new(handle.stats.serialize().to_string()).unwrap().into_raw();

    ret
}

#[no_mangle]
pub unsafe fn ice_context_stats_set_custom(handle: ContextHandle, k: *const c_char, v: *const c_char) {
    let handle = &*handle;

    let k = CStr::from_ptr(k).to_str().unwrap().to_string();
    let v = CStr::from_ptr(v).to_str().unwrap().to_string();

    handle.stats.set_custom(k, v);
}

#[no_mangle]
pub unsafe fn ice_context_set_custom_app_data(handle: ContextHandle, ptr: *const c_void) {
    let handle = &*handle;

    handle.custom_app_data.set_raw(ptr);
}

#[no_mangle]
pub unsafe fn ice_core_destroy_context_handle(handle: ContextHandle) {
    Arc::from_raw(handle);
}

#[no_mangle]
pub unsafe fn ice_core_fire_callback(call_info: *mut delegates::CallInfo, resp: *mut glue::response::Response) -> bool {
    let call_info = Box::from_raw(call_info);
    let resp = Box::from_raw(resp);

    match call_info.tx.send(resp) {
        Ok(_) => true,
        Err(_) => false
    }
}

#[no_mangle]
pub unsafe fn ice_core_borrow_request_from_call_info(call_info: *mut delegates::CallInfo) -> *mut glue::request::Request {
    let mut call_info = &mut *call_info;

    let req = call_info.req.borrow_mut() as *mut glue::request::Request;

    req
}

#[no_mangle]
pub unsafe fn ice_core_get_custom_app_data_from_call_info(call_info: *mut delegates::CallInfo) -> *const c_void {
    let call_info = &*call_info;

    call_info.custom_app_data.get_raw()
}

#[no_mangle]
pub unsafe fn ice_core_endpoint_get_id(ep: *mut router::Endpoint) -> i32 {
    let ep = &*ep;
    ep.id
}

#[no_mangle]
pub unsafe fn ice_core_endpoint_set_flag(ep: *mut router::Endpoint, name: *const c_char, value: bool) {
    let ep = &mut *ep;
    ep.flags.insert(CStr::from_ptr(name).to_str().unwrap().to_string(), value);
}

#[no_mangle]
pub unsafe fn ice_core_destroy_cstring(v: *mut c_char) {
    CString::from_raw(v);
}

#[no_mangle]
pub unsafe fn ice_core_cervus_enabled() -> bool {
    if cfg!(feature = "cervus") {
        true
    } else {
        false
    }
}
