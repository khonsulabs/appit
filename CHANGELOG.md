# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Breaking Changes

- `UnwindSafe` is no longer required for `WindowBehavior` or
  `WindowBehavior::Context`.

### Changed

- This crate's default features now include `wayland-csd-adwaita`. This enables
  winit's built-in decoration drawing on Wayland.

### Fixed

- `App` now implements `Application`.

### Added

- `AsApplication` is a new trait that can be implemented to resolve to the `App`
  type. This allows wrapper types to be written that hide the appit types.
- `WindowAttributes` now implement `Debug`.

## v0.1.1 (2023-12-18)

### Fixed

- Errors when building for Windows have been resolved.

## v0.1.0 (2023-12-18)

This is the initial alpha release.
