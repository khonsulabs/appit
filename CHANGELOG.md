# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Breaking Changes

- Several functions now accept an `ExecutingApp` parameter instead of a
  `Windows` parameter. This new type exposes access to other information from
  the event loop such as monitor information. The affected APIs are:

  - `PendingApp::new_with_event_callback`
  - `WindowBehavior::run_with_event_callback`
  - `WindowBehavior::run_with_context_and_event_callback`
- `RunningWindow::position` has been split into `RunningWindow::inner_position`
  and `RunningWindow::outer_position`. `RunningWindow::set_position` has been
  renamed to `RunningWindow::set_outer_position`.

### Fixed

- `Window`'s `Clone` implementation no longer requires its generic parameter to
  implement `Clone`.
- `WindowAttributes::active` is now honored when using `delay_visible`.

### Added

- `PendingApp::on_startup` accepts a callback that will be invoked once the
  event loop is executing.
- `WindowBehavior::moved` is called when the window moves.
- `RunningWindow::outer_size` is a new function that returns the window's
  current size including decorations.
- `App::prevent_shutdown()` returns a guard that prevents the application from
  closing automatically when the final window is closed.
- `WindowBehavior::initialized` is called once when the window has been fully
  initialized. This happens after the `delay_visible` logic has been executed.

### Changed

- `AsApplication` is now explicitly implemented for `App` and `PendingApp`
  rather than implemented using a blanket implementation. This allows downstream
  crates to create wrappers of these types that can implement `AsApplication`.

## v0.3.2 (2024-08-28)

### Fixed

- When multiple windows are open, windows now properly close fully without
  requiring that all windows are closed.

## v0.3.1 (2024-07-15)

### Added

- `WindowAttributes::delay_visible` is a new setting initializes the window
  `visible: false` before showing it after the first successful redraw. The goal
  is to avoid the OS drawing an empty window before the window behavior has
  initialized. This new attribute defaults to true.

## v0.3.0 (2024-05-12)

### Breaking Changes

- This crate no longer specifies a specific raw-window-handle flag for winit.
  This crate will maintain feature flags that allow picking whatever versions
  winit is exposing. As of writing this note, the choices are `rwh_05` and
  `rwh_06`. `rwh_05` was the feature that was activated in v0.2.0.
- `winit` has been updated to 0.30.0.
- `Window::id` now returns `Option<WindowId>`, as a window may be opened before
  the event loop has been started.
- `WindowBehavior::build`, `WindowBehavior::build_with`, `WindowBehavior::open`,
  and `WindowBehavior::open_with` now require exclusive references to the
  application.
- These gesture events have been renamed to match `winit`'s updated nomenclature:
  - `WindowBehavior::touchpad_magnify` -> `WindowBehavior::pinch_gesture`
  - `WindowBehavior::smart_magnify` -> `WindowBehavior::double_tap_gesture`

### Changed

- All `&Appplication` bounds now are `?Sized`, enabling `&dyn Application`
  parameters.
- Redraw requests from `winit` now block the event loop thread until the window
  has been repainted.

### Added

- `AsApplication` now provides `as_application_mut`.
- `WindowBeahvior::pan_gesture` is a new event provided by `winit`.

## v0.2.0 (2023-12-27)

### Breaking Changes

- `UnwindSafe` is no longer required for `WindowBehavior` or
  `WindowBehavior::Context`.

### Changed

- This crate's default features now include `wayland-csd-adwaita`. This enables
  winit's built-in decoration drawing on Wayland.

### Fixed

- `App` now implements `Application`.
- `Window` is now fully weak. Previously the channel for messages would still
  remain allocated while instances of `Window` existed. Now, the messages
  channel is freed as soon as the window is closed.

### Added

- `AsApplication` is a new trait that can be implemented to resolve to the `App`
  type. This allows wrapper types to be written that hide the appit types.
- `WindowAttributes` now implement `Debug`.

## v0.1.1 (2023-12-18)

### Fixed

- Errors when building for Windows have been resolved.

## v0.1.0 (2023-12-18)

This is the initial alpha release.
