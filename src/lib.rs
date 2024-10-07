#![doc = include_str!("../README.md")]
#![warn(missing_docs, clippy::pedantic)]
#![deny(unsafe_code)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_panics_doc)] // https://github.com/rust-lang/rust-clippy/issues/11436

mod private;
mod window;

#[cfg(all(target_os = "linux", feature = "xdg"))]
mod xdg;

use std::collections::HashMap;
use std::convert::Infallible;
use std::ops::Deref;
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::time::Duration;

use private::{OpenedWindow, WindowSpawner};
pub use window::{Run, RunningWindow, Window, WindowAttributes, WindowBehavior, WindowBuilder};
pub use winit;
use winit::application::ApplicationHandler;
use winit::error::{EventLoopError, OsError};
use winit::event::StartCause;
use winit::event_loop::{
    ActiveEventLoop, ControlFlow, EventLoop, EventLoopClosed, EventLoopProxy, OwnedDisplayHandle,
};
use winit::monitor::MonitorHandle;
use winit::window::WindowId;

use crate::private::{EventLoopMessage, WindowEvent, WindowMessage};

/// A reference to an executing application.
pub struct ExecutingApp<'a, AppMessage>(ExecutingAppHandle<'a, AppMessage>)
where
    AppMessage: Message;

impl<'a, AppMessage> ExecutingApp<'a, AppMessage>
where
    AppMessage: Message,
{
    fn new(
        windows: &'a Windows<<AppMessage as Message>::Window>,
        winit: impl Into<WinitHandle<'a, AppMessage>>,
    ) -> Self {
        Self(ExecutingAppHandle {
            windows,
            winit: winit.into(),
        })
    }

    /// Returns the list of available monitors.
    ///
    /// This function will return an empty `Vec` if invoked before the
    /// application has begun executing. This can occur if an app message is
    /// sent before a `PendingApp` is run.
    #[must_use]
    pub fn available_monitors(&self) -> Vec<MonitorHandle> {
        match &self.0.winit {
            WinitHandle::Owned(_) => Vec::new(),
            WinitHandle::Active(winit) => winit.available_monitors().collect(),
        }
    }

    /// Returns a handle to the primary monitor.
    ///
    /// This function will return None if:
    ///
    /// - The application hasn't begun executing.
    /// - The platform does not support determining a primary monitor.
    #[must_use]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        match &self.0.winit {
            WinitHandle::Owned(_) => None,
            WinitHandle::Active(winit) => winit.primary_monitor(),
        }
    }

    /// Returns a handle to the underlying display.
    #[must_use]
    pub fn owned_display_handle(&self) -> OwnedDisplayHandle {
        match &self.0.winit {
            WinitHandle::Owned(winit) => winit.owned_display_handle(),
            WinitHandle::Active(winit) => winit.owned_display_handle(),
        }
    }
}

impl<AppMessage> Deref for ExecutingApp<'_, AppMessage>
where
    AppMessage: Message,
{
    type Target = Windows<AppMessage::Window>;

    fn deref(&self) -> &Self::Target {
        self.0.windows
    }
}

struct ExecutingAppHandle<'a, AppMessage>
where
    AppMessage: Message,
{
    windows: &'a Windows<<AppMessage as Message>::Window>,
    winit: WinitHandle<'a, AppMessage>,
}

