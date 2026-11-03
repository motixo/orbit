// SPDX-License-Identifier: GPL-3.0-only

mod install;

use crate::{config::ID, fl};
use cosmic::{
    cosmic_config::{Config, ConfigGet, ConfigSet},
    iced::{self, Length},
    prelude::*,
    widget,
};
use std::{fs::File, os::unix::fs::OpenOptionsExt, time::Duration};

fn completed() -> bool {
    Config::new_state(ID, 1)
        .and_then(|state| state.get_local("sharing_setup_completed"))
        .unwrap_or(false)
}

pub fn launch(manual: bool) {
    if !manual && completed() {
        return;
    }
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    match std::process::Command::new(executable.with_file_name("orbit"))
        .arg("--sharing-setup")
        .env_remove("WAYLAND_SOCKET")
        .env_remove("ORBIT_START_PINNED")
        .env("COSMIC_SINGLE_INSTANCE", "0")
        .spawn()
    {
        Ok(mut child) if manual => {
            std::thread::spawn(move || child.wait());
        }
        Ok(mut child) => {
            let _ = child.wait();
        }
        Err(error) => eprintln!("Could not open sharing setup: {error}"),
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("Missing runtime directory")?;
    let lease = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(std::path::PathBuf::from(runtime).join("orbit-sharing-setup.lock"))?;
    match lease.try_lock() {
        Ok(()) => (),
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(error) => return Err(std::io::Error::from(error).into()),
    }
    crate::i18n::init(&i18n_embed::DesktopLanguageRequester::requested_languages());
    cosmic::app::run::<Setup>(
        cosmic::app::Settings::default()
            .size(iced::Size::new(420.0, 340.0))
            .size_limits(iced::Limits::NONE.min_width(360.0).min_height(300.0)),
        lease,
    )?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Report {
    supported: bool,
    installed: bool,
    exclusive: bool,
    details: String,
}

async fn check() -> Result<Report, String> {
    let capabilities = tokio::task::spawn_blocking(|| {
        owly_radio::probe::from_config(owly_radio::rundir::RunDir::new(owly_radio::rundir::RUN_DIR))
    })
    .await
    .map_err(|_| fl!("sharing-setup-check-error"))?;
    let supported = capabilities.hardware
        && capabilities.card.is_some()
        && !capabilities
            .missing
            .iter()
            .any(|item| item.id == "hardware");
    let installed = installed().await;
    let details = capabilities
        .missing
        .iter()
        .map(|item| item.say.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Report {
        supported,
        installed,
        exclusive: matches!(
            capabilities.mode,
            Some(owly_radio::quirks::Mode::Exclusive | owly_radio::quirks::Mode::ActiveMonitor)
        ),
        details,
    })
}

async fn installed() -> bool {
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("owly")
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .is_ok_and(|result| result.is_ok_and(|output| output.status.success()))
}

struct Setup {
    core: cosmic::Core,
    _lease: File,
    report: Option<Report>,
    busy: bool,
    step: Option<install::Step>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Message {
    Check,
    Checked(Result<Report, String>),
    Install,
    Progress(install::Step),
    Installed(Result<(), String>),
    Finish,
}

impl cosmic::Application for Setup {
    type Executor = cosmic::executor::Default;
    type Flags = File;
    type Message = Message;
    const APP_ID: &'static str = "io.github.motixo.Orbit.Setup";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(mut core: cosmic::Core, lease: File) -> (Self, Task<cosmic::Action<Message>>) {
        core.window.show_maximize = false;
        core.set_header_title(fl!("sharing-setup-title"));
        let title = core.set_title(core.main_window_id(), fl!("sharing-setup-title"));
        let app = Self {
            core,
            _lease: lease,
            report: None,
            busy: true,
            step: None,
            error: None,
        };
        (
            app,
            Task::batch([
                title,
                cosmic::task::future(async { Message::Checked(check().await) }),
            ]),
        )
    }

    fn view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::active().cosmic().spacing;
        let mut body = widget::column::with_capacity(8).spacing(spacing.space_s);
        if let Some(report) = &self.report {
            body = body.push(widget::text::body(if report.supported {
                fl!("sharing-setup-supported")
            } else {
                fl!("sharing-setup-unsupported")
            }));
            if !report.details.is_empty() {
                body = body.push(widget::text::caption(&report.details));
            }
            if report.supported {
                if report.installed {
                    body = body.push(widget::text::body(fl!("sharing-setup-installed")));
                } else {
                    body = body.push(widget::text::caption(fl!("sharing-setup-hint")));
                    if install::builds_from_source() {
                        body = body.push(widget::text::caption(fl!("sharing-setup-source")));
                    }
                }
                if report.exclusive {
                    body = body.push(widget::text::caption(fl!("sharing-setup-exclusive")));
                }
            } else {
                body = body.push(widget::text::caption(fl!("sharing-setup-browser")));
            }
        }
        if self.busy {
            let label = self
                .step
                .map_or_else(|| fl!("sharing-setup-checking"), install::Step::label);
            body = body.push(widget::text::body(label));
            if let Some(install::Step::Downloading(progress)) = self.step {
                body = body.push(widget::progress_bar::determinate_linear(progress / 100.0));
            } else {
                body = body.push(widget::progress_bar::indeterminate_linear());
            }
        }
        if let Some(error) = &self.error {
            body = body.push(widget::text::body(error));
        }
        let needs_install = self
            .report
            .as_ref()
            .is_some_and(|r| r.supported && !r.installed);
        let mut buttons = widget::row::with_capacity(2).spacing(spacing.space_xs);
        if needs_install {
            buttons = buttons.push(
                widget::button::suggested(fl!("sharing-setup-install"))
                    .on_press_maybe((!self.busy).then_some(Message::Install)),
            );
        } else if self.error.is_some() {
            buttons = buttons.push(
                widget::button::standard(fl!("sharing-setup-retry"))
                    .on_press_maybe((!self.busy).then_some(Message::Check)),
            );
        }
        buttons = buttons.push(
            widget::button::standard(if needs_install {
                fl!("sharing-setup-later")
            } else {
                fl!("sharing-setup-done")
            })
            .on_press_maybe((!self.busy).then_some(Message::Finish)),
        );
        widget::container(
            widget::column::with_children(vec![
                widget::scrollable(body).height(Length::Fill).into(),
                buttons.into(),
            ])
            .spacing(spacing.space_s),
        )
        .padding(spacing.space_s)
        .into()
    }

    fn on_app_exit(&mut self) -> Option<Message> {
        Some(Message::Finish)
    }

    fn on_close_requested(&self, _: iced::window::Id) -> Option<Message> {
        Some(Message::Finish)
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Check => {
                self.error = None;
                self.step = None;
                self.busy = true;
                return cosmic::task::future(async { Message::Checked(check().await) });
            }
            Message::Checked(result) => {
                self.busy = false;
                match result {
                    Ok(report) => self.report = Some(report),
                    Err(error) => self.error = Some(error),
                }
            }
            Message::Install if !self.busy => {
                self.busy = true;
                self.error = None;
                self.step = Some(install::Step::Downloading(0.0));
                return cosmic::task::stream(iced::stream::channel(16, |mut output| async move {
                    use iced::futures::SinkExt;
                    let result = install::run(&mut output).await;
                    let _ = output.send(Message::Installed(result)).await;
                }));
            }
            Message::Progress(step) => self.step = Some(step),
            Message::Installed(result) => {
                self.busy = false;
                self.step = None;
                match result {
                    Ok(()) => return self.update(Message::Check),
                    Err(error) => self.error = Some(error),
                }
            }
            Message::Finish if !self.busy => {
                match Config::new_state(ID, 1)
                    .and_then(|state| state.set("sharing_setup_completed", true))
                {
                    Ok(()) => return iced::exit(),
                    Err(_) => self.error = Some(fl!("sharing-setup-state-error")),
                }
            }
            _ => (),
        }
        Task::none()
    }
}
