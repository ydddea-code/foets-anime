//! Windows libmpv embedding via the MPV C render API on a native child HWND.
//!
//! Flow:
//! 1. `libmpv-2.dll` is loaded at runtime (next to fets-anime.exe, in .\mpv\,
//!    or on PATH). Entry points resolve through GetProcAddress so a missing
//!    DLL yields a clean error message instead of a crash.
//! 2. The frontend reports the DOM rect of its `#player-host` element. We
//!    convert it (WebView2 uses physical pixels) and create a borderless
//!    WS_CHILD window over that rect — this is the real native render target.
//!    No WebView pointer is ever used as an mpv target.
//! 3. A dedicated `mpv-worker` thread owns the mpv handle (thread affinity),
//!    creates the OpenGL render context bound to the child HWND, observes all
//!    playback properties, and forwards every MPV_EVENT_PROPERTY_CHANGE to the
//!    UI via `player://event`. UI state comes from real MPV events only.
//! 4. Resize moves/resizes the child window; teardown frees the render
//!    context before destroying mpv and unloading the DLL, in order.

use std::ffi::{c_void, CString};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use widestring::WideCString;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::database::error::{AppError, AppResult};
use crate::player::api::{PlayerBackend, PlayerStatus, TrackInfo};
use crate::player::MpvEvent;

// ---- minimal libmpv C ABI --------------------------------------------------

#[repr(C)]
struct MpvNode {
    format: u32,
    u: MpvNodeU,
}
#[repr(C)]
union MpvNodeU {
    flag: i8,
    i64: i64,
    double_: f64,
    string: *const std::ffi::c_char,
    list: *mut MpvNodeList,
    size: usize,
}
#[repr(C)]
struct MpvNodeList {
    count: i32,
    name: *mut *const std::ffi::c_char,
    nodes: *mut MpvNode,
}

const MPV_FORMAT_FLAG: u32 = 1;
const MPV_FORMAT_DOUBLE: u32 = 3;
const MPV_FORMAT_STRING: u32 = 5;
const MPV_FORMAT_NODE: u32 = 7;
const MPV_FORMAT_NODE_ARRAY: u32 = 8;

#[repr(C)]
#[derive(Clone, Copy)]
union RenderParamU {
    ptr: *const c_void,
    i: i32,
    d: f64,
    flag: bool,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct MpvRenderParam {
    name: *const i8,
    data: RenderParamU,
}

#[repr(C)]
struct MpvOpenGLInitParams {
    get_proc_address: Option<extern "system" fn(*const c_void, *const i8) -> *const c_void>,
    proc_address_context: *const c_void,
}

struct LibMpv {
    module: HMODULE,
    create: unsafe extern "C" fn() -> *mut c_void,
    destroy: unsafe extern "C" fn(*mut c_void),
    initialize: unsafe extern "C" fn(*mut c_void, *const c_void) -> i32,
    set_option: unsafe extern "C" fn(*mut c_void, *const i8, *const i8) -> i32,
    command: unsafe extern "C" fn(*mut c_void, *const *const i8) -> i32,
    set_property_string: unsafe extern "C" fn(*mut c_void, *const i8, *const i8) -> i32,
    get_property: unsafe extern "C" fn(*mut c_void, *const i8, u32, *mut c_void) -> i32,
    free_node: unsafe extern "C" fn(*mut MpvNode),
    wait_event: unsafe extern "C" fn(*mut c_void, f64) -> *const c_void,
    observe_property: unsafe extern "C" fn(*mut c_void, u64, *const i8, u32) -> i32,
    render_create: unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *const MpvRenderParam) -> i32,
    render_free: unsafe extern "C" fn(*mut c_void),
    api_version: unsafe extern "C" fn(u32) -> i32,
}

fn wide(p: &PathBuf) -> WideCString {
    WideCString::from_u_str(p.as_os_str().encode_wide().collect::<Vec<u16>>()).unwrap()
}

