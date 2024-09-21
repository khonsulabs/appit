use winit::{event_loop::EventLoopProxy, window::Theme};

use crate::{private::EventLoopMessage, Message};

fn observe_darkmode_changes<AppMessage>(proxy: EventLoopProxy<EventLoopMessage<AppMessage>>)
where
    AppMessage: Message,
{
    let _ignored_error = darkmode::subscribe(move |mode| {
        let _ = proxy.send_event(EventLoopMessage::ThemeChanged(match mode {
            darkmode::Mode::Dark => Theme::Dark,
            darkmode::Mode::Light => Theme::Light,
            darkmode::Mode::Default => return,
        }));
    });
}
