//! Prototype: does a libcosmic app build for and run on Windows?

#![windows_subsystem = "windows"]

use cosmic::app::Settings;
use cosmic::iced::{Alignment, Length, Size};
use cosmic::prelude::*;
use cosmic::{Core, executor, iced, widget};

#[derive(Clone, Debug)]
enum Message {
    Tick,
}

struct Bar {
    core: Core,
    clock: String,
}

fn now() -> String {
    chrono::Local::now().format("%a %d %b  %H:%M").to_string()
}

impl cosmic::Application for Bar {
    type Executor = executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "dk.cramt.defenestrate.Bar";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(mut core: Core, _: ()) -> (Self, cosmic::app::Task<Message>) {
        core.window.show_headerbar = false;
        (Bar { core, clock: now() }, Task::none())
    }

    fn update(&mut self, message: Message) -> cosmic::app::Task<Message> {
        match message {
            Message::Tick => self.clock = now(),
        }
        Task::none()
    }

    fn subscription(&self) -> iced::Subscription<Message> {
        iced::time::every(std::time::Duration::from_secs(1)).map(|_| Message::Tick)
    }

    fn view(&self) -> Element<'_, Message> {
        widget::row([
            widget::text::heading("defenestrate").into(),
            widget::space::horizontal().into(),
            widget::text::body(&self.clock).into(),
        ])
            .align_y(Alignment::Center)
            .padding([0, 12])
            .height(Length::Fill)
            .into()
    }
}

fn main() -> cosmic::iced::Result {
    let settings = Settings::default()
        .size(Size::new(1280., 32.))
        .client_decorations(true);
    cosmic::app::run::<Bar>(settings, ())
}
