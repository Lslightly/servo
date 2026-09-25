/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! An event loop implementation that works in headless mode.

use std::sync::{Arc, Mutex};

use gilrs::Event;
use log::warn;
use servo::EventLoopWaker;
#[cfg(feature = "gamepad")]
use servo::GamepadIndex;
use winit::event_loop::{EventLoop, EventLoop as WinitEventLoop, EventLoopProxy};

use super::app::App;
use crate::headless_event_loop::{HeadlessEventLoop, HeadlessEventLoopWaker};

#[derive(Debug)]
pub enum AppEvent {
    /// Another process or thread has kicked the OS event loop with EventLoopWaker.
    Waker,
    Accessibility(egui_winit::accesskit_winit::Event),
    #[cfg(feature = "gamepad")]
    Gamepad(Event, String, GamepadIndex),
}

impl From<egui_winit::accesskit_winit::Event> for AppEvent {
    fn from(event: egui_winit::accesskit_winit::Event) -> AppEvent {
        AppEvent::Accessibility(event)
    }
}

/// A headed or headless event loop. Headless event loops are necessary for environments without a
/// display server. Ideally, we could use the headed winit event loop in both modes, but on Linux,
/// the event loop requires a display server, which prevents running servoshell in a console.
#[allow(clippy::large_enum_variant)]
pub(crate) enum ServoShellEventLoop {
    /// A real Winit windowing event loop.
    Winit(EventLoop<AppEvent>),
    /// A fake event loop which contains a signalling flag used to ensure
    /// that pending events get processed in a timely fashion, and a condition
    /// variable to allow waiting on that flag changing state.
    Headless(Arc<HeadlessEventLoop>),
}

impl ServoShellEventLoop {
    pub(crate) fn headless() -> ServoShellEventLoop {
        ServoShellEventLoop::Headless(Default::default())
    }

    pub(crate) fn headed() -> ServoShellEventLoop {
        ServoShellEventLoop::Winit(
            WinitEventLoop::with_user_event()
                .build()
                .expect("Could not start winit event loop"),
        )
    }
}

impl ServoShellEventLoop {
    pub(crate) fn event_loop_proxy(&self) -> Option<EventLoopProxy<AppEvent>> {
        match self {
            ServoShellEventLoop::Winit(event_loop) => Some(event_loop.create_proxy()),
            ServoShellEventLoop::Headless(..) => None,
        }
    }

    pub fn create_event_loop_waker(&self) -> Box<dyn EventLoopWaker> {
        match self {
            ServoShellEventLoop::Winit(event_loop) => {
                Box::new(HeadedEventLoopWaker::new(event_loop))
            },
            ServoShellEventLoop::Headless(data) => Box::new(HeadlessEventLoopWaker(data.clone())),
        }
    }

    pub fn run_app(self, app: &mut App) {
        match self {
            ServoShellEventLoop::Winit(event_loop) => {
                event_loop
                    .run_app(app)
                    .expect("Failed while running events loop");
            },
            ServoShellEventLoop::Headless(event_loop) => {
                app.init(None);
                event_loop.run(|| app.pump_servo_event_loop(None));
            },
        }
    }
}

#[derive(Clone)]
struct HeadedEventLoopWaker {
    proxy: Arc<Mutex<EventLoopProxy<AppEvent>>>,
}

impl HeadedEventLoopWaker {
    fn new(event_loop: &EventLoop<AppEvent>) -> HeadedEventLoopWaker {
        let proxy = Arc::new(Mutex::new(event_loop.create_proxy()));
        HeadedEventLoopWaker { proxy }
    }
}

impl EventLoopWaker for HeadedEventLoopWaker {
    fn wake(&self) {
        // Kick the OS event loop awake.
        if let Err(err) = self.proxy.lock().unwrap().send_event(AppEvent::Waker) {
            warn!("Failed to wake up event loop ({}).", err);
        }
    }

    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
}
