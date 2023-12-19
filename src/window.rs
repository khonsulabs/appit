use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::panic::{AssertUnwindSafe, UnwindSafe};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use winit::dpi::{PhysicalPosition, PhysicalSize, Position, Size};
use winit::error::{EventLoopError, OsError};
use winit::event::{
    AxisId, DeviceId, ElementState, Ime, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, Touch,
    TouchPhase,
};
use winit::keyboard::PhysicalKey;
use winit::window::{Fullscreen, Icon, Theme, WindowButtons, WindowId, WindowLevel};

use crate::private::{self, WindowEvent};
use crate::{App, Application, EventLoopMessage, Message, PendingApp, WindowMessage, Windows};

/// A weak reference to a running window.
#[derive(Debug, Clone)]
pub struct Window<Message> {
    id: WindowId,
    sender: mpsc::SyncSender<WindowMessage<Message>>,
}

impl<Message> Window<Message> {
    /// Returns the winit id of the window.
    #[must_use]
    pub const fn id(&self) -> WindowId {
        self.id
    }

    /// Sends a message to the window.
    ///
    /// Returns `Ok` if the message was successfully sent. The message may not
    /// be received even if this function returns `Ok`, if the window closes
    /// between when the message was sent and when the message is received.
    ///
    /// # Errors
    ///
    /// If the window is already closed, this function returns `Err(message)`.
    pub fn send(&self, message: Message) -> Result<(), Message> {
        match self.sender.send(WindowMessage::User(message)) {
            Ok(()) => Ok(()),
            Err(mpsc::SendError(WindowMessage::User(message))) => Err(message),
            _ => unreachable!("same input as output"),
        }
    }
}

/// A builder for a window.
///
/// This type is similar to winit's
/// [`WindowBuilder`](winit::window::WindowBuilder), except that it only
/// supports the cross-platform interface. Support for additional
/// platform-specific settings may be possible as long as all types introduced
/// are `Send`.
pub struct WindowBuilder<'a, Behavior, Application, AppMessage>
where
    Behavior: self::WindowBehavior<AppMessage>,
    AppMessage: Message,
{
    owner: &'a Application,
    context: Behavior::Context,
    attributes: WindowAttributes,
}
impl<'a, Behavior, Application, AppMessage> Deref
    for WindowBuilder<'a, Behavior, Application, AppMessage>
where
    Behavior: self::WindowBehavior<AppMessage>,
    AppMessage: Message,
{
    type Target = WindowAttributes;

    fn deref(&self) -> &Self::Target {
        &self.attributes
    }
}

impl<'a, Behavior, Application, AppMessage> DerefMut
    for WindowBuilder<'a, Behavior, Application, AppMessage>
where
    Behavior: self::WindowBehavior<AppMessage>,
    AppMessage: Message,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.attributes
    }
}

/// Attributes of a desktop window.
///
/// This structure is equivalent to [`winit::window::WindowAttributes`] except
/// that `parent_window` accepts a [`Window`] rather than relying on raw window
/// handle.
#[allow(clippy::struct_excessive_bools)]
pub struct WindowAttributes {
    /// The inner size of the window.
    pub inner_size: Option<Size>,
    /// The minimum inner size of the window.
    pub min_inner_size: Option<Size>,
    /// The maximum inner size of the window.
    pub max_inner_size: Option<Size>,
    /// The position of the top-left of the frame of the window.
    pub position: Option<Position>,
    /// If true, the window can be resized by the user.
    pub resizable: bool,
    /// The collection of window buttons that are enabled.
    pub enabled_buttons: WindowButtons,
    /// The title of the window.
    pub title: String,
    /// The full screen configuration for the window.
    pub fullscreen: Option<Fullscreen>,
    /// The maximized state of the window.
    pub maximized: bool,
    /// The visibility state of the window.
    pub visible: bool,
    /// If true, the window's chrome will be hidden and only areas that have
    /// been drawn to will be opaque.
    pub transparent: bool,
    /// Controls the visibility of the window decorations.
    pub decorations: bool,
    /// The window's icon.
    pub window_icon: Option<Icon>,
    /// The window's preferred theme.
    pub preferred_theme: Option<Theme>,
    /// The increments in which the window will be allowed to resize by the user.
    pub resize_increments: Option<Size>,
    /// If true, the contents of the window will be prevented from being
    /// captured by other applications when supported.
    pub content_protected: bool,
    /// The level of the window.
    pub window_level: WindowLevel,
    /// Whether the window is active or not.
    pub active: bool,
    /// Name of the application
    ///
    /// - `WM_CLASS` on X11
    /// - application ID on wayland
    /// - class name on windows
    #[doc(alias("app_id", "class", "class_name"))]
    pub app_name: Option<String>,
}

