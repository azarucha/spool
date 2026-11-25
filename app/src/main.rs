#![windows_subsystem = "windows"]
//! Spool: Fenster um die Oberflaeche in ui/ und den Kern in kern/.

use kern::api::{Api, Dialogs};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{DragDropEvent, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

static WINDOW: OnceLock<tauri::WebviewWindow> = OnceLock::new();
static OPEN: Mutex<()> = Mutex::new(());

/// Dialog, der ans Hauptfenster gebunden ist; nie zwei gleichzeitig.
fn dialog<T>(show: impl FnOnce(rfd::FileDialog) -> Option<T>) -> Option<T> {
    let _one = OPEN.try_lock().ok()?;
    let d = rfd::FileDialog::new();
    show(match WINDOW.get() {
        Some(w) => d.set_parent(w),
        None => d,
    })
}

struct Native;

impl Dialogs for Native {
    fn files(&self) -> Vec<PathBuf> {
        let all: Vec<&str> = [kern::names::VIDEO, kern::names::AUDIO, kern::archive::ARCHIVES].concat();
        dialog(|d| d.add_filter("Videos, Musik, Archive", &all).add_filter("Alle Dateien", &["*"]).pick_files()).unwrap_or_default()
    }
    fn folder(&self) -> Option<PathBuf> {
        dialog(|d| d.pick_folder())
    }
    fn open(&self, target: &str) {
        // nur feste Webseiten und vorhandene Ordner, nie Programme
        if target.starts_with("https://") || Path::new(target).is_dir() {
            use std::os::windows::ffi::OsStrExt;
            let wide = |s: &str| std::ffi::OsStr::new(s).encode_wide().chain([0]).collect::<Vec<u16>>();
            let (verb, what) = (wide(if target.starts_with("https://") { "open" } else { "explore" }), wide(target));
            // SAFETY: nullterminierte Zeichenketten, keine weiteren Zeiger.
            unsafe {
                windows_sys::Win32::UI::Shell::ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), what.as_ptr(), std::ptr::null(), std::ptr::null(), 1)
            };
        }
    }
    fn free(&self, path: &Path) -> Option<u64> {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut free = 0u64;
        // SAFETY: nullterminierter Pfad, gueltiger Ausgabezeiger.
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut())
        };
        (ok != 0).then_some(free)
    }
    fn volume(&self, path: &Path) -> String {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut out = [0u16; 512];
        // SAFETY: nullterminierter Pfad, Puffer mit angegebener Laenge.
        let ok = unsafe { windows_sys::Win32::Storage::FileSystem::GetVolumePathNameW(wide.as_ptr(), out.as_mut_ptr(), out.len() as u32) };
        let n = out.iter().position(|c| *c == 0).unwrap_or(0);
        if ok != 0 && n > 0 { String::from_utf16_lossy(&out[..n]).to_lowercase() } else { path.to_string_lossy().to_lowercase() }
    }
}

#[tauri::command]
async fn call(api: tauri::State<'_, Arc<Api>>, method: String, args: Vec<Value>) -> Result<Value, String> {
    let api = api.inner().clone();
    tauri::async_runtime::spawn_blocking(move || api.call(&method, &args)).await.map_err(|e| e.to_string())
}

/// Titelleiste in Fensterfarbe, damit alles wie eine Flaeche wirkt (Windows 11).
fn tint(window: &tauri::WebviewWindow) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    let Ok(hwnd) = window.hwnd() else { return };
    let dark = window.theme().is_ok_and(|t| t == tauri::Theme::Dark);
    let color: u32 = if dark { 0x001E1C1C } else { 0x00F7F5F5 }; // BGR
    for (attr, val) in [(20u32, dark as u32), (34, color), (35, color)] {
        // SAFETY: gueltiges Fenster, 4-Byte-Wert wie von DWM erwartet. Fehler sind rein kosmetisch.
        unsafe { DwmSetWindowAttribute(hwnd.0 as _, attr, &val as *const u32 as _, 4) };
    }
}

fn main() {
    let api = Api::new(Box::new(Native), kern::sources::all_sources());
    let closing = api.clone();
    let dropping = api.clone();
    let again = api.clone();
    tauri::Builder::default()
        // Nur ein Spool gleichzeitig: ein zweiter Start gibt seine Dateien weiter und holt das Fenster nach vorn
        .plugin(tauri_plugin_single_instance::init(move |app, args, _| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
            again.add(args.into_iter().skip(1).map(PathBuf::from).filter(|p| p.exists()).collect(), false);
        }))
        .manage(api.clone())
        .invoke_handler(tauri::generate_handler![call])
        .on_window_event(move |w, e| match e {
            WindowEvent::CloseRequested { api: ask, .. } if closing.busy() => {
                let text = "Es läuft noch etwas. Trotzdem beenden?\n\nEine angefangene Kopie wird beim nächsten Mal fortgesetzt.";
                let answer = rfd::MessageDialog::new().set_title("Spool").set_description(text).set_buttons(rfd::MessageButtons::YesNo).show();
                if answer != rfd::MessageDialogResult::Yes {
                    ask.prevent_close();
                }
            }
            WindowEvent::ThemeChanged(_) => {
                if let Some(w) = w.get_webview_window("main") {
                    tint(&w);
                }
            }
            WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) => {
                let (a, p) = (dropping.clone(), paths.clone());
                std::thread::spawn(move || a.add(p, false));
            }
            _ => {}
        })
        .setup(move |app| {
            let w = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("Spool")
                .inner_size(1240.0, 800.0)
                .min_inner_size(960.0, 620.0)
                .center()
                .on_navigation(|url| url.scheme() == "tauri" || url.host_str() == Some("tauri.localhost"))
                .build()?;
            tint(&w);
            let _ = WINDOW.set(w);
            let files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).filter(|p| p.exists()).collect();
            if !files.is_empty() {
                std::thread::spawn(move || api.add(files, false));
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Spool konnte nicht starten");
}
