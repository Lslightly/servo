/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The standalone `servoshell` executable for OpenHarmony.
//!
//! The HAP loads servoshell as a library that renders into the `XComponent` surface of the
//! application. This executable instead runs Servo headless, so it can be started from an
//! `hdc shell` without an application, and several instances can run at the same time.
//! Offscreen rendering on OpenHarmony still uses the GPU.
//!
//! Servo's resources are read from the `resources` directory next to the executable.

use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;
use std::{env, panic, process};

use log::error;
use servo::{EventLoopWaker, ServoBuilder, UserContentManager};
use url::Url;

use super::resources;
use crate::headless_event_loop::{HeadlessEventLoop, HeadlessEventLoopWaker};
use crate::headless_window::HeadlessWindow;
use crate::prefs::{ArgumentParsingResult, parse_command_line_arguments};
use crate::running_app_state::RunningAppState;
use crate::window::{PlatformWindow, TopLevelWebViewCreationRequest};

pub(crate) fn main() {
    init_logging();
    panic::set_hook(Box::new(crate::panic_hook::panic_hook));
    crate::init_crypto();

    let args: Vec<String> = env::args().skip(1).collect();
    let (opts, preferences, servoshell_preferences) = match parse_command_line_arguments(&*args) {
        ArgumentParsingResult::ContentProcess(..) => {
            error!("Multiprocess mode is not supported on OpenHarmony");
            process::exit(1);
        },
        ArgumentParsingResult::ChromeProcess(opts, preferences, servoshell_preferences) => {
            (opts, preferences, servoshell_preferences)
        },
        ArgumentParsingResult::Exit => process::exit(0),
        ArgumentParsingResult::ErrorParsing => process::exit(1),
    };
    if !servoshell_preferences.headless {
        error!("The servoshell executable only runs headless on OpenHarmony, pass `--headless`");
        process::exit(1);
    }
    crate::init_tracing(servoshell_preferences.tracing_filter.as_deref());

    let Some(resource_dir) = env::current_exe()
        .ok()
        .and_then(|executable| Some(executable.parent()?.join("resources")))
        .filter(|resource_dir| resource_dir.join("named_resources").is_dir())
    else {
        error!("Could not find `resources/named_resources` next to the servoshell executable");
        process::exit(1);
    };
    resources::set_resource_dir(resource_dir);

    let event_loop = Arc::new(HeadlessEventLoop::default());
    let event_loop_waker: Box<dyn EventLoopWaker> =
        Box::new(HeadlessEventLoopWaker(event_loop.clone()));
    let servo = ServoBuilder::default()
        .opts(opts)
        .preferences(preferences.clone())
        .event_loop_waker(event_loop_waker.clone())
        .build();

    let user_content_manager = Rc::new(UserContentManager::new(&servo));
    for user_stylesheet in &servoshell_preferences.user_stylesheets {
        user_content_manager.add_stylesheet(user_stylesheet.clone());
    }

    let initial_url = servoshell_preferences
        .url
        .as_deref()
        .and_then(|url| Url::parse(url).ok())
        .or_else(|| Url::parse(&servoshell_preferences.homepage).ok())
        .unwrap_or_else(|| Url::parse("about:blank").expect("about:blank is a valid URL"));

    let state = Rc::new(RunningAppState::new(
        servo,
        servoshell_preferences,
        event_loop_waker,
        user_content_manager,
        preferences,
    ));
    let create_platform_window =
        |_: Url| -> Rc<dyn PlatformWindow> { HeadlessWindow::new(&state.servoshell_preferences) };
    state.open_window(
        create_platform_window(initial_url.clone()),
        TopLevelWebViewCreationRequest::WithUrl(initial_url),
    );
    event_loop.run(|| state.spin_event_loop(Some(&create_platform_window)));
}

/// Logs to stderr, filtered by `RUST_LOG`, like servoshell does on desktop platforms.
struct StderrLogger(env_filter::Filter);

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.0.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        if self.0.matches(record) {
            let _ = writeln!(
                std::io::stderr(),
                "[{} {}] {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

fn init_logging() {
    let filter = env_filter::Builder::from_env("RUST_LOG").build();
    log::set_max_level(filter.filter());
    let _ = log::set_boxed_logger(Box::new(StderrLogger(filter)));
}