impl Default for WindowAttributes {
    fn default() -> Self {
        let defaults = winit::window::WindowAttributes::default();
        let fullscreen = defaults.fullscreen().cloned();
        Self {
            inner_size: defaults.inner_size,
            min_inner_size: defaults.min_inner_size,
            max_inner_size: defaults.max_inner_size,
            position: defaults.position,
            resizable: defaults.resizable,
            enabled_buttons: defaults.enabled_buttons,
            title: defaults.title,
            fullscreen,
            maximized: defaults.maximized,
            visible: defaults.visible,
            transparent: defaults.transparent,
            decorations: defaults.decorations,
            window_icon: defaults.window_icon,
            preferred_theme: defaults.preferred_theme,
            resize_increments: defaults.resize_increments,
            content_protected: defaults.content_protected,
            window_level: defaults.window_level,
            active: defaults.active,
            app_name: None,
        }
    }
}

impl<'a, Behavior, Application, AppMessage> WindowBuilder<'a, Behavior, Application, AppMessage>
where
    Behavior: self::WindowBehavior<AppMessage>,
    Application: crate::Application<AppMessage>,
    AppMessage: Message,
{
    pub(crate) fn new(owner: &'a Application, context: Behavior::Context) -> Self {
        Self {
            owner,
            context,
            attributes: WindowAttributes::default(),
        }
    }

    /// Opens the window, if the application is still running or has not started
    /// running. The events of the window will be processed in a thread spawned
    /// by this function.
    ///
    /// If the application has shut down, this function returns None.
    ///
    /// # Errors
    ///
    /// The only errors this funciton can return arise from
    /// [`winit::window::WindowBuilder::build`].
    pub fn open(self) -> Result<Option<Window<AppMessage::Window>>, winit::error::OsError> {
        // The window's thread shouldn't ever block for long periods of time. To
        // avoid a "frozen" window causing massive memory allocations, we'll use
        // a fixed-size channel and be cautious to not block the main event loop
        // by always using try_send.
        let (sender, receiver) = mpsc::sync_channel(65536);
        let Some(winit) = self.owner.open(self.attributes, sender.clone())? else {
            return Ok(None);
        };
        let window = Window {
            id: winit.id(),
            sender: sender.clone(),
        };
        let running_window = RunningWindow {
            messages: (sender, receiver),
            responses: mpsc::sync_channel(1),
            app: self.owner.app(),
            occluded: winit.is_visible().unwrap_or(false),
            focused: winit.has_focus(),
            inner_size: winit.inner_size(),
            position: winit.inner_position().unwrap_or_default(),
            scale: winit.scale_factor(),
            theme: winit.theme().unwrap_or(Theme::Dark),
            window: winit,
            next_redraw_target: None,
            close: false,
            modifiers: Modifiers::default(),
            cursor_position: None,
            mouse_buttons: HashSet::default(),
            keys: HashSet::default(),
        };

        thread::spawn(move || running_window.run_with::<Behavior>(self.context));

        Ok(Some(window))
    }
}

type SyncChannel<T> = (mpsc::SyncSender<T>, mpsc::Receiver<T>);

/// A window that is running in its own thread.
pub struct RunningWindow<AppMessage>
where
    AppMessage: Message,
{
    window: Arc<winit::window::Window>,
    next_redraw_target: Option<RedrawTarget>,
    messages: SyncChannel<WindowMessage<AppMessage::Window>>,
    responses: SyncChannel<AppMessage::Response>,
    app: App<AppMessage>,
    inner_size: PhysicalSize<u32>,
    position: PhysicalPosition<i32>,
    cursor_position: Option<PhysicalPosition<f64>>,
    mouse_buttons: HashSet<MouseButton>,
    keys: HashSet<PhysicalKey>,
    scale: f64,
    close: bool,
    occluded: bool,
    focused: bool,
    theme: Theme,
    modifiers: Modifiers,
}

