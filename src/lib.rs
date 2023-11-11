#![doc = include_str!("../README.md")]
#![warn(missing_docs, clippy::pedantic)]
#![deny(unsafe_code)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_panics_doc)] // https://github.com/rust-lang/rust-clippy/issues/11436

mod private;
mod window;

pub use winit;

use raw_window_handle::HasRawWindowHandle;
pub use window::{RunningWindow, Window, WindowAttributes, WindowBehavior, WindowBuilder};

use winit::error::{EventLoopError, OsError};
use winit::window::WindowId;

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use winit::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use winit::{event::Event, event_loop::EventLoop};

use crate::private::{EventLoopMessage, WindowEvent, WindowMessage};

/// An application that is not yet running.
pub struct PendingApp<AppMessage>
where
    AppMessage: Message,
{
    event_loop: EventLoop<EventLoopMessage<AppMessage>>,
    message_callback: BoxedEventCallback<AppMessage>,
    running: App<AppMessage>,
}

type BoxedEventCallback<AppMessage> = Box<
    dyn FnMut(
        AppMessage,
        &Windows<<AppMessage as Message>::Window>,
    ) -> <AppMessage as Message>::Response,
>;

impl Default for PendingApp<()> {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingApp<()> {
    /// Returns a new app with no windows. If no windows are opened before the
    /// app is run, the app will immediately close.
    #[must_use]
    pub fn new() -> Self {
        Self::new_with_event_callback(|_, _| {})
    }
}

impl<AppMessage> PendingApp<AppMessage>
where
    AppMessage: Message,
{
    /// Returns a new app with no windows. If no windows are opened before the
    /// app is run, the app will immediately close.
    #[must_use]
    pub fn new_with_event_callback(
        event_callback: impl FnMut(AppMessage, &Windows<AppMessage::Window>) -> AppMessage::Response
            + 'static,
    ) -> Self {
        let event_loop = EventLoopBuilder::with_user_event()
            .build()
            .expect("should be able to create an EventLoop");
        let proxy = event_loop.create_proxy();
        Self {
            event_loop,
            running: App {
                proxy,
                windows: Windows::default(),
            },
            message_callback: Box::new(event_callback),
        }
    }

    /// Begins running the application.
    ///
    /// Internally this runs the [`EventLoop`].
    ///
    /// # Errors
    ///
    /// Returns an [`EventLoopError`] upon the loop exiting due to an error. See
    /// [`EventLoop::run`] for more information.
    pub fn run(mut self) -> Result<(), EventLoopError> {
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
                    EventLoopMessage::CloseWindow(window_id) => {
                        if self.running.windows.close(window_id) {
                            *control_flow = ControlFlow::ExitWithCode(0);
                        }
                    }
                    EventLoopMessage::WindowPanic(window_id) => {
                        if self.running.windows.close(window_id) {
                            *control_flow = ControlFlow::ExitWithCode(1);
                        }
                    }
                    EventLoopMessage::OpenWindow {
                        attrs,
                        sender,
                        open_sender,
                    } => {
                        let result = self.running.windows.open(target, attrs, sender);
                        let _result = open_sender.send(result);
                    }
                    EventLoopMessage::User {
                        message,
                        response_sender,
                    } => {
                        let _result = response_sender
                            .send((self.message_callback)(message, &self.running.windows));
                    }
                },
                Event::NewEvents(_)
                | Event::DeviceEvent { .. }
                | Event::Suspended
                | Event::Resumed
                | Event::LoopExiting
                | Event::AboutToWait => {}
            }
        })
    }
}

/// A reference to a multi-window application.
pub struct App<AppMessage>
where
    AppMessage: Message,
{
    proxy: EventLoopProxy<EventLoopMessage<AppMessage>>,
    windows: Windows<AppMessage::Window>,
}

