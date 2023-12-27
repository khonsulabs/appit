# appit

![appit is considered alpha and unsupported](https://img.shields.io/badge/status-alpha-orange)
[![crate version](https://img.shields.io/crates/v/appit.svg)](https://crates.io/crates/appit)
[![Documentation for `main` branch](https://img.shields.io/badge/docs-main-informational)](https://khonsulabs.github.io/appit/main/appit)

An opinionated wrapper for `winit` that provides a trait-based, multi-threaded
approach to implementing multi-window applications.

This crate's main type is `WindowBehavior`, a trait that provides functions for
nearly every `winit::event::WindowEvent`. This allows you to implement exactly
which events you wish to respond to, and ignore the rest without a large match
statement.

This crate also keeps track of the redraw state of the window, and allows
scheduling redraws in the future.

```rust,no_run
use appit::WindowBehavior;

struct MyWindow;

impl WindowBehavior for MyWindow {
    type Context = ();

    fn initialize(_window: &mut appit::RunningWindow, _context: Self::Context) -> Self {
        Self
    }

    fn redraw(&mut self, window: &mut appit::RunningWindow) {
        println!("Should redraw");
    }
}

fn main() {
    MyWindow::run()
}
```

## Project Status

This project is early in development as part of [Kludgine][kludgine] and
[Cushy][cushy]. It is considered alpha and unsupported at this time, and the
primary focus for [@ecton][ecton] is to use this for his own projects. Feature
requests and bug fixes will be prioritized based on @ecton's own needs.

If you would like to contribute, bug fixes are always appreciated. Before
working on a new feature, please [open an issue][issues] proposing the feature
and problem it aims to solve. Doing so will help prevent friction in merging
pull requests, as it ensures changes fit the vision the maintainers have for
Cushy.

[cushy]: https://github.com/khonsulabs/cushy
[kludgine]: https://github.com/khonsulabs/kludgine
[ecton]: https://github.com/khonsulabs/ecton
[issues]: https://github.com/khonsulabs/cushy/issues
