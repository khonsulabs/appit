use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, PoisonError, Weak};
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

use crate::private::{self, OpenedWindow, RedrawGuard, WindowEvent, WindowSpawner};
use crate::{
    App, Application, AsApplication, EventLoopMessage, ExecutingApp, Message, PendingApp,
    WindowMessage,
};

/// A weak reference to a running window.
#[derive(Debug)]
pub struct Window<Message> {
    opened: OpenedWindow,
    sender: Weak<mpsc::SyncSender<WindowMessage<Message>>>,
}

impl<Message> Window<Message> {
    /// Returns the winit id of the window.
    #[must_use]
    pub fn id(&self) -> Option<WindowId> {
        self.opened
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|w| w.id())
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
        let Some(sender) = self.sender.upgrade() else {
            return Err(message);
        };
        match sender.send(WindowMessage::User(message)) {
            Ok(()) => Ok(()),
            Err(mpsc::SendError(WindowMessage::User(message))) => Err(message),
            _ => unreachable!("same input as output"),
        }
    }
}

impl<Message> Clone for Window<Message> {
    fn clone(&self) -> Self {
        Self {
            opened: self.opened.clone(),
            sender: self.sender.clone(),
        }
    }
}

/// A builder for a window.
pub struct WindowBuilder<'a, Behavior, Application, AppMessage>
where
    Behavior: self::WindowBehavior<AppMessage>,
    AppMessage: Message,
    Application: ?Sized,
{
    owner: &'a mut Application,
    context: Behavior::Context,
    attributes: WindowAttributes,
}
impl<'a, Behavior, Application, AppMessage> Deref
    for WindowBuilder<'a, Behavior, Application, AppMessage>
where
    Behavior: self::WindowBehavior<AppMessage>,
    AppMessage: Message,
    Application: ?Sized,
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
    Application: ?Sized,
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
#[derive(Debug)]
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
    /// When true, this window will delay honoring the `visible` attribute until after the window behavior has been initialized and redrawn a single time.
    pub delay_visible: bool,
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
        let fullscreen = defaults.fullscreen.clone();
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
            delay_visible: true,
        }
    }
}

