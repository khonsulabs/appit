#![doc = include_str!("../README.md")]
#![warn(missing_docs, clippy::pedantic)]
#![deny(unsafe_code)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_panics_doc)] // https://github.com/rust-lang/rust-clippy/issues/11436

mod private;
mod window;

use std::collections::HashMap;
use std::process::exit;
use std::sync::{mpsc, Arc, Mutex, PoisonError};

pub use window::{RunningWindow, Window, WindowAttributes, WindowBehavior, WindowBuilder};
pub use winit;
use winit::error::{EventLoopError, OsError};
use winit::event::Event;
use winit::event_loop::{
    ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget,
};
use winit::window::WindowId;

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
        Self::new_with_event_callback(|(), _| {})
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
        self.event_loop.run(move |event, target| {
            target.set_control_flow(ControlFlow::Wait);
            match event {
                Event::WindowEvent { window_id, event } => {
                    let event = WindowEvent::from(event);
                    self.running
                        .windows
                        .send(window_id, WindowMessage::Event(event));
                }
                Event::UserEvent(message) => match message {
                    EventLoopMessage::CloseWindow(window_id) => {
                        if self.running.windows.close(window_id) {
                            exit(0)
                        }
                    }
                    EventLoopMessage::WindowPanic(window_id) => {
                        if self.running.windows.close(window_id) {
                            exit(1)
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
                | Event::MemoryWarning
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

/// A type that contains a reference to an [`Application`] implementor.
pub trait AsApplication<AppMessage> {
    /// Returns this type's application.
    fn as_application(&self) -> &dyn Application<AppMessage>
    where
        AppMessage: Message;
}

impl<T, AppMessage> AsApplication<AppMessage> for T
where
    T: Application<AppMessage>,
    AppMessage: Message,
{
    fn as_application(&self) -> &dyn Application<AppMessage>
    where
        AppMessage: Message,
    {
        self
    }
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
        window: WindowAttributes,
        sender: mpsc::SyncSender<WindowMessage<AppMessage::Window>>,
    ) -> Result<Option<Arc<winit::window::Window>>, OsError> {
        self.running
            .windows
            .open(&self.event_loop, window, sender)
            .map(Some)
    }
}

impl<AppMessage> Application<AppMessage> for App<AppMessage>
where
    AppMessage: Message,
{
    fn app(&self) -> App<AppMessage> {
        self.clone()
    }

    fn send(&mut self, message: AppMessage) -> Option<<AppMessage as Message>::Response> {
        let (response_sender, response_receiver) = mpsc::sync_channel(1);
        self.proxy
            .send_event(EventLoopMessage::User {
                message,
                response_sender,
            })
            .ok()?;
        response_receiver.recv().ok()
    }
}

impl<AppMessage> private::ApplicationSealed<AppMessage> for App<AppMessage>
where
    AppMessage: Message,
{
    fn open(
        &self,
        attrs: WindowAttributes,
        sender: mpsc::SyncSender<WindowMessage<AppMessage::Window>>,
    ) -> Result<Option<Arc<winit::window::Window>>, OsError> {
        let (open_sender, open_receiver) = mpsc::sync_channel(1);
        if self
            .proxy
            .send_event(EventLoopMessage::OpenWindow {
                attrs,
                sender,
                open_sender,
            })
            .is_err()
        {
            return Ok(None);
        }

        open_receiver.recv().map_or(Ok(None), |opt| opt.map(Some))
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
        attrs: WindowAttributes,
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
                builder = winit::platform::windows::WindowBuilderExtWindows::with_class_name(
                    builder, app_name,
                );
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
        let winit = Arc::new(builder.build(target)?);
        let mut windows = self.data.lock().map_or_else(PoisonError::into_inner, |g| g);
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