impl LibMpv {
    fn load() -> AppResult<Arc<LibMpv>> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("libmpv-2.dll"));
                candidates.push(dir.join("mpv").join("libmpv-2.dll"));
            }
        }
        let mut module: HMODULE = std::ptr::null_mut();
        for c in &candidates {
            module = unsafe { LoadLibraryExW(wide(c).as_ptr(), NULL, 0) };
            if !module.is_null() {
                break;
            }
        }
        if module.is_null() {
            // fall back to default search (PATH, system dirs)
            let w = WideCString::from_str_triple("libmpv-2.dll");
            module = unsafe { LoadLibraryW(w.as_ptr()) };
        }
        if module.is_null() {
            return Err(AppError::PlayerUnavailable(
                "libmpv-2.dll not found. Install an x64 libmpv build (e.g. from the mpv-build \
                 releases) and place libmpv-2.dll next to fets-anime.exe or on PATH. See docs/MPV_SETUP.md."
                    .into(),
            ));
        }
        macro_rules! sym {
            ($n:expr, $t:ty) => {{
                let name = CString::new($n).unwrap();
                let p = unsafe { GetProcAddress(module, name.as_ptr()) };
                match p {
                    Some(f) => unsafe { std::mem::transmute::<_, $t>(f as usize) },
                    None => {
                        unsafe {
                            FreeLibrary(module);
                        }
                        return Err(AppError::PlayerUnavailable(format!(
                            "libmpv-2.dll is missing required symbol '{}'",
                            $n
                        )));
                    }
                }
            }};
        }
        let lib = Arc::new(LibMpv {
            module,
            create: sym!("mpv_create", unsafe extern "C" fn() -> *mut c_void),
            destroy: sym!("mpv_destroy", unsafe extern "C" fn(*mut c_void)),
            initialize: sym!("mpv_initialize", unsafe extern "C" fn(*mut c_void, *const c_void) -> i32),
            set_option: sym!("mpv_set_option_string", unsafe extern "C" fn(*mut c_void, *const i8, *const i8) -> i32),
            command: sym!("mpv_command", unsafe extern "C" fn(*mut c_void, *const *const i8) -> i32),
            set_property_string: sym!("mpv_set_property_string", unsafe extern "C" fn(*mut c_void, *const i8, *const i8) -> i32),
            get_property: sym!("mpv_get_property", unsafe extern "C" fn(*mut c_void, *const i8, u32, *mut c_void) -> i32),
            free_node: sym!("mpv_free_node_contents", unsafe extern "C" fn(*mut MpvNode)),
            wait_event: sym!("mpv_wait_event", unsafe extern "C" fn(*mut c_void, f64) -> *const c_void),
            observe_property: sym!("mpv_observe_property", unsafe extern "C" fn(*mut c_void, u64, *const i8, u32) -> i32),
            render_create: sym!(
                "mpv_render_context_create",
                unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *const MpvRenderParam) -> i32
            ),
            render_free: sym!("mpv_render_context_free", unsafe extern "C" fn(*mut c_void)),
            api_version: sym!("mpv_client_api_version", unsafe extern "C" fn(u32) -> i32),
        });
        log::info!("libmpv client API version: {}", unsafe { (lib.api_version)(0xFFFF_FFFF) });
        Ok(lib)
    }

    fn unload(&self) {
        unsafe {
            FreeLibrary(self.module);
        }
    }
}

fn cs(v: &str) -> CString {
    CString::new(v).unwrap_or_else(|_| CString::new("").unwrap())
}

// ---- GL proc address resolver ---------------------------------------------

extern "system" fn gl_get_proc_address(_ctx: *const c_void, name: *const i8) -> *const c_void {
    static WGL: OnceLock<HMODULE> = OnceLock::new();
    let hmod = WGL.get_or_init(|| unsafe {
        let w = WideCString::from_str_triple("opengl32.dll");
        LoadLibraryW(w.as_ptr())
    });
    unsafe {
        let mut addr = GetProcAddress(*hmod, name) as *const c_void;
        if addr.is_null() {
            addr = wglGetProcAddress(name) as *const c_void;
        }
        // wglGetProcAddress returns 1..=3 for errors
        if (addr as isize) <= 3 {
            return std::ptr::null();
        }
        addr
    }
}

// Win32 import declared manually to keep feature surface small.
#[link(name = "opengl32")]
extern "system" {
    fn wglGetProcAddress(name: *const i8) -> *const c_void;
}

// ---- jobs / worker ---------------------------------------------------------

enum Job {
    Init(HWND, Sender<AppResult<()>>),
    SetBounds(i32, i32, i32, i32, Sender<AppResult<()>>),
    Load(String, Option<f64>, Sender<AppResult<()>>),
    Pause(bool, Sender<AppResult<()>>),
    Seek(f64, bool, Sender<AppResult<()>>),
    Volume(i64, Sender<AppResult<()>>),
    Muted(bool, Sender<AppResult<()>>),
    Speed(f64, Sender<AppResult<()>>),
    SetTrack(String, i64, Sender<AppResult<()>>),
    Status(Sender<AppResult<PlayerStatus>>),
    Tracks(Sender<AppResult<Vec<TrackInfo>>>),
    Shutdown(Sender<()>),
}