impl<AppMessage> RunningWindow<AppMessage>
where
    AppMessage: Message,
{
    /// Returns a reference to the underlying window.
    #[must_use]
    pub fn winit(&self) -> &winit::window::Window {
        &self.window
    }

    /// Returns a handle to this window.
    #[must_use]
    pub fn handle(&self) -> Window<AppMessage::Window> {
        Window {
            id: self.window.id(),
            sender: self.messages.0.clone(),
        }
    }

    /// Returns the target for when the window will be redrawn.
    #[must_use]
    pub const fn next_redraw_target(&self) -> Option<RedrawTarget> {
        self.next_redraw_target
    }

    /// Sets the window to redraw as soon as it can.
    pub fn set_needs_redraw(&mut self) {
        self.next_redraw_target = Some(RedrawTarget::Immediate);
    }

    /// Sets the window to redraw at the provided time.
    ///
    /// If the window is already set to redraw sooner, this function does
    /// nothing.
    pub fn redraw_at(&mut self, instant: Instant) {
        // Make sure this new scheduled time isn't further out than our current target.
        match self.next_redraw_target {
            Some(RedrawTarget::Immediate) => return,
            Some(RedrawTarget::Scheduled(at)) => {
                if at < instant {
                    return;
                }
            }
            None => {}
        }

        self.next_redraw_target = Some(RedrawTarget::Scheduled(instant));
    }

    /// Sets the window to redraw after a `duration`.
    ///
    /// If the window is already set to redraw sooner, this function does
    /// nothing.
    pub fn redraw_in(&mut self, duration: Duration) {
        self.redraw_at(Instant::now() + duration);
    }

    /// Returns the current title of the window.
    #[must_use]
    pub fn title(&self) -> String {
        self.window.title()
    }

    /// Sets the window's title to `new_title`.
    pub fn set_title(&self, new_title: &str) {
        self.window.set_title(new_title);
    }

    /// Sets the window's minimum inner size.
    pub fn set_min_inner_size(&self, min_size: Option<PhysicalSize<u32>>) {
        self.window.set_min_inner_size(min_size);
    }

    /// Sets the window's maximum inner size.
    pub fn set_max_inner_size(&self, max_size: Option<PhysicalSize<u32>>) {
        self.window.set_max_inner_size(max_size);
    }

    /// Returns the current size of the interior of the window, in pixels.
    #[must_use]
    pub const fn inner_size(&self) -> PhysicalSize<u32> {
        self.inner_size
    }

    /// Sets the inner size of the window, in pixels.
    pub fn set_inner_size(&self, new_size: PhysicalSize<u32>) {
        // TODO not sure if this is reasonable
        self.window.set_min_inner_size(Some(new_size));
        self.window.set_max_inner_size(Some(new_size));
    }

    /// Returns the current locpositionation of the window, in pixels.
    #[must_use]
    pub const fn position(&self) -> PhysicalPosition<i32> {
        self.position
    }

    /// Sets the current position of the window, in pixels.
    pub fn set_position(&self, new_position: PhysicalPosition<i32>) {
        self.window.set_outer_position(new_position);
    }

    /// Returns the position of the cursor relative to the window's upper-left
    /// corner, in pixels.
    #[must_use]
    pub const fn cursor_position(&self) -> Option<PhysicalPosition<f64>> {
        self.cursor_position
    }

    /// Returns the current scale factor for the window.
    #[must_use]
    pub const fn scale(&self) -> f64 {
        self.scale
    }

    /// Returns true if the window is currently invisible, hidden behind other
    /// windows, minimized, or otherwise hidden from the user's view.
    #[must_use]
    pub const fn occluded(&self) -> bool {
        self.occluded
    }

    /// Returns true if the window is currently focused for keyboard input.
    #[must_use]
    pub const fn focused(&self) -> bool {
        self.focused
    }

    /// Returns the current theme of the window.
    #[must_use]
    pub const fn theme(&self) -> Theme {
        self.theme
    }

    /// Returns the current state of the keyboard modifier keys.
    #[must_use]
    pub const fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    fn run_with<Behavior>(mut self, context: Behavior::Context)
    where
        Behavior: self::WindowBehavior<AppMessage>,
    {
        let proxy = self.app.proxy.clone();
        let window_id = self.window.id();
        let possible_panic = std::panic::catch_unwind(AssertUnwindSafe(move || {
            let mut behavior = Behavior::initialize(&mut self, context);
            while !self.close && self.process_messages_until_redraw(&mut behavior) {
                self.next_redraw_target = None;
                #[allow(unsafe_code)]
                unsafe {
                    // SAFETY: `behaviour` is dropped after this block, and `self`
                    //         outlives it.
                    behavior.redraw(&mut self);
                }
            }
            // Do not notify the main thread to close the window until after the
            // behavior is dropped. This upholds the requirement for RawWindowHandle
            // by making sure that any resources required by the behavior have had a
            // chance to be freed.
        }));

        //
        if let Err(panic) = possible_panic {
            let _result = proxy.send_event(EventLoopMessage::WindowPanic(window_id));
            std::panic::resume_unwind(panic)
        } else {
            let _result = proxy.send_event(EventLoopMessage::CloseWindow(window_id));
        }
    }

    fn process_messages_until_redraw<Behavior>(&mut self, behavior: &mut Behavior) -> bool
    where
        Behavior: self::WindowBehavior<AppMessage>,
    {
        loop {
            let message = match TimeUntilRedraw::from(self.next_redraw_target) {
                // The scheduled redraw time has already elapsed, or we need to
                // redraw. Process messages that are already enqueued, but don't
                // block.
                TimeUntilRedraw::None => match self.messages.1.try_recv() {
                    Ok(message) => message,
                    Err(mpsc::TryRecvError::Disconnected) => return false,
                    Err(mpsc::TryRecvError::Empty) => return true,
                },
                // We have a scheduled time for the next frame, and it hasn't
                // elapsed yet.
                TimeUntilRedraw::Some(duration_remaining) => {
                    match self.messages.1.recv_timeout(duration_remaining) {
                        Ok(message) => message,
                        Err(mpsc::RecvTimeoutError::Timeout) => return true,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return false,
                    }
                }
                // No scheduled redraw time, sleep until the next message.
                TimeUntilRedraw::Indefinite => match self.messages.1.recv() {
                    Ok(message) => message,
                    Err(_) => return false,
                },
            };

            if !self.handle_message(message, behavior) {
                break false;
            }
        }
    }

    #[allow(clippy::too_many_lines)] // can't avoid the match
    fn handle_message<Behavior>(
        &mut self,
        message: WindowMessage<AppMessage::Window>,
        behavior: &mut Behavior,
    ) -> bool
    where
        Behavior: self::WindowBehavior<AppMessage>,
    {
        match message {
            WindowMessage::User(user) => behavior.event(self, user),
            WindowMessage::Event(evt) => match evt {
                WindowEvent::RedrawRequested => {
                    self.set_needs_redraw();
                }
                WindowEvent::CloseRequested => {
                    if behavior.close_requested(self) {
                        self.close();
                    }
                }
                WindowEvent::Focused(focused) => {
                    self.focused = focused;
                    behavior.focus_changed(self);
                }
                WindowEvent::Occluded(occluded) => {
                    self.occluded = occluded;
                    behavior.occlusion_changed(self);
                }
                WindowEvent::ScaleFactorChanged { scale_factor } => {
                    // Ensure both values are updated before any behavior
                    // callbacks are invoked.
                    self.scale = scale_factor;
                    let new_inner_size = self.window.inner_size();
                    let inner_size_changed = self.inner_size != new_inner_size;
                    self.inner_size = new_inner_size;
                    behavior.scale_factor_changed(self);
                    if inner_size_changed {
                        behavior.resized(self);
                    }
                }
                WindowEvent::Resized(new_inner_size) => {
                    if self.inner_size != new_inner_size {
                        self.inner_size = new_inner_size;
                        behavior.resized(self);
                    }
                }
                WindowEvent::Moved(position) => {
                    self.position = position;
                }
                WindowEvent::Destroyed => {
                    return false;
                }
                WindowEvent::ThemeChanged(theme) => {
                    self.theme = theme;
                    behavior.theme_changed(self);
                }
                WindowEvent::DroppedFile(path) => {
                    behavior.dropped_file(self, path);
                }
                WindowEvent::HoveredFile(path) => {
                    behavior.hovered_file(self, path);
                }
                WindowEvent::HoveredFileCancelled => {
                    behavior.hovered_file_cancelled(self);
                }
                WindowEvent::ReceivedCharacter(char) => {
                    behavior.received_character(self, char);
                }
                WindowEvent::KeyboardInput {
                    device_id,
                    event,
                    is_synthetic,
                } => {
                    match event.state {
                        ElementState::Pressed => {
                            self.keys.insert(event.physical_key);
                        }
                        ElementState::Released => {
                            self.keys.remove(&event.physical_key);
                        }
                    }
                    behavior.keyboard_input(self, device_id, event, is_synthetic);
                }
                WindowEvent::ModifiersChanged(modifiers) => {
                    self.modifiers = modifiers;
                    behavior.modifiers_changed(self);
                }
                WindowEvent::Ime(ime) => {
                    behavior.ime(self, ime);
                }
                WindowEvent::CursorMoved {
                    device_id,
                    position,
                } => {
                    self.cursor_position = Some(position);
                    behavior.cursor_moved(self, device_id, position);
                }
                WindowEvent::CursorEntered { device_id } => {
                    behavior.cursor_entered(self, device_id);
                }
                WindowEvent::CursorLeft { device_id } => {
                    self.cursor_position = None;
                    behavior.cursor_left(self, device_id);
                }
                WindowEvent::MouseWheel {
                    device_id,
                    delta,
                    phase,
                } => {
                    behavior.mouse_wheel(self, device_id, delta, phase);
                }
                WindowEvent::MouseInput {
                    device_id,
                    state,
                    button,
                } => {
                    match state {
                        ElementState::Pressed => {
                            self.mouse_buttons.insert(button);
                        }
                        ElementState::Released => {
                            self.mouse_buttons.remove(&button);
                        }
                    }
                    behavior.mouse_input(self, device_id, state, button);
                }
                WindowEvent::TouchpadPressure {
                    device_id,
                    pressure,
                    stage,
                } => {
                    behavior.touchpad_pressure(self, device_id, pressure, stage);
                }
                WindowEvent::AxisMotion {
                    device_id,
                    axis,
                    value,
                } => {
                    behavior.axis_motion(self, device_id, axis, value);
                }
                WindowEvent::Touch(touch) => {
                    behavior.touch(self, touch);
                }
                WindowEvent::TouchpadMagnify {
                    device_id,
                    delta,
                    phase,
                } => {
                    behavior.touchpad_magnify(self, device_id, delta, phase);
                }
                WindowEvent::SmartMagnify { device_id } => {
                    behavior.smart_magnify(self, device_id);
                }
                WindowEvent::TouchpadRotate {
                    device_id,
                    delta,
                    phase,
                } => {
                    behavior.touchpad_rotate(self, device_id, delta, phase);
                }
                WindowEvent::ActivationTokenDone { .. } => todo!(),
            },
        }

        true
    }

    /// Sets this window to close as soon as possible.
    pub fn close(&mut self) {
        self.close = true;
        self.set_needs_redraw();
    }

    /// Returns an iterator of the currently pressed keys.
    ///
    /// This iterator does not guarantee any specific order.
    pub fn pressed_keys(&self) -> impl Iterator<Item = PhysicalKey> + '_ {
        self.keys.iter().copied()
    }

    /// Returns true if the given key code is currently pressed.
    #[must_use]
    pub fn key_pressed(&self, key: &PhysicalKey) -> bool {
        self.keys.contains(key)
    }

    /// Returns an iterator of the currently pressed mouse buttons.
    ///
    /// This iterator does not guarantee any specific order.
    pub fn pressed_mouse_buttons(&self) -> impl Iterator<Item = MouseButton> + '_ {
        self.mouse_buttons.iter().copied()
    }

    /// Returns true if the button is currently pressed.
    #[must_use]
    pub fn mouse_button_pressed(&self, button: &MouseButton) -> bool {
        self.mouse_buttons.contains(button)
    }
}