enum WinitHandle<'a, AppMessage>
where
    AppMessage: Message,
{
    Owned(&'a EventLoop<EventLoopMessage<AppMessage>>),
    Active(&'a ActiveEventLoop),
}

impl<'a, AppMessage> From<&'a ActiveEventLoop> for WinitHandle<'a, AppMessage>
where
    AppMessage: Message,
{
    fn from(handle: &'a ActiveEventLoop) -> Self {
        Self::Active(handle)
    }
}

impl<'a, AppMessage> From<&'a EventLoop<EventLoopMessage<AppMessage>>>
    for WinitHandle<'a, AppMessage>
where
    AppMessage: Message,
{
    fn from(handle: &'a EventLoop<EventLoopMessage<AppMessage>>) -> Self {
        Self::Owned(handle)
    }
}

/// An application that is not yet running.
pub struct PendingApp<AppMessage>
where
    AppMessage: Message,
{
    event_loop: EventLoop<EventLoopMessage<AppMessage>>,
    message_callback: BoxedEventCallback<AppMessage>,
    running: App<AppMessage>,
    on_startup: Vec<Box<StartupClosure<AppMessage>>>,
    pending_windows: Vec<PendingWindow<AppMessage>>,
    on_error: Option<Box<dyn FnMut(AppMessage::Error)>>,
}

struct PendingWindow<AppMessage>
where
    AppMessage: Message,
{
    window: WindowAttributes,
    sender: Arc<mpsc::SyncSender<WindowMessage<AppMessage::Window>>>,
    spawner: WindowSpawner,
}

type BoxedEventCallback<AppMessage> =
    Box<dyn FnMut(AppMessage, ExecutingApp<'_, AppMessage>) -> <AppMessage as Message>::Response>;

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
        event_callback: impl FnMut(AppMessage, ExecutingApp<'_, AppMessage>) -> AppMessage::Response
            + 'static,
    ) -> Self {
        let event_loop = EventLoop::with_user_event()
            .build()
            .expect("should be able to create an EventLoop");
        let proxy = event_loop.create_proxy();
        Self {
            event_loop,
            running: App {
                proxy,
                windows: Windows::default(),
                started: Arc::new(AtomicBool::new(false)),
            },
            message_callback: Box::new(event_callback),
            on_startup: Vec::new(),
            pending_windows: Vec::new(),
            on_error: None,
        }
    }

    /// Sets a handler that is invoked when an app receives an
    /// [`AppTypes::Error`].
    pub fn on_error<F>(&mut self, on_error: F)
    where
        F: FnMut(AppMessage::Error) + 'static,
    {
        self.on_error = Some(Box::new(on_error));
    }

    /// Executes `on_startup` once the app event loop has started.
    ///
    /// This is useful because some information provided by winit is only
    /// available after the event loop has started. For example, to enter an
    /// exclusive full screen mode, monitor information must be accessed which
    /// requires the event loop to have been started.
    pub fn on_startup<F>(&mut self, on_startup: F)
    where
        F: FnOnce(ExecutingApp<'_, AppMessage>) + Send + 'static,
    {
        self.on_startup.push(Box::new(on_startup));
    }

    /// Begins running the application.
    ///
    /// Internally this runs the [`EventLoop`].
    ///
    /// # Errors
    ///
    /// Returns an [`EventLoopError`] upon the loop exiting due to an error. See
    /// [`EventLoop::run`] for more information.
    pub fn run(self) -> Result<(), EventLoopError> {
        let Self {
            event_loop,
            message_callback,
            running,
            on_startup,
            pending_windows,
            on_error,
        } = self;

        #[cfg(all(target_os = "linux", feature = "xdg"))]
        xdg::observe_darkmode_changes(event_loop.create_proxy());

        event_loop.run_app(&mut RunningApp::<AppMessage> {
            message_callback,
            running,
            on_startup,
            pending_windows,
            on_error,
        })
    }
}

struct RunningApp<AppMessage>
where
    AppMessage: Message,
{
    message_callback: BoxedEventCallback<AppMessage>,
    running: App<AppMessage>,
    on_startup: Vec<Box<StartupClosure<AppMessage>>>,
    pending_windows: Vec<PendingWindow<AppMessage>>,
    on_error: Option<Box<dyn FnMut(AppMessage::Error)>>,
}

impl<AppMessage> ApplicationHandler<EventLoopMessage<AppMessage>> for RunningApp<AppMessage>
where
    AppMessage: Message,
{
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        let StartCause::Init = cause else {
            return;
        };
        self.running.started.store(true, Ordering::Relaxed);
        for PendingWindow {
            window,
            sender,
            spawner,
        } in self.pending_windows.drain(..)
        {
            // TODO how to handle open failure errors for pending windows?
            let window = self
                .running
                .windows
                .open(event_loop, window, sender)
                .expect("error spawning initial window");
            spawner(window);
        }
        for on_startup in self.on_startup.drain(..) {
            on_startup(ExecutingApp::new(&self.running.windows, event_loop));
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Wait);
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: winit::event::WindowEvent,
    ) {
        let (event, waiter) = WindowEvent::from_winit(event);
        self.running
            .windows
            .send(window_id, WindowMessage::Event(event));
        if let Some(waiter) = waiter {
            waiter.wait(Duration::from_millis(16));
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, message: EventLoopMessage<AppMessage>) {
        match message {
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
                spawner,
            } => {
                let result = self.running.windows.open(event_loop, attrs, sender);
                if let Ok(open) = &result {
                    spawner(open.clone());
                }
                let _result = open_sender.send(result);
            }
            EventLoopMessage::User {
                message,
                response_sender,
            } => {
                let _result = response_sender.send((self.message_callback)(
                    message,
                    ExecutingApp::new(&self.running.windows, event_loop),
                ));
            }
            EventLoopMessage::PreventShutdown => {
                self.running.windows.prevent_shutdown();
            }
            EventLoopMessage::AllowShutdown => {
                if self.running.windows.allow_shutdown() {
                    exit(0)
                }
            }
            EventLoopMessage::Error(err) => {
                if let Some(handler) = &mut self.on_error {
                    handler(err);
                }
            }
            #[cfg(all(target_os = "linux", feature = "xdg"))]
            EventLoopMessage::ThemeChanged(theme) => {
                self.running.windows.theme_changed(theme);
            }
        }
    }
}

type StartupClosure<AppMessage> = dyn FnOnce(ExecutingApp<'_, AppMessage>) + Send;

/// A reference to a multi-window application.
pub struct App<AppMessage>
where
    AppMessage: Message,
{
    proxy: EventLoopProxy<EventLoopMessage<AppMessage>>,
    windows: Windows<AppMessage::Window>,
    started: Arc<AtomicBool>,
}

impl<AppMessage> App<AppMessage>
where
    AppMessage: Message,
{
    /// Sends an app message to the main event loop to be handled by the
    /// callback provided when the app was created.
    ///
    /// This function will return None if the main event loop is no longer
    /// running. Otherwise, this function will block until the result of the
    /// callback has been received.
    pub fn send(&self, message: AppMessage) -> Option<AppMessage::Response> {
        if !self.started.load(Ordering::Relaxed) {
            return None;
        }

        let (response_sender, response_receiver) = mpsc::sync_channel(1);
        self.proxy
            .send_event(EventLoopMessage::User {
                message,
                response_sender,
            })
            .ok()?;
        response_receiver.recv().ok()
    }

    /// Sends an error to the event loop.
    ///
    /// # Errors
    ///
    /// Returns an error if the event loop is not currently running.
    pub fn send_error(
        &self,
        error: AppMessage::Error,
    ) -> Result<(), EventLoopClosed<AppMessage::Error>> {
        if !self.started.load(Ordering::Relaxed) {
            return Err(EventLoopClosed(error));
        }

        match self.proxy.send_event(EventLoopMessage::Error(error)) {
            Ok(()) => Ok(()),
            Err(EventLoopClosed(EventLoopMessage::Error(err))) => Err(EventLoopClosed(err)),
            _ => unreachable!("returned value should be the same"),
        }
    }

    /// Creates a guard that prevents this app from shutting down.
    ///
    /// If the app is not currently running, this function returns None.
    ///
    /// Once a guard is allocated the app will not be closed automatically when
    /// the final window is closed. If the final shutdown guard is dropped while
    /// no windows are open, the app will be closed.
    pub fn prevent_shutdown(&self) -> Option<ShutdownGuard<AppMessage>> {
        self.proxy
            .send_event(EventLoopMessage::PreventShutdown)
            .ok()
            .map(|()| ShutdownGuard { app: self.clone() })
    }
}

impl<AppMessage> Clone for App<AppMessage>
where
    AppMessage: Message,
{
    fn clone(&self) -> Self {
        Self {
            proxy: self.proxy.clone(),
            windows: self.windows.clone(),
            started: self.started.clone(),
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

    /// Sends an error to the event loop.
    ///
    /// # Errors
    ///
    /// Returns an error if the event loop is not currently running.
    fn send_error(
        &mut self,
        error: AppMessage::Error,
    ) -> Result<(), EventLoopClosed<AppMessage::Error>>;
}

/// A type that contains a reference to an [`Application`] implementor.
pub trait AsApplication<AppMessage> {
    /// Returns this type's application.
    fn as_application(&self) -> &dyn Application<AppMessage>
    where
        AppMessage: Message;

    /// Returns this type's application.
    fn as_application_mut(&mut self) -> &mut dyn Application<AppMessage>
    where
        AppMessage: Message;
}

impl<AppMessage> AsApplication<AppMessage> for App<AppMessage>
where
    AppMessage: Message,
{
    fn as_application(&self) -> &dyn Application<AppMessage>
    where
        AppMessage: Message,
    {
        self
    }

    fn as_application_mut(&mut self) -> &mut dyn Application<AppMessage>
    where
        AppMessage: Message,
    {
        self
    }
}

impl<AppMessage> AsApplication<AppMessage> for PendingApp<AppMessage>
where
    AppMessage: Message,
{
    fn as_application(&self) -> &dyn Application<AppMessage>
    where
        AppMessage: Message,
    {
        self
    }

    fn as_application_mut(&mut self) -> &mut dyn Application<AppMessage>
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
    /// The type that is communicated when an error occurs that the event
    /// loop/app should handle.
    type Error: Send;
}

impl Message for () {
    type Response = ();
    type Window = ();
    type Error = Infallible;
}

impl<AppMessage> Application<AppMessage> for PendingApp<AppMessage>
where
    AppMessage: Message,
{
    fn app(&self) -> App<AppMessage> {
        self.running.clone()
    }

    fn send(&mut self, message: AppMessage) -> Option<<AppMessage as Message>::Response> {
        Some((self.message_callback)(
            message,
            ExecutingApp::new(&self.running.windows, &self.event_loop),
        ))
    }

    fn send_error(
        &mut self,
        error: <AppMessage as Message>::Error,
    ) -> Result<(), EventLoopClosed<<AppMessage as Message>::Error>> {
        if let Some(on_error) = &mut self.on_error {
            on_error(error);
        }
        Ok(())
    }
}

impl<AppMessage> private::ApplicationSealed<AppMessage> for PendingApp<AppMessage>
where
    AppMessage: Message,
{
    fn open(
        &mut self,
        window: WindowAttributes,
        sender: Arc<mpsc::SyncSender<WindowMessage<AppMessage::Window>>>,
        spawner: WindowSpawner,
    ) -> Result<Option<OpenedWindow>, OsError> {
        self.pending_windows.push(PendingWindow {
            window,
            sender,
            spawner,
        });
        Ok(None)
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
        let this: &Self = self;
        this.send(message)
    }

    fn send_error(
        &mut self,
        error: <AppMessage as Message>::Error,
    ) -> Result<(), EventLoopClosed<<AppMessage as Message>::Error>> {
        let this: &Self = self;
        this.send_error(error)
    }
}

impl<AppMessage> private::ApplicationSealed<AppMessage> for App<AppMessage>
where
    AppMessage: Message,
{
    fn open(
        &mut self,
        attrs: WindowAttributes,
        sender: Arc<mpsc::SyncSender<WindowMessage<AppMessage::Window>>>,
        spawner: WindowSpawner,
    ) -> Result<Option<OpenedWindow>, OsError> {
        let (open_sender, open_receiver) = mpsc::sync_channel(1);
        if self
            .proxy
            .send_event(EventLoopMessage::OpenWindow {
                attrs,
                sender,
                open_sender,
                spawner,
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
    data: Arc<Mutex<WindowsData<Message>>>,
}

struct WindowsData<Message> {
    open: HashMap<WindowId, OpenWindow<Message>>,
    guards: usize,
}

impl<Message> WindowsData<Message> {
    fn should_shutdown(&self) -> bool {
        self.open.is_empty() && self.guards == 0
    }
}

impl<Message> Default for Windows<Message> {
    fn default() -> Self {
        Self {
            data: Arc::new(Mutex::new(WindowsData {
                open: HashMap::new(),
                guards: 0,
            })),
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
    /// Gets an instance of the winit window for the given window id, if it has
    /// been opened and is still open.
    pub fn get(&self, id: WindowId) -> Option<Arc<winit::window::Window>> {
        let windows = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        windows.open.get(&id).and_then(|w| w.winit.winit())
    }

    #[allow(unsafe_code)]
    fn open(
        &self,
        target: &ActiveEventLoop,
        attrs: WindowAttributes,
        sender: Arc<mpsc::SyncSender<WindowMessage<Message>>>,
    ) -> Result<OpenedWindow, OsError> {
        let mut builder = winit::window::WindowAttributes::default()
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

        #[cfg(any(all(target_os = "linux", feature = "wayland"), target_os = "windows"))]
        if let Some(app_name) = &attrs.app_name {
            #[cfg(all(target_os = "linux", feature = "wayland"))]
            {
                builder = winit::platform::wayland::WindowAttributesExtWayland::with_name(
                    builder, app_name, "",
                );
                builder =
                    winit::platform::x11::WindowAttributesExtX11::with_name(builder, app_name, "");
            }
            #[cfg(target_os = "windows")]
            {
                builder = winit::platform::windows::WindowAttributesExtWindows::with_class_name(
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
        let winit = Arc::new(target.create_window(builder)?);
        let id = winit.id();
        let winit = OpenedWindow(Arc::new(Mutex::new(Some(winit))));
        let mut windows = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        windows.open.insert(
            id,
            OpenWindow {
                winit: winit.clone(),
                sender,
            },
        );
        Ok(winit)
    }

    fn send(&self, window: WindowId, message: WindowMessage<Message>) {
        let mut data = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(open_window) = data.open.get(&window) {
            match open_window.sender.try_send(message) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => {
                    eprintln!("Dropping event for {window:?}.");
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    // Window no longer active, remove it.
                    data.open.remove(&window);
                }
            }
        }
    }

    fn close(&self, window: WindowId) -> bool {
        let mut data = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(closed) = data.open.remove(&window) {
            closed.winit.close();
        }
        data.should_shutdown()
    }

    fn prevent_shutdown(&self) {
        let mut data = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        data.guards += 1;
    }

    fn allow_shutdown(&self) -> bool {
        let mut data = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        data.guards -= 1;
        data.should_shutdown()
    }

    #[cfg(all(target_os = "linux", feature = "xdg"))]
    fn theme_changed(&self, theme: winit::window::Theme) {
        let data = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        for window in data.open.values() {
            let _ = window
                .sender
                .send(WindowMessage::Event(WindowEvent::ThemeChanged(theme)));
        }
    }
}

struct OpenWindow<User> {
    winit: OpenedWindow,
    sender: Arc<mpsc::SyncSender<WindowMessage<User>>>,
}

/// A guard preventing an [`App`] from shutting down.
pub struct ShutdownGuard<AppMessage>
where
    AppMessage: Message,
{
    app: App<AppMessage>,
}

impl<AppMessage> Drop for ShutdownGuard<AppMessage>
where
    AppMessage: Message,
{
    fn drop(&mut self) {
        let _ = self.app.proxy.send_event(EventLoopMessage::AllowShutdown);
    }
}