pub struct MpvEmbed {
    tx: Sender<Job>,
    active: Arc<AtomicBool>,
    event_sink: Arc<Mutex<Option<tauri::AppHandle>>>,
    worker: Arc<Mutex<Option<JoinHandle<()>>>>,
    parent_hwnd: Arc<AtomicI64>,
}

impl MpvEmbed {
    pub fn new() -> Self {
        let (tx, rx) = channel::<Job>();
        let active = Arc::new(AtomicBool::new(false));
        let event_sink: Arc<Mutex<Option<tauri::AppHandle>>> = Arc::new(Mutex::new(None));
        let parent_hwnd = Arc::new(AtomicI64::new(0));
        let h = std::thread::Builder::new()
            .name("mpv-worker".into())
            .spawn({
                let a = active.clone();
                let es = event_sink.clone();
                move || worker_loop(rx, a, es)
            })
            .expect("spawn mpv worker");
        MpvEmbed {
            tx,
            active,
            event_sink,
            worker: Arc::new(Mutex::new(Some(h))),
            parent_hwnd,
        }
    }

    /// Wire the Tauri app handle so backend events reach the webview.
    pub fn attach_app(&self, app: tauri::AppHandle) {
        if let Ok(mut g) = self.event_sink.lock() {
            *g = Some(app);
        }
    }

    /// Resolve the native frame HWND of the main window (parent of the
    /// WebView2 child). This is what we embed into — never the WebView itself.
    pub fn set_parent_from_window(&self, window: &tauri::WebviewWindow) -> AppResult<()> {
        let hwnd = window
            .hwnd()
            .map_err(|e| AppError::PlayerUnavailable(format!("cannot get window HWND: {e}")))?;
        let raw = hwnd.0 as isize;
        let parent = unsafe { GetAncestor(raw as HWND, GA_ROOT_PARENT) };
        let target = if parent.is_null() { raw as HWND } else { parent };
        self.parent_hwnd.store(target as i64, Ordering::SeqCst);
        Ok(())
    }

    fn request<T>(&self, make: impl FnOnce(Sender<AppResult<T>>) -> Job) -> AppResult<T>
    where
        T: Send + 'static,
    {
        let (tx, rx) = channel();
        self.tx
            .send(make(tx))
            .map_err(|_| AppError::PlayerUnavailable("player worker is gone".into()))?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| AppError::PlayerUnavailable("player did not respond in time".into()))?
    }

    fn fire(&self, job: Job) {
        let _ = self.tx.send(job);
    }
}

impl Drop for MpvEmbed {
    fn drop(&mut self) {
        PlayerBackend::shutdown(self);
    }
}

impl PlayerBackend for MpvEmbed {
    fn init(&self, _hwnd_unused: isize) -> AppResult<()> {
        let parent = self.parent_hwnd.load(Ordering::SeqCst) as HWND;
        if parent.is_null() {
            return Err(AppError::PlayerUnavailable("main window handle unavailable".into()));
        }
        let r = self.request(move |tx| Job::Init(parent, tx))?;
        if r.is_ok() {
            self.active.store(true, Ordering::SeqCst);
        }
        r
    }
    fn load(&self, path: &str, start_pos: Option<f64>) -> AppResult<()> {
        if !self.active.load(Ordering::SeqCst) {
            return Err(AppError::PlayerUnavailable("player not initialized".into()));
        }
        let p = path.to_string();
        self.request(move |tx| Job::Load(p, start_pos, tx))
    }
    fn play_pause(&self, pause: bool) -> AppResult<()> {
        self.request(move |tx| Job::Pause(pause, tx))
    }
    fn seek(&self, secs: f64, absolute: bool) -> AppResult<()> {
        self.request(move |tx| Job::Seek(secs, absolute, tx))
    }
    fn set_volume(&self, vol: i64) -> AppResult<()> {
        self.request(move |tx| Job::Volume(vol, tx))
    }
    fn set_muted(&self, muted: bool) -> AppResult<()> {
        self.request(move |tx| Job::Muted(muted, tx))
    }
    fn set_speed(&self, speed: f64) -> AppResult<()> {
        self.request(move |tx| Job::Speed(speed, tx))
    }
    fn cycle_track(&self, kind: &str, id: i64) -> AppResult<()> {
        let k = kind.to_string();
        self.request(move |tx| Job::SetTrack(k, id, tx))
    }
    fn status(&self) -> AppResult<PlayerStatus> {
        self.request(|tx| Job::Status(tx))
    }
    fn tracks(&self) -> AppResult<Vec<TrackInfo>> {
        self.request(|tx| Job::Tracks(tx))
    }
    fn resize(&self, width: u32, height: u32) {
        // Bounds updates go through set_bounds; plain resize just repaints.
        let _ = (width, height);
    }
    fn shutdown(&self) {
        let (tx, rx) = channel();
        if self.tx.send(Job::Shutdown(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(5));
        }
        if let Ok(mut g) = self.worker.lock() {
            if let Some(h) = g.take() {
                let _ = h.join();
            }
        }
        self.active.store(false, Ordering::SeqCst);
    }
    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

/// Move/resize the embedded child window to the player host rect (physical px).
pub fn set_player_bounds(embed: &MpvEmbed, x: i32, y: i32, w: i32, h: i32) -> AppResult<()> {
    if !embed.is_active() {
        return Ok(());
    }
    let (tx, rx) = channel();
    embed.fire(Job::SetBounds(x, y, w, h, tx));
    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|_| AppError::PlayerUnavailable("bounds update timed out".into()))?
}