impl<AppMessage> Application<AppMessage> for RunningWindow<AppMessage>
where
    AppMessage: Message,
{
    fn app(&self) -> App<AppMessage> {
        self.app.clone()
    }

    fn send(&mut self, message: AppMessage) -> Option<<AppMessage as Message>::Response> {
        self.app
            .proxy
            .send_event(EventLoopMessage::User {
                message,
                response_sender: self.responses.0.clone(),
            })
            .ok()?;
        self.responses.1.recv().ok()
    }
}

impl<AppMessage> private::ApplicationSealed<AppMessage> for RunningWindow<AppMessage>
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
            .app
            .proxy
            .send_event(EventLoopMessage::OpenWindow {
                attrs,
                sender,
                open_sender,
            })
            .is_ok()
        {
            if let Ok(window) = open_receiver.recv() {
                return window.map(Some);
            }
        }

        Ok(None)
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum RedrawTarget {
    Immediate,
    Scheduled(Instant),
}

impl From<Option<RedrawTarget>> for TimeUntilRedraw {
    fn from(value: Option<RedrawTarget>) -> Self {
        match value {
            Some(RedrawTarget::Immediate) => TimeUntilRedraw::None,
            Some(RedrawTarget::Scheduled(at)) => match at.checked_duration_since(Instant::now()) {
                Some(remaining) if !remaining.is_zero() => TimeUntilRedraw::Some(remaining),
                _ => TimeUntilRedraw::None,
            },
            None => Self::Indefinite,
        }
    }
}

