//! AMWAPOS desktop shell.
//!
//! The shell is deliberately thin: it resolves the data folder, opens the
//! core, starts background services and exposes ONE typed IPC command
//! (`rpc`) that routes to the transport-neutral command dispatcher. There is no
//! IPC path to SQL, the file system or a shell.

mod hello;
mod secrets;

use std::path::PathBuf;
use std::sync::Arc;

use amwapos_core::service::AppCore;
use amwapos_core::AppError;
use amwapos_hub::Runtime;
use serde_json::Value;
use tauri::Manager;

struct AppState {
    rt: Option<Arc<Runtime>>,
    startup_error: Option<AppError>,
}

#[tauri::command]
async fn rpc(state: tauri::State<'_, AppState>, cmd: String, token: Option<String>, args: Option<Value>) -> Result<Value, AppError> {
    match (&state.rt, &state.startup_error) {
        (Some(rt), _) => rt.dispatch(&cmd, token, args.unwrap_or_else(|| serde_json::json!({}))).await,
        (None, Some(e)) => Err(e.clone()),
        _ => Err(AppError::internal("The application did not start.")),
    }
}

/// Store data outside the install folder so upgrades and uninstalls never
/// touch business records. Windows: %ProgramData%\AMWAPOS\data (shared by all
/// Windows users of the POS computer). Override with AMWAPOS_DATA_DIR.
fn data_dir(app: &tauri::App) -> PathBuf {
    if let Ok(d) = std::env::var("AMWAPOS_DATA_DIR") {
        return PathBuf::from(d);
    }
    #[cfg(windows)]
    {
        if let Ok(pd) = std::env::var("PROGRAMDATA") {
            return PathBuf::from(pd).join("AMWAPOS").join("data");
        }
    }
    app.path().app_data_dir().unwrap_or_else(|_| PathBuf::from("amwapos-data")).join("data")
}

fn init_logging(dir: &std::path::Path) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let logs = dir.parent().unwrap_or(dir).join("logs");
    std::fs::create_dir_all(&logs).ok()?;
    // Daily files, 30 kept: a till runs for years and must not fill its disk with logs.
    let appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("amwapos")
        .filename_suffix("log")
        .max_log_files(30)
        .build(&logs)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .json()
        .with_writer(writer)
        .with_env_filter(std::env::var("AMWAPOS_LOG").unwrap_or_else(|_| "info".into()))
        .with_current_span(false)
        .init();
    Some(guard)
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second launch focuses the running window instead of opening the database twice.
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .setup(|app| {
            let dir = data_dir(app);
            let guard = init_logging(&dir);
            // Keep the log writer alive for the process lifetime.
            app.manage(LogGuard(guard));
            tracing::info!(version = amwapos_core::audit::APP_VERSION, build = env!("AMWAPOS_BUILD_SHA"), data_dir = %dir.display(), "AMWAPOS starting");
            let state = match AppCore::open(&dir, Arc::new(secrets::OsSecretStore)) {
                Ok(core) => {
                    let rt = Runtime::new(Arc::new(core));
                    rt.set_sidecar_resources(app.path().resource_dir().ok().as_deref());
                    *rt.step_up.lock().unwrap() = Some(Arc::new(hello::verify));
                    let rt2 = rt.clone();
                    tauri::async_runtime::spawn(async move { rt2.ensure_services() });
                    AppState { rt: Some(rt), startup_error: None }
                }
                Err(e) => {
                    tracing::error!(code = ?e.code, error = %e.message, "startup failed");
                    AppState { rt: None, startup_error: Some(e) }
                }
            };
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![rpc])
        .run(tauri::generate_context!())
        .expect("error while running AMWAPOS");
}

struct LogGuard(#[allow(dead_code)] Option<tracing_appender::non_blocking::WorkerGuard>);
