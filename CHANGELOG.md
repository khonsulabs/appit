# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Breaking Changes

- This crate no longer specifies a specific raw-window-handle flag for winit.
  This crate will maintain feature flags that allow picking whatever versions
  winit is exposing. As of writing this note, the choices are `rwh_05` and
  `rwh_06`. `rwh_05` was the feature that was activated in v0.2.0.

### Changed

- All `&Appplication` bounds now are `?Sized`, enabling `&dyn Application`
  parameters.

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