// ---- session (owned by worker thread) -------------------------------------

struct Session {
    lib: Arc<LibMpv>,
    mpv: *mut c_void,
    render_ctx: *mut c_void,
    child: HWND,
}

unsafe impl Send for Session {}

const WATCHED_PROPS: &[&str] = &[
    "pause", "time-pos", "duration", "eof-reached", "volume", "mute", "speed",
    "filename", "vid", "aid", "sid", "idle-active", "media-title", "track-list",
];

fn set_prop(s: &Session, name: &str, val: &str) -> AppResult<()> {
    let (cn, cv) = (cs(name), cs(val));
    if unsafe { (s.lib.set_property_string)(s.mpv, cn.as_ptr(), cv.as_ptr()) } == 0 {
        Ok(())
    } else {
        Err(AppError::Other(format!("failed to set mpv property '{name}'")))
    }
}

fn command(s: &Session, args: &[&str]) -> AppResult<()> {
    let strs: Vec<CString> = args.iter().map(|a| cs(a)).collect();
    let mut ptrs: Vec<*const i8> = strs.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    if unsafe { (s.lib.command)(s.mpv, ptrs.as_ptr()) } == 0 {
        Ok(())
    } else {
        Err(AppError::Other(format!("mpv command failed: {:?}", args.first())))
    }
}

fn prop_f64(s: &Session, name: &str) -> f64 {
    let cn = cs(name);
    let mut v: f64 = f64::NAN;
    unsafe {
        (s.lib.get_property)(s.mpv, cn.as_ptr(), MPV_FORMAT_DOUBLE, &mut v as *mut _ as *mut c_void);
    }
    v
}

fn prop_flag(s: &Session, name: &str) -> bool {
    let cn = cs(name);
    let mut v: i32 = 0;
    unsafe {
        (s.lib.get_property)(s.mpv, cn.as_ptr(), MPV_FORMAT_FLAG, &mut v as *mut _ as *mut c_void);
    }
    v != 0
}

fn prop_str(s: &Session, name: &str) -> String {
    let cn = cs(name);
    let mut out: *mut i8 = std::ptr::null_mut();
    unsafe {
        if (s.lib.get_property)(s.mpv, cn.as_ptr(), MPV_FORMAT_STRING, &mut out as *mut _ as *mut c_void) == 0
            && !out.is_null()
        {
            let v = std::ffi::CStr::from_ptr(out).to_string_lossy().to_string();
            mpv_free(out as *mut c_void);
            return v;
        }
    }
    String::new()
}

/// mpv_free must come from the dynamically loaded DLL (we may statically
/// link nothing against libmpv so the app runs without it installed).
fn mpv_free(data: *mut c_void) {
    static FREE: OnceLock<Option<unsafe extern "C" fn(*mut c_void)>> = OnceLock::new();
    let f = FREE.get_or_init(|| unsafe {
        let name = CString::new("mpv_free").unwrap();
        // find our loaded module handle by symbol lookup on default search
        let w = WideCString::from_str_triple("libmpv-2.dll");
        let h = GetModuleHandleW(w.as_ptr());
        if h.is_null() {
            None
        } else {
            GetProcAddress(h, name.as_ptr()).map(|p| std::mem::transmute::<_, unsafe extern "C" fn(*mut c_void)>(p as usize))
        }
    });
    if let Some(f) = f {
        unsafe { f(data) }
    }
}