impl<'a, Behavior, Application, AppMessage> WindowBuilder<'a, Behavior, Application, AppMessage>
where
    Behavior: self::WindowBehavior<AppMessage>,
    Application: crate::AsApplication<AppMessage> + ?Sized,
    AppMessage: Message,
{
    pub(crate) fn new(owner: &'a mut Application, context: Behavior::Context) -> Self {
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
    /// This function returns any error that winit may return from attempting to
    /// open a window.
    pub fn open(mut self) -> Result<Option<Window<AppMessage::Window>>, winit::error::OsError> {
        // The window's thread shouldn't ever block for long periods of time. To
        // avoid a "frozen" window causing massive memory allocations, we'll use
        // a fixed-size channel and be cautious to not block the main event loop
        // by always using try_send.
        let (sender, receiver) = mpsc::sync_channel(65536);
        let sender = Arc::new(sender);
        let app = self.owner.as_application().app();
        let show_after_init = (self.attributes.delay_visible
            && std::mem::replace(&mut self.attributes.visible, false))
        .then_some(self.attributes.active);

        let Some(winit) = self.owner.as_application_mut().open(
            self.attributes,
            sender.clone(),
            Box::new({
                let sender = sender.clone();
                move |opened| {
                    let winit = opened.winit().expect("just opened");
                    let running_window = RunningWindow {
                        messages: (sender, receiver),
                        responses: mpsc::sync_channel(1),
                        app,
                        occluded: winit.is_visible().unwrap_or(false),
                        focused: winit.has_focus(),
                        inner_size: winit.inner_size(),
                        outer_size: winit.outer_size(),
                        inner_position: winit.inner_position().unwrap_or_default(),
                        outer_position: winit.outer_position().unwrap_or_default(),
                        scale: winit.scale_factor(),
                        theme: winit.theme().unwrap_or(Theme::Dark),
                        window: winit,
                        opened,
                        next_redraw_target: None,
                        close: false,
                        modifiers: Modifiers::default(),
                        cursor_position: None,
                        mouse_buttons: HashSet::default(),
                        keys: HashSet::default(),
                        show_after_init,
                    };

                    thread::spawn(move || running_window.run_with::<Behavior>(self.context));
                }
            }),
        )?
        else {
            return Ok(None);
        };
        let window = Window {
            opened: winit.clone(),
            sender: Arc::downgrade(&sender),
        };

        Ok(Some(window))
    }
}

type SyncArcChannel<T> = (Arc<mpsc::SyncSender<T>>, mpsc::Receiver<T>);
type SyncChannel<T> = (mpsc::SyncSender<T>, mpsc::Receiver<T>);

enum HandleMessageResult {
    Ok,
    RedrawRequired(RedrawGuard),
    Destroyed,
}

/// A window that is running in its own thread.
#[allow(clippy::struct_excessive_bools)] // stop judging me clippy!
pub struct RunningWindow<AppMessage>
where
    AppMessage: Message,
{
    window: Arc<winit::window::Window>,
    opened: OpenedWindow,
    next_redraw_target: Option<RedrawTarget>,
    messages: SyncArcChannel<WindowMessage<AppMessage::Window>>,
    responses: SyncChannel<AppMessage::Response>,
    app: App<AppMessage>,
    inner_size: PhysicalSize<u32>,
    outer_size: PhysicalSize<u32>,
    outer_position: PhysicalPosition<i32>,
    inner_position: PhysicalPosition<i32>,
    cursor_position: Option<PhysicalPosition<f64>>,
    mouse_buttons: HashSet<MouseButton>,
    keys: HashSet<PhysicalKey>,
    scale: f64,
    close: bool,
    occluded: bool,
    focused: bool,
    theme: Theme,
    modifiers: Modifiers,
    show_after_init: Option<bool>,
}

impl<AppMessage> RunningWindow<AppMessage>
where
    AppMessage: Message,
{
    /// Returns a reference to the underlying window.
    #[must_use]
    pub fn winit(&self) -> &Arc<winit::window::Window> {
        &self.window
    }

    /// Returns a handle to this window.
    #[must_use]
    pub fn handle(&self) -> Window<AppMessage::Window> {
        Window {
            opened: self.opened.clone(),
            sender: Arc::downgrade(&self.messages.0),
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
    #[must_use]
    pub fn request_inner_size(&mut self, new_size: PhysicalSize<u32>) -> Option<PhysicalSize<u32>> {
        let result = self.window.request_inner_size(new_size);
        if let Some(applied_size) = result {
            self.inner_size = applied_size;
            self.outer_size = self.window.outer_size();
        }
        result
    }

    /// Returns the current outer size of the window, in pixels.
    #[must_use]
    pub const fn outer_size(&self) -> PhysicalSize<u32> {
        self.outer_size
    }

    /// Returns the current outer position of the window, in pixels.
    #[must_use]
    pub const fn outer_position(&self) -> PhysicalPosition<i32> {
        self.outer_position
    }

    /// Returns the current inner position of the window, in pixels.
    #[must_use]
    pub const fn inner_position(&self) -> PhysicalPosition<i32> {
        self.inner_position
    }

    /// Sets the current position of the window, in pixels.
    pub fn set_outer_position(&self, new_position: PhysicalPosition<i32>) {
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
        // We assert unwind safety here due to internal types on some platforms
        // in winit use dyn trait objects that do not specify unwind safety.
        // However, in this situation we are not recovering the window itself.
        // The panic will cause us to ask winit to close the window. There is no
        // recovery for a panic inside of a window, the only question is whether
        // the entire app panics or not.
        let possible_panic = std::panic::catch_unwind(AssertUnwindSafe(move || {
            let mut behavior = Behavior::initialize(&mut self, context)?;

            // When it takes a while for a graphics stack to initialize, we can
            // avoid showing a blank window due to our multi-threaded event
            // handling by not showing the window until the graphics stack has
            // been initialized.
            if let Some(activate) = self.show_after_init {
                self.next_redraw_target = None;
                behavior.redraw(&mut self);
                self.window.set_visible(true);
                if activate {
                    self.window.focus_window();
                }
            }

            behavior.initialized(&mut self);

            while !self.close {
                match self.process_messages_until_redraw(&mut behavior) {
                    Ok(guard) => {
                        self.next_redraw_target = None;
                        self.inner_size = self.window.inner_size();
                        behavior.redraw(&mut self);
                        drop(guard);
                    }
                    Err(()) => break,
                }
            }
            // Do not notify the main thread to close the window until after the
            // behavior is dropped. This upholds the requirement for RawWindowHandle
            // by making sure that any resources required by the behavior have had a
            // chance to be freed.
            Ok(())
        }));

        match possible_panic {
            Ok(Ok(())) => {
                let _result = proxy.send_event(EventLoopMessage::CloseWindow(window_id));
            }
            Ok(Err(init_error)) => {
                let _result = proxy.send_event(EventLoopMessage::Error(init_error));
            }
            Err(panic) => {
                let _result = proxy.send_event(EventLoopMessage::WindowPanic(window_id));
                std::panic::resume_unwind(panic)
            }
        }
    }

    fn process_messages_until_redraw<Behavior>(
        &mut self,
        behavior: &mut Behavior,
    ) -> Result<Option<RedrawGuard>, ()>
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
                    Err(mpsc::TryRecvError::Disconnected) => return Err(()),
                    Err(mpsc::TryRecvError::Empty) => return Ok(None),
                },
                // We have a scheduled time for the next frame, and it hasn't
                // elapsed yet.
                TimeUntilRedraw::Some(duration_remaining) => {
                    match self.messages.1.recv_timeout(duration_remaining) {
                        Ok(message) => message,
                        Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
                        Err(mpsc::RecvTimeoutError::Disconnected) => return Err(()),
                    }
                }
                // No scheduled redraw time, sleep until the next message.
                TimeUntilRedraw::Indefinite => match self.messages.1.recv() {
                    Ok(message) => message,
                    Err(_) => return Err(()),
                },
            };

            match self.handle_message(message, behavior) {
                HandleMessageResult::Ok => {}
                HandleMessageResult::RedrawRequired(guard) => return Ok(Some(guard)),
                HandleMessageResult::Destroyed => return Err(()),
            }
        }
    }

    #[allow(clippy::too_many_lines)] // can't avoid the match
    fn handle_message<Behavior>(
        &mut self,
        message: WindowMessage<AppMessage::Window>,
        behavior: &mut Behavior,
    ) -> HandleMessageResult
    where
        Behavior: self::WindowBehavior<AppMessage>,
    {
        match message {
            WindowMessage::User(user) => behavior.event(self, user),
            WindowMessage::Event(evt) => match evt {
                WindowEvent::RedrawRequested(guard) => {
                    self.set_needs_redraw();
                    return HandleMessageResult::RedrawRequired(guard);
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
                    let new_outer_size = self.window.outer_size();
                    self.inner_size = new_inner_size;
                    self.outer_size = new_outer_size;
                    behavior.scale_factor_changed(self);
                    if self.inner_size != new_inner_size || self.outer_size != new_outer_size {
                        behavior.resized(self);
                    }
                }
                WindowEvent::Resized(new_inner_size) => {
                    let new_outer_size = self.window.outer_size();
                    let outer_size_changed = new_outer_size != self.outer_size;
                    self.outer_size = new_outer_size;
                    if outer_size_changed || self.inner_size != new_inner_size {
                        self.inner_size = new_inner_size;
                        behavior.resized(self);
                    }
                }
                WindowEvent::Moved(outer_position) => {
                    let inner_position = self.window.inner_position().unwrap_or_default();
                    if self.outer_position != outer_position
                        || self.inner_position != inner_position
                    {
                        self.outer_position = outer_position;
                        self.inner_position = inner_position;
                        behavior.moved(self);
                    }
                }
                WindowEvent::Destroyed => {
                    return HandleMessageResult::Destroyed;
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
                WindowEvent::PinchGesture {
                    device_id,
                    delta,
                    phase,
                } => {
                    behavior.pinch_gesture(self, device_id, delta, phase);
                }
                WindowEvent::PanGesture {
                    device_id,
                    delta,
                    phase,
                } => {
                    behavior.pan_gesture(self, device_id, delta, phase);
                }
                WindowEvent::DoubleTapGesture { device_id } => {
                    behavior.double_tap_gesture(self, device_id);
                }
                WindowEvent::RotationGesture {
                    device_id,
                    delta,
                    phase,
                } => {
                    behavior.touchpad_rotate(self, device_id, delta, phase);
                }
                WindowEvent::ActivationTokenDone { .. } => {}
            },
        }

        HandleMessageResult::Ok
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
    fn send_error(
        &mut self,
        error: <AppMessage as Message>::Error,
    ) -> Result<(), winit::event_loop::EventLoopClosed<<AppMessage as Message>::Error>> {
        self.app.send_error(error)
    }
}

impl<AppMessage> private::ApplicationSealed<AppMessage> for RunningWindow<AppMessage>
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
            .app
            .proxy
            .send_event(EventLoopMessage::OpenWindow {
                attrs,
                sender,
                open_sender,
                spawner,
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
pub trait WindowBehavior<AppMessage>: Sized + 'static
where
    AppMessage: Message,
{
    /// A type that is passed to [`initialize()`](Self::initialize).
    ///
    /// This allows providing data to the window from the thread that is opening
    /// the window without requiring that `WindowBehavior` also be `Send`.
    type Context: Send;
    /// Returns a new window builder for this behavior. When the window is
    /// initialized, a default [`Context`](Self::Context) will be passed.
    fn build<App>(app: &mut App) -> WindowBuilder<'_, Self, App, AppMessage>
    where
        App: AsApplication<AppMessage> + ?Sized,
        Self::Context: Default,
    {
        Self::build_with(app, <Self::Context as Default>::default())
    }

    /// Returns a new window builder for this behavior. When the window is
    /// initialized, the provided context will be passed.
    fn build_with<App>(
        app: &mut App,
        context: Self::Context,
    ) -> WindowBuilder<'_, Self, App, AppMessage>
    where
        App: AsApplication<AppMessage> + ?Sized,
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
    /// Returns an [`EventLoopError`](winit::error::EventLoopError) upon the
    /// loop exiting due to an error. See
    /// [`EventLoop::run`](winit::event_loop::EventLoop::run) for more
    /// information.
    fn run_with_event_callback(
        app_callback: impl FnMut(AppMessage, ExecutingApp<'_, AppMessage>) -> AppMessage::Response
            + 'static,
    ) -> Result<(), EventLoopError>
    where
        Self::Context: Default,
    {
        let mut app = PendingApp::new_with_event_callback(app_callback);
        Self::open(&mut app).expect("error opening initial window");
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
    /// Returns an [`EventLoopError`](winit::error::EventLoopError) upon the
    /// loop exiting due to an error. See
    /// [`EventLoop::run`](winit::event_loop::EventLoop::run) for more
    /// information.
    fn run_with_context_and_event_callback(
        context: Self::Context,
        app_callback: impl FnMut(AppMessage, ExecutingApp<'_, AppMessage>) -> AppMessage::Response
            + 'static,
    ) -> Result<(), EventLoopError> {
        let mut app = PendingApp::new_with_event_callback(app_callback);
        Self::open_with(&mut app, context).expect("error opening initial window");
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
    /// This function returns any error that winit may return from attempting to
    /// open a window.
    fn open<App>(app: &mut App) -> Result<Option<Window<AppMessage::Window>>, OsError>
    where
        App: AsApplication<AppMessage> + ?Sized,
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
    /// This function returns any error that winit may return from attempting to
    /// open a window.
    fn open_with<App>(
        app: &mut App,
        context: Self::Context,
    ) -> Result<Option<Window<AppMessage::Window>>, OsError>
    where
        App: AsApplication<AppMessage> + ?Sized,
    {
        Self::build_with(app, context).open()
    }

    /// Returns a new instance of this behavior after initializing itself with
    /// the window and context.
    ///
    /// # Errors
    ///
    /// If the window cannot be initialized, this function should return the
    /// cause of the failure.
    fn initialize(
        window: &mut RunningWindow<AppMessage>,
        context: Self::Context,
    ) -> Result<Self, AppMessage::Error>;

    /// Displays the contents of the window.
    fn redraw(&mut self, window: &mut RunningWindow<AppMessage>);

    /// Invoked once a window is fully initialized.
    ///
    /// This is invoked after the window has been presented to the user, if it
    /// is initially visible.
    #[allow(unused_variables)]
    fn initialized(&mut self, window: &mut RunningWindow<AppMessage>) {}

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

    /// The window has been moved. [`RunningWindow::outer_position()`] returns
    /// the current position.
    #[allow(unused_variables)]
    fn moved(&mut self, window: &mut RunningWindow<AppMessage>) {}

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

    /// A magnification gesture.
    #[allow(unused_variables)]
    fn pinch_gesture(
        &mut self,
        window: &mut RunningWindow<AppMessage>,
        device_id: DeviceId,
        delta: f64,
        phase: TouchPhase,
    ) {
    }

    /// A pan/scroll gesture.
    #[allow(unused_variables)]
    fn pan_gesture(
        &mut self,
        window: &mut RunningWindow<AppMessage>,
        device_id: DeviceId,
        delta: PhysicalPosition<f32>,
        phase: TouchPhase,
    ) {
    }

    /// A request to smart-magnify the window.
    #[allow(unused_variables)]
    fn double_tap_gesture(&mut self, window: &mut RunningWindow<AppMessage>, device_id: DeviceId) {}

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

/// A runnable window.
pub trait Run: WindowBehavior<()> {
    /// Runs a window with a default instance of this behavior's
    /// [`Context`](WindowBehavior::Context).
    ///
    /// This function is shorthand for creating a [`PendingApp`], opening this
    /// window inside of it, and running the pending app.
    ///
    /// # Errors
    ///
    /// Returns an [`EventLoopError`](winit::error::EventLoopError) upon the
    /// loop exiting due to an error. See
    /// [`EventLoop::run`](winit::event_loop::EventLoop::run) for more
    /// information.
    fn run() -> Result<(), EventLoopError>
    where
        Self::Context: Default,
    {
        let mut app = PendingApp::new();
        Self::open(&mut app).expect("error opening initial window");
        app.run()
    }

    /// Runs a window with the provided [`Context`](WindowBehavior::Context).
    ///
    /// This function is shorthand for creating a [`PendingApp`], opening this
    /// window inside of it, and running the pending app.
    ///
    /// # Errors
    ///
    /// Returns an [`EventLoopError`](winit::error::EventLoopError) upon the
    /// loop exiting due to an error. See
    /// [`EventLoop::run`](winit::event_loop::EventLoop::run) for more
    /// information.
    fn run_with(context: Self::Context) -> Result<(), EventLoopError> {
        let mut app = PendingApp::new();
        Self::open_with(&mut app, context).expect("error opening initial window");
        app.run()
    }
}

impl<T> Run for T where T: WindowBehavior<()> {}
