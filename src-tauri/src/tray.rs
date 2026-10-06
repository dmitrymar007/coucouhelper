// Notification-area icon: Open, Settings, Pause, Stop chat, Updates, Quit.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};

use crate::island::WINDOW_LABEL;

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Coucou", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "Pause", true, None::<&str>)?;
    let stop_chat = MenuItem::with_id(app, "stop-chat", "Stop chat", true, None::<&str>)?;
    let update = MenuItem::with_id(app, "update", "Check for updates", crate::update::packaged(), None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(app, &[&open, &sep1, &settings, &pause, &stop_chat, &update, &sep2, &quit])?;

    let mut builder = TrayIconBuilder::with_id("coucou")
        .tooltip("Coucou")
        .menu(&menu)
        .on_menu_event(|app: &AppHandle, event| match event.id.as_ref() {
            "quit" => app.exit(0),
            "settings" => crate::show_settings_window(app),
            "stop-chat" => app.state::<crate::chat::Chat>().stop(),
            "update" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if crate::update::status(&app).available {
                        let _ = crate::update::install(&app).await;
                    } else if crate::update::check(&app).await.is_ok() && !crate::update::status(&app).available {
                        let _ = app.emit_to(WINDOW_LABEL, "update-none", ());
                    }
                });
            }
            id => {
                let _ = app.emit_to(WINDOW_LABEL, "tray", id.to_string());
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    app.manage(UpdateItem(update));
    Ok(())
}

/// The tray's update item, relabelled when a newer version is out.
struct UpdateItem(MenuItem<tauri::Wry>);

pub fn set_update(app: &AppHandle, version: Option<&str>) {
    if let Some(item) = app.try_state::<UpdateItem>() {
        let text = match version {
            Some(v) => format!("Update to {v}…"),
            None => "Check for updates".to_string(),
        };
        let _ = item.0.set_text(text);
    }
}