fn read_tracks(s: &Session) -> Vec<TrackInfo> {
    let cn = cs("track-list");
    let mut node = MpvNode {
        format: MPV_FORMAT_NODE_ARRAY,
        u: MpvNodeU { size: 0 },
    };
    let mut out = Vec::new();
    unsafe {
        if (s.lib.get_property)(s.mpv, cn.as_ptr(), MPV_FORMAT_NODE, &mut node as *mut _ as *mut c_void) != 0 {
            return out;
        }
        if node.format != MPV_FORMAT_NODE_ARRAY || node.u.list.is_null() {
            (s.lib.free_node)(&mut node);
            return out;
        }
        let list = &*node.u.list;
        for i in 0..list.count.max(0) as usize {
            let n = &*list.nodes.add(i);
            if n.format != MPV_FORMAT_NODE_ARRAY || n.u.list.is_null() {
                continue;
            }
            let props = &*n.u.list;
            let mut t = TrackInfo { id: -1, kind: String::new(), selected: false, title: None, lang: None };
            for j in 0..props.count.max(0) as usize {
                let key = std::ffi::CStr::from_ptr(*props.name.add(j)).to_string_lossy().to_string();
                let val = &*props.nodes.add(j);
                match key.as_str() {
                    "id" => t.id = val.u.i64,
                    "type" if !val.u.string.is_null() => {
                        t.kind = std::ffi::CStr::from_ptr(val.u.string).to_string_lossy().to_string()
                    }
                    "selected" => t.selected = val.u.flag != 0,
                    "title" if !val.u.string.is_null() => {
                        t.title = Some(std::ffi::CStr::from_ptr(val.u.string).to_string_lossy().to_string())
                    }
                    "lang" if !val.u.string.is_null() => {
                        t.lang = Some(std::ffi::CStr::from_ptr(val.u.string).to_string_lossy().to_string())
                    }
                    _ => {}
                }
            }
            out.push(t);
        }
        (s.lib.free_node)(&mut node);
    }
    out
}

fn emit(es: &Arc<Mutex<Option<tauri::AppHandle>>>, ev: MpvEvent) {
    if let Ok(g) = es.lock() {
        if let Some(app) = g.as_ref() {
            use tauri::Emitter;
            let _ = app.emit("player://event", &ev);
        }
    }
}

#[repr(C)]
struct RawEvent {
    reserved: u32,
    id: i32,
    error: i32,
    data: *mut c_void,
}
#[repr(C)]
struct PropChangeData {
    name: *const i8,
    format: u32,
    // union follows
}
#[repr(C)]
union PropData {
    flag: i8,
    double_: f64,
    string: *const i8,
}
#[repr(C)]
struct EndFileData {
    _reserved: u32,
    file_error: i32,
}

const EV_START_FILE: i32 = 5;
const EV_PROPERTY_CHANGE: i32 = 6;
const EV_END_FILE: i32 = 7;
const EV_FILE_LOADED: i32 = 8;
const EV_SHUTDOWN: i32 = 13;

fn pump_events(s: &Session, es: &Arc<Mutex<Option<tauri::AppHandle>>>, timeout: f64) -> bool {
    let mut shutdown = false;
    loop {
        let evp = unsafe { (s.lib.wait_event)(s.mpv, if shutdown { 0.0 } else { timeout }) };
        if evp.is_null() {
            break;
        }
        let e = unsafe { &*(evp as *const RawEvent) };
        match e.id {
            EV_PROPERTY_CHANGE if !e.data.is_null() => {
                let pc = unsafe { &*(e.data as *const PropChangeData) };
                let name = unsafe { std::ffi::CStr::from_ptr(pc.name) }.to_string_lossy().to_string();
                let value = unsafe {
                    let du = &*(e.data as *const u8).add(std::mem::size_of::<PropChangeData>()) as *const PropData;
                    match pc.format {
                        MPV_FORMAT_FLAG => serde_json::json!((*du).flag != 0),
                        MPV_FORMAT_DOUBLE => serde_json::json!((*du).double_),
                        MPV_FORMAT_STRING => {
                            let p = (*du).string;
                            if p.is_null() {
                                serde_json::Value::Null
                            } else {
                                serde_json::json!(std::ffi::CStr::from_ptr(p).to_string_lossy())
                            }
                        }
                        _ => serde_json::Value::Null,
                    }
                };
                if name == "track-list" {
                    let tracks = read_tracks(s);
                    emit(
                        es,
                        MpvEvent {
                            name: "track-list".into(),
                            value: serde_json::to_value(&tracks).unwrap_or_default(),
                        },
                    );
                } else {
                    emit(es, MpvEvent { name, value });
                }
            }
            EV_START_FILE | EV_FILE_LOADED => {
                let tracks = read_tracks(s);
                emit(
                    es,
                    MpvEvent {
                        name: "track-list".into(),
                        value: serde_json::to_value(&tracks).unwrap_or_default(),
                    },
                );
            }
            EV_END_FILE if !e.data.is_null() => {
                let ef = unsafe { &*(e.data as *const EndFileData) };
                if ef.file_error != 0 {
                    emit(
                        es,
                        MpvEvent {
                            name: "file-error".into(),
                            value: serde_json::json!(format!(
                                "Playback failed (mpv error {}). The file may be missing, corrupt or unsupported.",
                                ef.file_error
                            )),
                        },
                    );
                }
            }
            EV_SHUTDOWN => shutdown = true,
            _ => {}
        }
        if shutdown {
            break;
        }
    }
    shutdown
}

