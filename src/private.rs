use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::error::OsError;
use winit::event::{
    AxisId, DeviceId, ElementState, Ime, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, Touch,
    TouchPhase,
};
use winit::event_loop::AsyncRequestSerial;
use winit::window::{ActivationToken, Theme, WindowId};

use crate::window::WindowAttributes;
use crate::Message;

pub trait ApplicationSealed<AppMessage>
where
    AppMessage: Message,
{
    fn open(
        &self,
        window: WindowAttributes,
        sender: Arc<mpsc::SyncSender<WindowMessage<AppMessage::Window>>>,
    ) -> Result<Option<Arc<winit::window::Window>>, OsError>;
}

pub enum EventLoopMessage<AppMessage>
where
    AppMessage: Message,
{
    OpenWindow {
        attrs: WindowAttributes,
        sender: Arc<mpsc::SyncSender<WindowMessage<AppMessage::Window>>>,
        open_sender: mpsc::SyncSender<Result<Arc<winit::window::Window>, OsError>>,
    },
    CloseWindow(WindowId),
    WindowPanic(WindowId),
    User {
        message: AppMessage,
        response_sender: mpsc::SyncSender<AppMessage::Response>,
    },
}

#[derive(Debug)]
pub enum WindowMessage<User> {
    User(User),
    Event(WindowEvent),
}

#[derive(Debug)]
pub enum WindowEvent {
    RedrawRequested,

    /// The size of the window has changed. Contains the client area's new dimensions.
    Resized(PhysicalSize<u32>),

    /// The position of the window has changed. Contains the window's new position.
    ///
    /// ## Platform-specific
    ///
    /// - **iOS / Android / Web / Wayland:** Unsupported.
    Moved(PhysicalPosition<i32>),

    /// The window has been requested to close.
    CloseRequested,

    /// The window has been destroyed.
    Destroyed,

    /// A file has been dropped into the window.
    ///
    /// When the user drops multiple files at once, this event will be emitted for each file
    /// separately.
    DroppedFile(PathBuf),

    /// A file is being hovered over the window.
    ///
    /// When the user hovers multiple files at once, this event will be emitted for each file
    /// separately.
    HoveredFile(PathBuf),

    /// A file was hovered, but has exited the window.
    ///
    /// There will be a single `HoveredFileCancelled` event triggered even if multiple files were
    /// hovered.
    HoveredFileCancelled,

    /// The window received a unicode character.
    ///
    /// See also the [`Ime`](Self::Ime) event for more complex character sequences.
    ReceivedCharacter(char),

    /// The window gained or lost focus.
    ///
    /// The parameter is true if the window has gained focus, and false if it has lost focus.
    Focused(bool),

    /// An event from the keyboard has been received.
    KeyboardInput {
        device_id: DeviceId,
        event: KeyEvent,
        /// If `true`, the event was generated synthetically by winit
        /// in one of the following circumstances:
        ///
        /// * Synthetic key press events are generated for all keys pressed
        ///   when a window gains focus. Likewise, synthetic key release events
        ///   are generated for all keys pressed when a window goes out of focus.
        ///   ***Currently, this is only functional on X11 and Windows***
        ///
        /// Otherwise, this value is always `false`.
        is_synthetic: bool,
    },

    /// The keyboard modifiers have changed.
    ///
    /// ## Platform-specific
    ///
    /// - **Web:** This API is currently unimplemented on the web. This isn't by design - it's an
    ///   issue, and it should get fixed - but it's the current state of the API.
    ModifiersChanged(Modifiers),

    /// An event from an input method.
    ///
    /// **Note:** You have to explicitly enable this event using [`Window::set_ime_allowed`].
    ///
    /// ## Platform-specific
    ///
    /// - **iOS / Android / Web:** Unsupported.
    Ime(Ime),

    /// The cursor has moved on the window.
    CursorMoved {
        device_id: DeviceId,

        /// (x,y) coords in pixels relative to the top-left corner of the window. Because the range of this data is
        /// limited by the display area and it may have been transformed by the OS to implement effects such as cursor
        /// acceleration, it should not be used to implement non-cursor-like interactions such as 3D camera control.
        position: PhysicalPosition<f64>,
    },

    /// The cursor has entered the window.
    CursorEntered {
        device_id: DeviceId,
    },

    /// The cursor has left the window.
    CursorLeft {
        device_id: DeviceId,
    },

    /// A mouse wheel movement or touchpad scroll occurred.
    MouseWheel {
        device_id: DeviceId,
        delta: MouseScrollDelta,
        phase: TouchPhase,
    },

    /// An mouse button press has been received.
    MouseInput {
        device_id: DeviceId,
        state: ElementState,
        button: MouseButton,
    },

    /// Touchpad pressure event.
    ///
    /// At the moment, only supported on Apple forcetouch-capable macbooks.
    /// The parameters are: pressure level (value between 0 and 1 representing how hard the touchpad
    /// is being pressed) and stage (integer representing the click level).
    TouchpadPressure {
        device_id: DeviceId,
        pressure: f32,
        stage: i64,
    },

    /// Motion on some analog axis. May report data redundant to other, more specific events.
    AxisMotion {
        device_id: DeviceId,
        axis: AxisId,
        value: f64,
    },

    /// Touch event has been received
    Touch(Touch),

