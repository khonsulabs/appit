# appit

An opinionated wrapper for `winit` that provides a trait-based approach to
implementing multi-window applications.

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

## Why not use this crate?

- Very new, largely untested.
- Not all platforms support threads, and a single-window, single-thread code
  path is not supported yet.