fn create_child(parent: HWND) -> AppResult<HWND> {
    // Child window class registered once per process.
    #[link(name = "user32")]
    extern "system" {
        fn RegisterClassExW(lpwce: *const WNDCLASSEXW) -> u16;
    }
    #[repr(C)]
    struct WNDCLASSEXW {
        cb_size: u32,
        style: u32,
        lpfn_wnd_proc: usize,
        cb_cls_extra: u32,
        cb_wnd_extra: u32,
        h_instance: HINSTANCE,
        h_icon: HICON,
        h_cursor: HCURSOR,
        h_brush: HBRUSH,
        lpsz_class_name: *const u16,
        lpsz_menu_name: *const u16,
        h_icon_sm: HICON,
    }
    static ATOM: OnceLock<u16> = OnceLock::new();
    let atom = *ATOM.get_or_init(|| unsafe {
        let cls = WideCString::from_str_triple("FetsMpvHost");
        let wc = WNDCLASSEXW {
            cb_size: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfn_wnd_proc: def_child_proc as usize,
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: GetModuleHandleW(std::ptr::null()),
            h_icon: NULL as HICON,
            h_cursor: NULL as HCURSOR,
            h_brush: NULL as HBRUSH,
            lpsz_class_name: cls.as_ptr(),
            lpsz_menu_name: std::ptr::null(),
            h_icon_sm: NULL as HICON,
        };
        RegisterClassExW(&wc)
    });
    if atom == 0 {
        return Err(AppError::PlayerUnavailable("failed to register mpv host window class".into()));
    }
    let cls = WideCString::from_str_triple("FetsMpvHost");
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            cls.as_ptr(),
            std::ptr::null(),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            0,
            0,
            1,
            1,
            parent,
            NULL,
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(AppError::PlayerUnavailable("CreateWindowExW for mpv host failed".into()));
    }
    Ok(hwnd)
}

unsafe extern "system" fn def_child_proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, msg, w, l)
}

fn init_session(parent: HWND) -> AppResult<Session> {
    let lib = LibMpv::load()?;
    unsafe {
        let mpv = (lib.create)();
        if mpv.is_null() {
            lib.unload();
            return Err(AppError::PlayerUnavailable("mpv_create failed".into()));
        }
        // Options must be set before mpv_initialize.
        (lib.set_option)(mpv, b"terminal\0".as_ptr(), b"no\0".as_ptr());
        (lib.set_option)(mpv, b"osc\0".as_ptr(), b"no\0".as_ptr());
        (lib.set_option)(mpv, b"load-scripts\0".as_ptr(), b"no\0".as_ptr());
        (lib.set_option)(mpv, b"input-default-bindings\0".as_ptr(), b"no\0".as_ptr());
        (lib.set_option)(mpv, b"keep-open\0".as_ptr(), b"yes\0".as_ptr());
        // hwdec auto-safe: hardware when whitelisted/safe, software fallback otherwise.
        (lib.set_option)(mpv, b"hwdec\0".as_ptr(), b"auto-safe\0".as_ptr());
        (lib.set_option)(mpv, b"gpu-context\0".as_ptr(), b"win\0".as_ptr());

        let initp = MpvOpenGLInitParams {
            get_proc_address: Some(gl_get_proc_address),
            proc_address_context: std::ptr::null(),
        };
        if (lib.initialize)(mpv, &initp as *const _ as *const c_void) != 0 {
            (lib.destroy)(mpv);
            lib.unload();
            return Err(AppError::PlayerUnavailable("mpv_initialize failed".into()));
        }

        let child = match create_child(parent) {
            Ok(c) => c,
            Err(e) => {
                (lib.destroy)(mpv);
                lib.unload();
                return Err(e);
            }
        };

        let wid = child as i64;
        let mut ctx: *mut c_void = std::ptr::null_mut();
        let params = [
            MpvRenderParam {
                name: b"api\0".as_ptr() as *const i8,
                data: RenderParamU { ptr: b"opengl\0".as_ptr() as *const c_void },
            },
            MpvRenderParam {
                name: b"opengl-init-proc-address\0".as_ptr() as *const i8,
                data: RenderParamU { ptr: &initp as *const _ as *const c_void },
            },
            MpvRenderParam {
                name: b"wid\0".as_ptr() as *const i8,
                data: RenderParamU { ptr: wid as *const c_void },
            },
            MpvRenderParam { name: std::ptr::null(), data: RenderParamU { ptr: std::ptr::null() } },
        ];
        let rc = (lib.render_create)(&mut ctx, mpv, params.as_ptr());
        if rc != 0 {
            DestroyWindow(child);
            (lib.destroy)(mpv);
            lib.unload();
            return Err(AppError::PlayerUnavailable(format!(
                "mpv_render_context_create failed (code {rc}). Use an x64 libmpv build with GPU support."
            )));
        }

        for p in WATCHED_PROPS {
            let cn = cs(p);
            (lib.observe_property)(mpv, 0, cn.as_ptr(), MPV_FORMAT_NODE);
        }

        Ok(Session {
            lib,
            mpv,
            render_ctx: ctx,
            child,
        })
    }
}

