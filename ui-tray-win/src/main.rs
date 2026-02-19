#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod config;
#[cfg(windows)]
mod procs;

#[cfg(not(windows))]
fn main() {
    eprintln!("ui-tray-win is Windows-only");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::sync::{Arc, Mutex};

    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{TrayIconBuilder, TrayIconEvent};

    use winit::event::{Event, StartCause};
    use winit::event_loop::{ControlFlow, EventLoop};

    #[derive(Debug, Clone)]
    enum UserEvent {
        Tray(TrayIconEvent),
        Menu(MenuEvent),
    }

    let procs: Arc<Mutex<procs::Procs>> = Arc::new(Mutex::new(procs::Procs::default()));
    let cfg: Arc<Mutex<config::UiConfig>> = Arc::new(Mutex::new(config::load_config().unwrap_or_default()));

    // Menu
    let menu = Menu::new();

    let open_ui = MenuItem::new("Open control panel", true, None);
    let reload_cfg = MenuItem::new("Reload config", true, None);
    let start_all = MenuItem::new("Start all", true, None);
    let stop_all = MenuItem::new("Stop all", true, None);
    let sep1 = PredefinedMenuItem::separator();
    let enable_autostart = MenuItem::new("Enable autostart", true, None);
    let disable_autostart = MenuItem::new("Disable autostart", true, None);
    let sep2 = PredefinedMenuItem::separator();
    let exit = MenuItem::new("Exit", true, None);

    menu.append(&open_ui)?;
    menu.append(&reload_cfg)?;
    menu.append(&start_all)?;
    menu.append(&stop_all)?;
    menu.append(&sep1)?;
    menu.append(&enable_autostart)?;
    menu.append(&disable_autostart)?;
    menu.append(&sep2)?;
    menu.append(&exit)?;

    // Icon: keep it simple (no custom icon yet). Some hosts will display a default.
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("MultiClipRelay")
        .build()?;

    // Event loop (user events)
    let event_loop: EventLoop<UserEvent> = EventLoop::with_user_event().build()?;

    let proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Tray(event));
    }));

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Menu(event));
    }));

    // Initial autostart menu state.
    {
        let on = autostart::is_enabled();
        enable_autostart.set_enabled(!on);
        disable_autostart.set_enabled(on);
    }

    // Capture ids to avoid borrowing the MenuItem handles outside the loop.
    let open_ui_id = open_ui.id().clone();
    let reload_cfg_id = reload_cfg.id().clone();
    let start_all_id = start_all.id().clone();
    let stop_all_id = stop_all.id().clone();
    let enable_autostart_id = enable_autostart.id().clone();
    let disable_autostart_id = disable_autostart.id().clone();
    let exit_id = exit.id().clone();

    event_loop
        .run(move |event, el| {
        // Default: wait for events.
        el.set_control_flow(ControlFlow::Wait);

        match event {
            Event::NewEvents(StartCause::Init) => {
                // prune exited children
                let mut st = procs.lock().unwrap();
                procs::prune_exited(&mut st.relay);
                procs::prune_exited(&mut st.watch);
                procs::prune_exited(&mut st.apply);

                // refresh autostart menu state
                let on = autostart::is_enabled();
                enable_autostart.set_enabled(!on);
                disable_autostart.set_enabled(on);
            }
            Event::UserEvent(UserEvent::Tray(tray_ev)) => {
                // Tray icon clicks etc.
                // Double click: open the control panel.
                if matches!(tray_ev, tray_icon::TrayIconEvent::DoubleClick { .. }) {
                    let _ = procs::spawn_ui_egui();
                }

                let mut st = procs.lock().unwrap();
                procs::prune_exited(&mut st.relay);
                procs::prune_exited(&mut st.watch);
                procs::prune_exited(&mut st.apply);
            }
            Event::UserEvent(UserEvent::Menu(me)) => {
                // prune exited children
                {
                    let mut st = procs.lock().unwrap();
                    procs::prune_exited(&mut st.relay);
                    procs::prune_exited(&mut st.watch);
                    procs::prune_exited(&mut st.apply);
                }

                let id = me.id().clone();

                if id == open_ui_id {
                    let _ = procs::spawn_ui_egui();
                } else if id == reload_cfg_id {
                    let new_cfg = config::load_config().unwrap_or_default();
                    *cfg.lock().unwrap() = new_cfg;
                } else if id == start_all_id {
                    let cfg2 = cfg.lock().unwrap().clone();
                    let mut st = procs.lock().unwrap();
                    if st.relay.is_none() {
                        st.relay = procs::spawn_relay(cfg2.relay_bind_hint().as_deref()).ok();
                    }
                    if st.apply.is_none() {
                        st.apply = procs::spawn_node(&[
                            "win-apply",
                            "--room",
                            &cfg2.room,
                            "--relay",
                            &cfg2.relay_addr,
                        ])
                        .ok();
                    }
                    if st.watch.is_none() {
                        let max_text = cfg2.max_text_bytes.to_string();
                        let max_img = cfg2.max_image_bytes.to_string();
                        let max_file = cfg2.max_file_bytes.to_string();
                        st.watch = procs::spawn_node(&[
                            "win-watch",
                            "--room",
                            &cfg2.room,
                            "--relay",
                            &cfg2.relay_addr,
                            "--max-text-bytes",
                            &max_text,
                            "--max-image-bytes",
                            &max_img,
                            "--max-file-bytes",
                            &max_file,
                        ])
                        .ok();
                    }
                } else if id == stop_all_id {
                    let mut st = procs.lock().unwrap();
                    procs::terminate_best_effort(&mut st.watch);
                    procs::terminate_best_effort(&mut st.apply);
                    procs::terminate_best_effort(&mut st.relay);
                } else if id == enable_autostart_id {
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = autostart::enable(&exe);
                    }
                    let on = autostart::is_enabled();
                    enable_autostart.set_enabled(!on);
                    disable_autostart.set_enabled(on);
                } else if id == disable_autostart_id {
                    let _ = autostart::disable();
                    let on = autostart::is_enabled();
                    enable_autostart.set_enabled(!on);
                    disable_autostart.set_enabled(on);
                } else if id == exit_id {
                    let mut st = procs.lock().unwrap();
                    procs::terminate_best_effort(&mut st.watch);
                    procs::terminate_best_effort(&mut st.apply);
                    procs::terminate_best_effort(&mut st.relay);
                    el.exit();
                }
            }
            _ => {}
        }
        })
        .map_err(|e| anyhow::anyhow!("event loop failed: {e:?}"))?;

    Ok(())
}
