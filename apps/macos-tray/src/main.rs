//! A real, windowless macOS client. Native objects stay on the main thread.
#[cfg(not(target_os = "macos"))]
compile_error!("synapsed-macos-tray is macOS-only; exclude it from Linux workspace selections.");

#[cfg(target_os = "macos")]
mod menu;
#[cfg(target_os = "macos")]
mod worker;

#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};
#[cfg(target_os = "macos")]
use tao::{
    event::{Event, StartCause},
    event_loop::{ControlFlow, EventLoopBuilder},
    platform::macos::{ActivationPolicy, EventLoopExtMacOS},
};
#[cfg(target_os = "macos")]
use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent, menu::MenuEvent};

#[cfg(target_os = "macos")]
enum UiEvent {
    Wake,
    Menu(MenuEvent),
    Tray(TrayIconEvent),
}

#[cfg(target_os = "macos")]
struct App {
    ui: Option<menu::Ui>,
    worker: worker::Worker,
}
#[cfg(target_os = "macos")]
impl App {
    fn apply(&mut self, mailbox: &worker::Mailbox) -> Result<(), String> {
        if let Some(ui) = &mut self.ui {
            ui.apply(mailbox.take())?;
        }
        Ok(())
    }
    fn stop(&mut self) {
        self.worker.stop();
        if let Some(ui) = self.ui.take() {
            ui.detach();
        }
    }
}

#[cfg(target_os = "macos")]
fn main() {
    // Tao must initialize NSApplication before any other library uses it. It is
    // constructed on main, with no window, Dock icon, or forced app activation.
    let mut event_loop = EventLoopBuilder::<UiEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    event_loop.set_dock_visibility(false);
    event_loop.set_activate_ignoring_other_apps(false);

    // Both libraries use a global OnceCell: register once, before native Init,
    // and never mix these handlers with their receiver() polling API.
    let menu_proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(UiEvent::Menu(event));
    }));
    let tray_proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |event| {
        if matches!(
            &event,
            TrayIconEvent::Click {
                button: MouseButton::Left | MouseButton::Right,
                button_state: MouseButtonState::Down,
                ..
            }
        ) {
            let _ = tray_proxy.send_event(UiEvent::Tray(event));
        }
    }));
    let mailbox = worker::Mailbox::new(event_loop.create_proxy());
    let mut app: Option<App> = None;
    let mut quitting: Option<Instant> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if quitting.is_none() {
            let outcome: Result<bool, String> = match event {
                Event::NewEvents(StartCause::Init) => menu::Ui::new().map(|ui| {
                    let worker = worker::Worker::start(mailbox.clone());
                    app = Some(App {
                        ui: Some(ui),
                        worker,
                    });
                    false
                }),
                Event::UserEvent(UiEvent::Wake) => app
                    .as_mut()
                    .map_or(Ok(false), |app| app.apply(&mailbox).map(|()| false)),
                Event::UserEvent(UiEvent::Menu(event)) => app.as_mut().map_or(Ok(false), |app| {
                    app.apply(&mailbox)?;
                    Ok(app
                        .ui
                        .as_mut()
                        .is_some_and(|ui| ui.activate(&event.id, &app.worker)))
                }),
                Event::UserEvent(UiEvent::Tray(TrayIconEvent::Click {
                    id,
                    button: MouseButton::Left | MouseButton::Right,
                    button_state: MouseButtonState::Down,
                    ..
                })) => {
                    app.as_mut().map_or(Ok(false), |app| {
                        if app.ui.as_ref().is_none_or(|ui| ui.tray.id() != &id) {
                            return Ok(false);
                        }
                        app.apply(&mailbox)?;
                        if let Some(ui) = &app.ui {
                            // Manual tracking is the boundary. Tao suppresses reentrant
                            // app callbacks while this native nested loop is active;
                            // the background client still replaces its one latest Arc.
                            ui.tray.show_menu();
                        }
                        // A dismissed menu is just as important as a selected item:
                        // reconcile topology/checks after show_menu, never on mouse leave.
                        app.apply(&mailbox)?;
                        Ok(false)
                    })
                }
                Event::LoopDestroyed => {
                    if let Some(app) = &mut app {
                        app.stop();
                    }
                    Ok(false)
                }
                _ => Ok(false),
            };
            match outcome {
                Ok(false) => {}
                Ok(true) => quitting = Some(Instant::now() + Duration::from_secs(2)),
                Err(error) => {
                    eprintln!("Synapsed tray: native UI failed: {error}");
                    quitting = Some(Instant::now() + Duration::from_secs(2));
                }
            }
            if quitting.is_some() {
                // run() does not return normally: release native UI and cancel the
                // client before selecting Exit. There is no daemon stop operation.
                if let Some(app) = &mut app {
                    app.stop();
                }
            }
        }
        if let Some(deadline) = quitting {
            let finished = app.as_ref().is_none_or(|app| app.worker.is_finished());
            if finished || Instant::now() >= deadline {
                if let Some(app) = &mut app {
                    if finished {
                        app.worker.finish();
                    } else {
                        eprintln!("Synapsed tray: client shutdown deadline reached.");
                    }
                }
                app.take();
                *control_flow = ControlFlow::Exit;
            } else {
                // Short bounded shutdown wakeups only; steady-state remains Wait.
                *control_flow = ControlFlow::WaitUntil(
                    (Instant::now() + Duration::from_millis(20)).min(deadline),
                );
            }
        }
    });
}