impl<AppMessage> Clone for App<AppMessage>
where
    AppMessage: Message,
{
    fn clone(&self) -> Self {
        Self {
            proxy: self.proxy.clone(),
            windows: self.windows.clone(),
        }
    }
}

/// A type that has a handle to the application thread.
pub trait Application<AppMessage>: private::ApplicationSealed<AppMessage>
where
    AppMessage: Message,
{
    /// Returns a handle to the running application.
    fn app(&self) -> App<AppMessage>;

    /// Sends an app message to the main event loop to be handled by the
    /// callback provided when the app was created.
    ///
    /// This function will return None if the main event loop is no longer
    /// running. Otherwise, this function will block until the result of the
    /// callback has been received.
    fn send(&mut self, message: AppMessage) -> Option<AppMessage::Response>;
}

/// A message with an associated response type.
pub trait Message: Send + 'static {
    /// The message type that is able to be sent to individual windows.
    type Window: Send;
    /// The type returned when responding to this message.
    type Response: Send;
}

impl Message for () {
    type Response = ();
    type Window = ();
}

impl<AppMessage> Application<AppMessage> for PendingApp<AppMessage>
where
    AppMessage: Message,
{
    fn app(&self) -> App<AppMessage> {
        self.running.clone()
    }

    fn send(&mut self, message: AppMessage) -> Option<<AppMessage as Message>::Response> {
        Some((self.message_callback)(message, &self.running.windows))
    }
}

impl<AppMessage> private::ApplicationSealed<AppMessage> for PendingApp<AppMessage>
where
    AppMessage: Message,
{
    fn open(
        &self,
        window: WindowAttributes<AppMessage::Window>,
        sender: mpsc::SyncSender<WindowMessage<AppMessage::Window>>,
    ) -> Result<Option<Arc<winit::window::Window>>, OsError> {
        self.running
            .windows
            .open(&self.event_loop, window, sender)
            .map(Some)
    }
}

/// A collection of open windows.
pub struct Windows<Message> {
    data: Arc<Mutex<HashMap<WindowId, OpenWindow<Message>>>>,
}

impl<Message> Default for Windows<Message> {
    fn default() -> Self {
        Self {
            data: Arc::default(),
        }
    }
}

impl<Message> Clone for Windows<Message> {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
        }
    }
}

impl<Message> Windows<Message> {
    /// Gets an instance of the winit window for the given window id, if it is
    /// still open.
    pub fn get(&self, id: WindowId) -> Option<Arc<winit::window::Window>> {
        let windows = self.data.lock().map_or_else(PoisonError::into_inner, |g| g);
        windows.get(&id).map(|w| w.winit.clone())
    }

    #[allow(unsafe_code)]
    fn open<AppMessage>(
        &self,
        target: &EventLoopWindowTarget<EventLoopMessage<AppMessage>>,
        attrs: WindowAttributes<Message>,
        sender: mpsc::SyncSender<WindowMessage<Message>>,
    ) -> Result<Arc<winit::window::Window>, OsError>
    where
        AppMessage: crate::Message<Window = Message>,
    {
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

        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if let Some(app_name) = &attrs.app_name {
            #[cfg(target_os = "linux")]
            {
                builder = winit::platform::wayland::WindowBuilderExtWayland::with_name(
                    builder, app_name, "",
                );
                builder =
                    winit::platform::x11::WindowBuilderExtX11::with_name(builder, app_name, "");
            }
            #[cfg(target_os = "windows")]
            {
                builder =
                    platform::windows::WindowBuilderExtWindows::with_name(builder, app_name, "");
            }
        }

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

    fn send(&self, window: WindowId, message: WindowMessage<Message>) {
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

    fn close(&self, window: WindowId) -> bool {
        let mut data = self.data.lock().map_or_else(PoisonError::into_inner, |g| g);
        data.remove(&window);
        data.is_empty()
    }
}

struct OpenWindow<User> {
    winit: Arc<winit::window::Window>,
    sender: mpsc::SyncSender<WindowMessage<User>>,
}
