use gpui::{App, AppContext, Context, Entity, SharedString, Window};

mod home;
mod native;
mod shell;
mod state;

use shell::Shell;
use state::AppState;

fn main() {
    App::new().run(|cx: &mut AppContext| {
        let app_state = cx.new(|_cx| AppState::new());

        cx.open_window(
            gpui::WindowOptions {
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some(SharedString::new_static("crabcc · live")),
                    appears_transparent: true,
                    traffic_light_position: Some(gpui::Point::new(
                        gpui::px(9.0),
                        gpui::px(9.0),
                    )),
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Shell::new(app_state, window, cx)),
        )
        .unwrap();
    });
}
