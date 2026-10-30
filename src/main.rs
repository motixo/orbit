// SPDX-License-Identifier: GPL-3.0-only

mod app;
mod i18n;

fn main() -> cosmic::iced::Result {
    let languages = i18n_embed::DesktopLanguageRequester::requested_languages();
    i18n::init(&languages);
    let settings = cosmic::app::Settings::default()
        .size(cosmic::iced::Size::new(440.0, 540.0))
        .size_limits(
            cosmic::iced::Limits::NONE
                .min_width(440.0)
                .max_width(440.0)
                .min_height(540.0)
                .max_height(540.0),
        );
    cosmic::app::run::<app::Orbit>(settings, ())
}