fn teardown(s: Session) {
    unsafe {
        // Correct order: stop → free render context → destroy mpv → destroy
        // child window → unload library. Nothing is freed while callbacks or
        // rendering could still reference it (worker thread owns all of it).
        let _ = command(&s, &["stop"]);
        if !s.render_ctx.is_null() {
            (s.lib.render_free)(s.render_ctx);
        }
        if !s.mpv.is_null() {
            (s.lib.destroy)(s.mpv);
        }
        if !s.child.is_null() {
            DestroyWindow(s.child);
        }
        s.lib.unload();
    }
    log::info!("mpv session torn down cleanly");
}

fn worker_loop(rx: Receiver<Job>, active: Arc<AtomicBool>, es: Arc<Mutex<Option<tauri::AppHandle>>>) {
    let mut sess: Option<Session> = None;
    loop {
        let job = match rx.recv() {
            Ok(j) => j,
            Err(_) => break,
        };
        let mut quit = false;
        match job {
            Job::Init(parent, ack) => {
                let r = match sess.take() {
                    Some(old) => {
                        teardown(old);
                        init_session(parent)
                    }
                    None => init_session(parent),
                };
                match r {
                    Ok(s) => {
                        sess = Some(s);
                        let _ = ack.send(Ok(()));
                    }
                    Err(e) => {
                        log::error!("mpv init failed: {e}");
                        let _ = ack.send(Err(e));
                    }
                }
            }
            Job::SetBounds(x, y, w, h, ack) => {
                let r = match &sess {
                    Some(s) => {
                        unsafe {
                            SetWindowPos(
                                s.child,
                                NULL as HWND,
                                x,
                                y,
                                w.max(1),
                                h.max(1),
                                SWP_NOZORDER | SWP_NOACTIVATE,
                            )
                        };
                        Ok(())
                    }
                    None => Err(AppError::PlayerUnavailable("player not initialized".into())),
                };
                let _ = ack.send(r);
            }
            Job::Load(path, start, ack) => {
                let r = sess
                    .as_ref()
                    .map_or(Err(AppError::PlayerUnavailable("player not initialized".into())), |s| {
                        command(s, &["loadfile", &path])?;
                        if let Some(p) = start.filter(|p| *p > 5.0) {
                            let ps = p.to_string();
                            let _ = command(s, &["seek", &ps, "absolute"]);
                        }
                        let _ = set_prop(s, "pause", "no");
                        Ok(())
                    });
                let _ = ack.send(r);
            }
            Job::Pause(p, ack) => {
                let _ = ack.send(sess.as_ref().map_or(Err(AppError::PlayerUnavailable("player off".into())), |s| {
                    set_prop(s, "pause", if p { "yes" } else { "no" })
                }));
            }
            Job::Seek(secs, absolute, ack) => {
                let _ = ack.send(sess.as_ref().map_or(Err(AppError::PlayerUnavailable("player off".into())), |s| {
                    let v = secs.to_string();
                    let mode = if absolute { "absolute" } else { "relative" };
                    command(s, &["seek", &v, mode])
                }));
            }
            Job::Volume(v, ack) => {
                let _ = ack.send(sess.as_ref().map_or(Err(AppError::PlayerUnavailable("player off".into())), |s| {
                    set_prop(s, "volume", &v.clamp(0, 130).to_string())
                }));
            }
            Job::Muted(m, ack) => {
                let _ = ack.send(sess.as_ref().map_or(Err(AppError::PlayerUnavailable("player off".into())), |s| {
                    set_prop(s, "mute", if m { "yes" } else { "no" })
                }));
            }
            Job::Speed(v, ack) => {
                let _ = ack.send(sess.as_ref().map_or(Err(AppError::PlayerUnavailable("player off".into())), |s| {
                    set_prop(s, "speed", &v.to_string())
                }));
            }
            Job::SetTrack(kind, id, ack) => {
                let _ = ack.send(sess.as_ref().map_or(Err(AppError::PlayerUnavailable("player off".into())), |s| {
                    let prop = match kind.as_str() {
                        "video" => "vid",
                        "audio" => "aid",
                        "sub" | "subtitle" => "sid",
                        _ => return Err(AppError::InvalidInput("unknown track kind".into())),
                    };
                    set_prop(s, prop, &id.to_string())
                }));
            }
            Job::Status(ack) => {
                let _ = ack.send(sess.as_ref().map_or(Err(AppError::PlayerUnavailable("player off".into())), |s| {
                    Ok(PlayerStatus {
                        idle: prop_flag(s, "idle-active"),
                        paused: prop_flag(s, "pause"),
                        position: prop_f64(s, "time-pos"),
                        duration: prop_f64(s, "duration"),
                        volume: prop_f64(s, "volume") as i64,
                        muted: prop_flag(s, "mute"),
                        speed: prop_f64(s, "speed"),
                        filename: prop_str(s, "filename"),
                    })
                }??));
            }
            Job::Tracks(ack) => {
                let _ = ack.send(sess.as_ref().map_or(Err(AppError::PlayerUnavailable("player off".into())), |s| Ok(read_tracks(s))));
            }
            Job::Shutdown(ack) => {
                if let Some(s) = sess.take() {
                    teardown(s);
                }
                active.store(false, Ordering::SeqCst);
                let _ = ack.send(());
                return;
            }
        }

        // Event pump: after each command drain quickly, then wait up to 250ms
        // for the next event/job (≈0% CPU while idle, smooth progress updates
        // while playing since mpv emits time-pos continuously).
        loop {
            let dead = match &sess {
                Some(s) => pump_events(s, &es, 0.05),
                None => false,
            };
            if dead {
                if let Some(s) = sess.take() {
                    teardown(s);
                }
                quit = true;
                break;
            }
            match rx.try_recv() {
                Ok(next) => {
                    // dispatch nested job synchronously
                    let _ = handle_nested(next, &mut sess, &active, &es);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    quit = true;
                    break;
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(20));
                    if sess.is_some() {
                        // bounded non-blocking re-pump keeps latency low
                        if let Some(s) = &sess {
                            if pump_events(s, &es, 0.0) {
                                if let Some(s) = sess.take() {
                                    teardown(s);
                                }
                                quit = true;
                            }
                        }
                    } else {
                        break;
                    }
                    if quit {
                        break;
                    }
                }
            }
        }
        if quit {
            break;
        }
    }
    if let Some(s) = sess.take() {
        teardown(s);
    }
    log::info!("mpv worker exited");
}

