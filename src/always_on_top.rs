// SPDX-License-Identifier: GPL-3.0-only

use cosmic::Application;
use cosmic::cctk::{
    self,
    cosmic_protocols::{
        toplevel_info::v1::client::zcosmic_toplevel_handle_v1::State as WindowState,
        toplevel_management::v1::client::zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1 as Capability,
    },
    sctk::{
        self,
        output::{OutputHandler, OutputState},
        registry::{ProvidesRegistryState, RegistryState},
    },
    toplevel_info::{ToplevelInfoHandler, ToplevelInfoState},
    toplevel_management::{ToplevelManagerHandler, ToplevelManagerState},
    wayland_client::{
        Connection, Proxy, QueueHandle, WEnum, globals::registry_queue_init,
        protocol::wl_output::WlOutput,
    },
    wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
};

pub fn start() -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("orbit-on-top".into())
        .spawn(|| {
            if let Err(error) = run() {
                eprintln!("Could not keep Orbit on top: {error}");
            }
        })
        .map(|_| ())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();
    let registry = RegistryState::new(&globals);
    let manager = ToplevelManagerState::try_new(&registry, &qh)
        .ok_or("COSMIC toplevel management is unavailable")?;
    let info =
        ToplevelInfoState::try_new(&registry, &qh).ok_or("Toplevel information is unavailable")?;
    let mut state = State {
        output: OutputState::new(&globals, &qh),
        registry,
        manager,
        info,
        sticky_supported: false,
    };
    loop {
        queue.blocking_dispatch(&mut state)?;
    }
}

struct State {
    registry: RegistryState,
    output: OutputState,
    info: ToplevelInfoState,
    manager: ToplevelManagerState,
    sticky_supported: bool,
}

impl State {
    fn keep_above(&self) {
        if !self.sticky_supported {
            return;
        }
        for info in self.info.toplevels() {
            if info.app_id == crate::app::Orbit::APP_ID
                && !info.state.contains(&WindowState::Sticky)
                && let Some(toplevel) = &info.cosmic_toplevel
            {
                // COSMIC's sticky layer stays above workspace windows on every workspace.
                self.manager.manager.set_sticky(toplevel);
            }
        }
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    sctk::registry_handlers!(OutputState);
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl ToplevelManagerHandler for State {
    fn toplevel_manager_state(&mut self) -> &mut ToplevelManagerState {
        &mut self.manager
    }

    fn capabilities(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        capabilities: Vec<WEnum<Capability>>,
    ) {
        // COSMIC implements set_sticky since manager v3 but can omit its capability.
        self.sticky_supported = capabilities.contains(&WEnum::Value(Capability::Sticky))
            || self.manager.manager.version() >= 3;
        self.keep_above();
    }
}

impl ToplevelInfoHandler for State {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.info
    }

    fn new_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ExtForeignToplevelHandleV1,
    ) {
        self.keep_above();
    }

    fn update_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ExtForeignToplevelHandleV1,
    ) {
        self.keep_above();
    }

    fn toplevel_closed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ExtForeignToplevelHandleV1,
    ) {
    }
}

sctk::delegate_registry!(State);
sctk::delegate_output!(State);
cctk::delegate_toplevel_info!(State);
cctk::delegate_toplevel_manager!(State);
