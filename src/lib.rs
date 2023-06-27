#![doc = include_str!("../README.md")]
#![warn(missing_docs, clippy::pedantic)]
#![deny(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

mod private;
mod window;

use raw_window_handle::HasRawWindowHandle;
pub use window::{RunningWindow, Window, WindowBehavior, WindowBuilder};

use winit::error::OsError;
use winit::window::WindowId;

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use winit::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use winit::{event::Event, event_loop::EventLoop};

use crate::private::{AppMessage, WindowEvent, WindowMessage};
use crate::window::WindowAttributes;

/// An application that is not yet running.
pub struct PendingApp {
    event_loop: EventLoop<AppMessage>,
    running: App,
}

impl Default for PendingApp {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingApp {
    /// Returns a new app with no windows. If no windows are opened before the
    /// app is run, the app will immediately close.
    #[must_use]
    pub fn new() -> Self {
        let event_loop = EventLoopBuilder::with_user_event().build();
        let proxy = event_loop.create_proxy();
        Self {
            event_loop,
            running: App {
                proxy,
                windows: Windows::default(),
            },
        }
    }

    /// Begins running the application. This function will never return.
    ///
    /// Internally this runs the [`EventLoop`].
    pub fn run(self) -> ! {
        self.event_loop.run(move |event, target, control_flow| {
            *control_flow = ControlFlow::Wait;
            match event {
                Event::WindowEvent { window_id, event } => {
                    let event = WindowEvent::from(event);
                    self.running
                        .windows
                        .send(window_id, WindowMessage::Event(event));
                }
                Event::RedrawRequested(window_id) => {
                    self.running.windows.send(window_id, WindowMessage::Redraw);
                }
                Event::UserEvent(message) => match message {
                    AppMessage::CloseWindow(window_id) => {
                        if self.running.windows.close(window_id) {
                            *control_flow = ControlFlow::ExitWithCode(0);
                        }
                    }
                    AppMessage::OpenWindow {
                        attrs,
                        sender,
                        open_sender,
                    } => {
                        let result = self.running.windows.open(target, attrs, sender);
                        let _result = open_sender.send(result);
                    }
                },
                Event::NewEvents(_)
                | Event::DeviceEvent { .. }
                | Event::Suspended
                | Event::Resumed
                | Event::MainEventsCleared
                | Event::RedrawEventsCleared
                | Event::LoopDestroyed => {}
            }
        });
    }
}

/// A reference to a multi-window application.
#[derive(Clone)]
pub struct App {
    proxy: EventLoopProxy<AppMessage>,
    windows: Windows,
}

/// A type that has a handle to the application thread.
pub trait Application: private::ApplicationSealed {}

impl Application for PendingApp {}

impl private::ApplicationSealed for PendingApp {
    fn app(&self) -> App {
        self.running.clone()
    }

    fn open(
        &self,
        window: WindowAttributes,
        sender: mpsc::SyncSender<WindowMessage>,
    ) -> Result<Option<Arc<winit::window::Window>>, OsError> {
        self.running
            .windows
            .open(&self.event_loop, window, sender)
            .map(Some)
    }
}

#[derive(Default, Clone)]
struct Windows {
    data: Arc<Mutex<HashMap<WindowId, OpenWindow>>>,
}

impl Windows {
    fn get(&self, id: WindowId) -> Option<Arc<winit::window::Window>> {
        let windows = self.data.lock().map_or_else(PoisonError::into_inner, |g| g);
        windows.get(&id).map(|w| w.winit.clone())
    }

    #[allow(unsafe_code)]
    fn open(
        &self,
        target: &EventLoopWindowTarget<AppMessage>,
        attrs: WindowAttributes,
        sender: mpsc::SyncSender<WindowMessage>,
    ) -> Result<Arc<winit::window::Window>, OsError> {
        let mut builder = winit::window::WindowBuilder::new()
            .with_active(attrs.active)
            .with_resizable(attrs.resizable)
            .with_enabled_buttons(attrs.enabled_buttons)
            .with_title(attrs.title)
            .with_maximized(attrs.maximized)
            .with_visible(attrs.visible)
            .with_transparent(attrs.transparent)
            .with_decorations(attrs.decorations)
            .with_window_level(attrs.window_level)
            .with_content_protected(attrs.content_protected)
            .with_fullscreen(attrs.fullscreen)
            .with_window_icon(attrs.window_icon)
            .with_theme(attrs.preferred_theme);

        if let Some(inner_size) = attrs.inner_size {
            builder = builder.with_inner_size(inner_size);
        }
        if let Some(min_inner_size) = attrs.min_inner_size {
            builder = builder.with_min_inner_size(min_inner_size);
        }
        if let Some(max_inner_size) = attrs.max_inner_size {
            builder = builder.with_max_inner_size(max_inner_size);
        }
        if let Some(position) = attrs.position {
            builder = builder.with_position(position);
        }
        if let Some(resize_increments) = attrs.resize_increments {
            builder = builder.with_resize_increments(resize_increments);
        }
        let mut windows = self.data.lock().map_or_else(PoisonError::into_inner, |g| g);
        if let Some(parent_window) = attrs.parent_window {
            let parent_window = windows
                .get(&parent_window.id())
                .expect("invalid parent window");
            // SAFETY: The only way for us to resolve to a winit Window is for
            // the window to still be in our list of open windows. This
            // guarantees that the window handle is still valid.
            unsafe {
                builder = builder.with_parent_window(Some(parent_window.winit.raw_window_handle()));
            }
        }
        let winit = Arc::new(builder.build(target)?);
        windows.insert(
            winit.id(),
            OpenWindow {
                winit: winit.clone(),
                sender,
            },
        );
        Ok(winit)
    }

    pub fn send(&self, window: WindowId, message: WindowMessage) {
        let mut data = self.data.lock().map_or_else(PoisonError::into_inner, |g| g);
        if let Some(open_window) = data.get(&window) {
            match open_window.sender.try_send(message) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => {
                    eprintln!("Dropping event for {window:?}.");
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    // Window no longer active, remove it.
                    data.remove(&window);
                }
            }
        }
    }

    pub fn close(&self, window: WindowId) -> bool {
        let mut data = self.data.lock().map_or_else(PoisonError::into_inner, |g| g);
        data.remove(&window);
        data.is_empty()
    }
}

struct OpenWindow {
    winit: Arc<winit::window::Window>,
    sender: mpsc::SyncSender<WindowMessage>,
}