fn handle_nested(
    job: Job,
    sess: &mut Option<Session>,
    _active: &Arc<AtomicBool>,
    _es: &Arc<Mutex<Option<tauri::AppHandle>>>,
) -> bool {
    match job {
        Job::Shutdown(ack) => {
            if let Some(s) = sess.take() {
                teardown(s);
            }
            let _ = ack.send(());
            true
        }
        Job::SetBounds(x, y, w, h, ack) => {
            if let Some(s) = sess {
                unsafe {
                    SetWindowPos(s.child, NULL as HWND, x, y, w.max(1), h.max(1), SWP_NOZORDER | SWP_NOACTIVATE);
                }
            }
            let _ = ack.send(Ok(()));
            false
        }
        other => {
            // Requeue anything else by sending back through our own channel is
            // impossible here; handle the common control ops inline.
            if let Some(s) = sess.as_ref() {
                match other {
                    Job::Pause(p, ack) => {
                        let _ = ack.send(set_prop(s, "pause", if p { "yes" } else { "no" }));
                    }
                    Job::Volume(v, ack) => {
                        let _ = ack.send(set_prop(s, "volume", &v.clamp(0, 130).to_string()));
                    }
                    Job::Muted(m, ack) => {
                        let _ = ack.send(set_prop(s, "mute", if m { "yes" } else { "no" }));
                    }
                    Job::Speed(v, ack) => {
                        let _ = ack.send(set_prop(s, "speed", &v.to_string()));
                    }
                    Job::Seek(secs, abs, ack) => {
                        let v = secs.to_string();
                        let mode = if abs { "absolute" } else { "relative" };
                        let _ = ack.send(command(s, &["seek", &v, mode]));
                    }
                    _ => {}
                }
            }
            false
        }
    }
}