#[derive(Debug)]
enum TimeUntilRedraw {
    None,
    Some(Duration),
    Indefinite,
}

/// The behavior that drives the contents of a window.
///
/// With winit and appit, the act of populating the window is up to the
/// consumers of the libraries. This trait provides functions for each of the
/// events a window may receive, enabling the type to react and update its
/// state.
pub trait WindowBehavior<AppMessage>: UnwindSafe + Sized + 'static
where
    AppMessage: Message,
{
    /// A type that is passed to [`initialize()`](Self::initialize).
    ///
    /// This allows providing data to the window from the thread that is opening
    /// the window without requiring that `WindowBehavior` also be `Send`.
    type Context: Send + UnwindSafe;

    /// Returns a new window builder for this behavior. When the window is
    /// initialized, a default [`Context`](Self::Context) will be passed.
    fn build<App>(app: &App) -> WindowBuilder<'_, Self, App, AppMessage>
    where
        App: Application<AppMessage>,
        Self::Context: Default,
    {
        Self::build_with(app, <Self::Context as Default>::default())
    }

    /// Returns a new window builder for this behavior. When the window is
    /// initialized, the provided context will be passed.
    fn build_with<App>(
        app: &App,
        context: Self::Context,
    ) -> WindowBuilder<'_, Self, App, AppMessage>
    where
        App: Application<AppMessage>,
    {
        WindowBuilder::new(app, context)
    }

    /// Runs a window with a default instance of this behavior's
    /// [`Context`](Self::Context).
    ///
    /// This function is shorthand for creating a [`PendingApp`], opening this
    /// window inside of it, and running the pending app.
    ///
    /// Messages can be sent to the application's main thread using
    /// [`Application::send`]. Each time a message is received by the main event
    /// loop, `app_callback` will be invoked.
    ///
    /// # Errors
    ///
    /// Returns an [`EventLoopError`] upon the loop exiting due to an error. See
    /// [`EventLoop::run`] for more information.
    fn run_with_event_callback(
        app_callback: impl FnMut(AppMessage, &Windows<AppMessage::Window>) -> AppMessage::Response
            + 'static,
    ) -> Result<(), EventLoopError>
    where
        Self::Context: Default,
    {
        let app = PendingApp::new_with_event_callback(app_callback);
        Self::open(&app).expect("error opening initial window");
        app.run()
    }

    /// Runs a window with the provided [`Context`](Self::Context).
    ///
    /// This function is shorthand for creating a [`PendingApp`], opening this
    /// window inside of it, and running the pending app.
    ///
    /// Messages can be sent to the application's main thread using
    /// [`Application::send`]. Each time a message is received by the main event
    /// loop, `app_callback` will be invoked.
    ///
    /// # Errors
    ///
    /// Returns an [`EventLoopError`] upon the loop exiting due to an error. See
    /// [`EventLoop::run`] for more information.
    fn run_with_context_and_event_callback(
        context: Self::Context,
        app_callback: impl FnMut(AppMessage, &Windows<AppMessage::Window>) -> AppMessage::Response
            + 'static,
    ) -> Result<(), EventLoopError> {
        let app = PendingApp::new_with_event_callback(app_callback);
        Self::open_with(&app, context).expect("error opening initial window");
        app.run()
    }

    /// Opens a new window with a default instance of this behavior's
    /// [`Context`](Self::Context). The events of the window will be processed
    /// in a thread spawned by this function.
    ///
    /// If the application has shut down, this function returns None.
    ///
    /// # Errors
    ///
    /// The only errors this funciton can return arise from
    /// [`winit::window::WindowBuilder::build`].
    fn open<App>(app: &App) -> Result<Option<Window<AppMessage::Window>>, OsError>
    where
        App: Application<AppMessage>,
        Self::Context: Default,
    {
        Self::build(app).open()
    }

    /// Opens a new window with the provided [`Context`](Self::Context). The
    /// events of the window will be processed in a thread spawned by this
    /// function.
    ///
    /// If the application has shut down, this function returns None.
    ///
    /// # Errors
    ///
    /// The only errors this funciton can return arise from
    /// [`winit::window::WindowBuilder::build`].
    fn open_with<App>(
        app: &App,
        context: Self::Context,
    ) -> Result<Option<Window<AppMessage::Window>>, OsError>
    where
        App: Application<AppMessage>,
    {
        Self::build_with(app, context).open()
    }

    /// Returns a new instance of this behavior after initializing itself with
    /// the window and context.
    fn initialize(window: &mut RunningWindow<AppMessage>, context: Self::Context) -> Self;

    /// Displays the contents of the window.
    /// 
    /// SAFETY: The caller ensures that `window` outlives this `WindowBehaviour`
    #[allow(unsafe_code)]
    unsafe fn redraw(&mut self, window: &mut RunningWindow<AppMessage>);

    /// The window has been requested to be closed. This can happen as a result
    /// of the user clicking the close button.
    ///
    /// If the window should be closed, return true. To prevent closing the
    /// window, return false.
    #[allow(unused_variables)]
    fn close_requested(&mut self, window: &mut RunningWindow<AppMessage>) -> bool {
        true
    }

    /// The window has gained or lost keyboard focus.
    /// [`RunningWindow::focused()`] returns the current state.
    #[allow(unused_variables)]
    fn focus_changed(&mut self, window: &mut RunningWindow<AppMessage>) {}

    /// The window has been occluded or revealed. [`RunningWindow::occluded()`]
    /// returns the current state.
    #[allow(unused_variables)]
    fn occlusion_changed(&mut self, window: &mut RunningWindow<AppMessage>) {}

    /// The window's scale factor has changed. [`RunningWindow::scale()`]
    /// returns the current scale.
    #[allow(unused_variables)]
    fn scale_factor_changed(&mut self, window: &mut RunningWindow<AppMessage>) {}

    /// The window has been resized. [`RunningWindow::inner_size()`]
    /// returns the current size.
    #[allow(unused_variables)]
    fn resized(&mut self, window: &mut RunningWindow<AppMessage>) {}

    /// The window's theme has been updated. [`RunningWindow::theme()`]
    /// returns the current theme.
    #[allow(unused_variables)]
    fn theme_changed(&mut self, window: &mut RunningWindow<AppMessage>) {}

    /// A file has been dropped on the window.
    #[allow(unused_variables)]
    fn dropped_file(&mut self, window: &mut RunningWindow<AppMessage>, path: PathBuf) {}

    /// A file is hovering over the window.
    #[allow(unused_variables)]
    fn hovered_file(&mut self, window: &mut RunningWindow<AppMessage>, path: PathBuf) {}

    /// A file being overed has been cancelled.
    #[allow(unused_variables)]
    fn hovered_file_cancelled(&mut self, window: &mut RunningWindow<AppMessage>) {}

    /// An input event has generated a character.
    #[allow(unused_variables)]
    fn received_character(&mut self, window: &mut RunningWindow<AppMessage>, char: char) {}

    /// A keyboard event occurred while the window was focused.
    #[allow(unused_variables)]
    fn keyboard_input(
        &mut self,
        window: &mut RunningWindow<AppMessage>,
        device_id: DeviceId,
        event: KeyEvent,
        is_synthetic: bool,
    ) {
    }

    /// The keyboard modifier keys have changed. [`RunningWindow::modifiers()`]
    /// returns the current modifier keys state.
    #[allow(unused_variables)]
    fn modifiers_changed(&mut self, window: &mut RunningWindow<AppMessage>) {}

    /// An international input even thas occurred for the window.
    #[allow(unused_variables)]
    fn ime(&mut self, window: &mut RunningWindow<AppMessage>, ime: Ime) {}

    /// A cursor has moved over the window.
    #[allow(unused_variables)]
    fn cursor_moved(
        &mut self,
        window: &mut RunningWindow<AppMessage>,
        device_id: DeviceId,
        position: PhysicalPosition<f64>,
    ) {
    }

    /// A cursor has hovered over the window.
    #[allow(unused_variables)]
    fn cursor_entered(&mut self, window: &mut RunningWindow<AppMessage>, device_id: DeviceId) {}

    /// A cursor is no longer hovering over the window.
    #[allow(unused_variables)]
    fn cursor_left(&mut self, window: &mut RunningWindow<AppMessage>, device_id: DeviceId) {}

    /// An event from a mouse wheel.
    #[allow(unused_variables)]
    fn mouse_wheel(
        &mut self,
        window: &mut RunningWindow<AppMessage>,
        device_id: DeviceId,
        delta: MouseScrollDelta,
        phase: TouchPhase,
    ) {
    }

    /// A mouse button was pressed or released.
    #[allow(unused_variables)]
    fn mouse_input(
        &mut self,
        window: &mut RunningWindow<AppMessage>,
        device_id: DeviceId,
        state: ElementState,
        button: MouseButton,
    ) {
    }

    /// A pressure-sensitive touchpad was touched.
    #[allow(unused_variables)]
    fn touchpad_pressure(
        &mut self,
        window: &mut RunningWindow<AppMessage>,
        device_id: DeviceId,
        pressure: f32,
        stage: i64,
    ) {
    }

    /// A multi-axis input device has registered motion.
    #[allow(unused_variables)]
    fn axis_motion(
        &mut self,
        window: &mut RunningWindow<AppMessage>,
        device_id: DeviceId,
        axis: AxisId,
        value: f64,
    ) {
    }

    /// A touch event.
    #[allow(unused_variables)]
    fn touch(&mut self, window: &mut RunningWindow<AppMessage>, touch: Touch) {}

    /// A touchpad-originated magnification gesture.
    #[allow(unused_variables)]
    fn touchpad_magnify(
        &mut self,
        window: &mut RunningWindow<AppMessage>,
        device_id: DeviceId,
        delta: f64,
        phase: TouchPhase,
    ) {
    }

    /// A request to smart-magnify the window.
    #[allow(unused_variables)]
    fn smart_magnify(&mut self, window: &mut RunningWindow<AppMessage>, device_id: DeviceId) {}

    /// A touchpad-originated rotation gesture.
    #[allow(unused_variables)]
    fn touchpad_rotate(
        &mut self,
        window: &mut RunningWindow<AppMessage>,
        device_id: DeviceId,
        delta: f32,
        phase: TouchPhase,
    ) {
    }

    /// A user event has been received by the window.
    #[allow(unused_variables)]
    fn event(&mut self, window: &mut RunningWindow<AppMessage>, event: AppMessage::Window) {}
}

pub trait Run: WindowBehavior<()> {
    /// Runs a window with a default instance of this behavior's
    /// [`Context`](Self::Context).
    ///
    /// This function is shorthand for creating a [`PendingApp`], opening this
    /// window inside of it, and running the pending app.
    fn run() -> Result<(), EventLoopError>
    where
        Self::Context: Default,
    {
        let app = PendingApp::new();
        Self::open(&app).expect("error opening initial window");
        app.run()
    }

    /// Runs a window with the provided [`Context`](Self::Context).
    ///
    /// This function is shorthand for creating a [`PendingApp`], opening this
    /// window inside of it, and running the pending app.
    fn run_with(context: Self::Context) -> Result<(), EventLoopError> {
        let app = PendingApp::new();
        Self::open_with(&app, context).expect("error opening initial window");
        app.run()
    }
}

impl<T> Run for T where T: WindowBehavior<()> {}