    /// The window's scale factor has changed.
    ///
    /// The following user actions can cause DPI changes:
    ///
    /// * Changing the display's resolution.
    /// * Changing the display's scale factor (e.g. in Control Panel on Windows).
    /// * Moving the window to a display with a different scale factor.
    ///
    /// After this event callback has been processed, the window will be resized to whatever value
    /// is pointed to by the `new_inner_size` reference. By default, this will contain the size suggested
    /// by the OS, but it can be changed to any value.
    ///
    /// For more information about DPI in general, see the [`dpi`](crate::dpi) module.
    ScaleFactorChanged {
        scale_factor: f64,
    },

    /// The system window theme has changed.
    ///
    /// Applications might wish to react to this to change the theme of the content of the window
    /// when the system changes the window theme.
    ///
    /// ## Platform-specific
    ///
    /// At the moment this is only supported on Windows.
    ThemeChanged(Theme),

    /// The window has been occluded (completely hidden from view).
    ///
    /// This is different to window visibility as it depends on whether the window is closed,
    /// minimised, set invisible, or fully occluded by another window.
    ///
    /// Platform-specific behavior:
    /// - **iOS / Android / Web / Wayland / Windows:** Unsupported.
    Occluded(bool),

    TouchpadMagnify {
        device_id: DeviceId,
        delta: f64,
        phase: TouchPhase,
    },
    SmartMagnify {
        device_id: DeviceId,
    },
    TouchpadRotate {
        device_id: DeviceId,
        delta: f32,
        phase: TouchPhase,
    },
    /// The activation token was delivered back and now could be used.
    ///
    /// Delivered in response to [`request_activation_token`].
    ActivationTokenDone {
        serial: AsyncRequestSerial,
        token: ActivationToken,
    },
}

impl From<winit::event::WindowEvent> for WindowEvent {
    #[allow(clippy::too_many_lines)] // it's a match statement
    fn from(event: winit::event::WindowEvent) -> Self {
        match event {
            winit::event::WindowEvent::RedrawRequested => Self::RedrawRequested,
            winit::event::WindowEvent::Resized(size) => Self::Resized(size),
            winit::event::WindowEvent::Moved(pos) => Self::Moved(pos),
            winit::event::WindowEvent::CloseRequested => Self::CloseRequested,
            winit::event::WindowEvent::Destroyed => Self::Destroyed,
            winit::event::WindowEvent::DroppedFile(path) => Self::DroppedFile(path),
            winit::event::WindowEvent::HoveredFile(path) => Self::HoveredFile(path),
            winit::event::WindowEvent::HoveredFileCancelled => Self::HoveredFileCancelled,
            winit::event::WindowEvent::Focused(focused) => Self::Focused(focused),
            winit::event::WindowEvent::KeyboardInput {
                device_id,
                event,
                is_synthetic,
            } => Self::KeyboardInput {
                device_id,
                event,
                is_synthetic,
            },

            winit::event::WindowEvent::ModifiersChanged(modifiers) => {
                Self::ModifiersChanged(modifiers)
            }
            winit::event::WindowEvent::Ime(ime) => Self::Ime(ime),
            winit::event::WindowEvent::CursorMoved {
                device_id,
                position,
                ..
            } => Self::CursorMoved {
                device_id,
                position,
            },
            winit::event::WindowEvent::CursorEntered { device_id } => {
                Self::CursorEntered { device_id }
            }
            winit::event::WindowEvent::CursorLeft { device_id } => Self::CursorLeft { device_id },
            winit::event::WindowEvent::MouseWheel {
                device_id,
                delta,
                phase,
                ..
            } => Self::MouseWheel {
                device_id,
                delta,
                phase,
            },
            winit::event::WindowEvent::MouseInput {
                device_id,
                state,
                button,
                ..
            } => Self::MouseInput {
                device_id,
                state,
                button,
            },
            winit::event::WindowEvent::TouchpadPressure {
                device_id,
                pressure,
                stage,
            } => Self::TouchpadPressure {
                device_id,
                pressure,
                stage,
            },
            winit::event::WindowEvent::AxisMotion {
                device_id,
                axis,
                value,
            } => Self::AxisMotion {
                device_id,
                axis,
                value,
            },
            winit::event::WindowEvent::Touch(touch) => Self::Touch(touch),
            winit::event::WindowEvent::ScaleFactorChanged {
                scale_factor,
                 .. // TODO use the suggested size from the writer <https://github.com/rust-windowing/winit/issues/3080>
            } => {
                Self::ScaleFactorChanged {
                    scale_factor,
                }
            },
            winit::event::WindowEvent::ThemeChanged(theme) => Self::ThemeChanged(theme),
            winit::event::WindowEvent::Occluded(occluded) => Self::Occluded(occluded),
            winit::event::WindowEvent::TouchpadMagnify {
                device_id,
                delta,
                phase,
            } => Self::TouchpadMagnify {
                device_id,
                delta,
                phase,
            },
            winit::event::WindowEvent::SmartMagnify { device_id } => {
                Self::SmartMagnify { device_id }
            }
            winit::event::WindowEvent::TouchpadRotate {
                device_id,
                delta,
                phase,
            } => Self::TouchpadRotate {
                device_id,
                delta,
                phase,
            },
            winit::event::WindowEvent::ActivationTokenDone { serial, token } => {
                Self::ActivationTokenDone { serial, token }
            }
        }
    }
}
