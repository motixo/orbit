// SPDX-License-Identifier: GPL-3.0-only

use crate::share::Bundle;
use cosmic::dialog::ashpd::{
    AppID,
    documents::{Documents, Permission},
};
use serde::{Deserialize, Serialize};
use std::{fs::File, path::PathBuf};

const SERVICE: &str = "io.github.hepp3n.kdeconnect";

#[derive(Clone, Debug, Deserialize, Serialize, zbus::zvariant::Type)]
pub struct Device {
    pub id: String,
    pub name: String,
    device_type: String,
    paired: bool,
    reachable: bool,
}

#[zbus::proxy(
    interface = "io.github.hepp3n.kdeconnect.Daemon",
    default_service = "io.github.hepp3n.kdeconnect",
    default_path = "/io/github/hepp3n/kdeconnect/Daemon"
)]
trait Daemon {
    fn list_devices(&self) -> zbus::Result<Vec<Device>>;
    fn send_files(&self, device_id: &str, files: Vec<String>) -> zbus::Result<()>;
}

pub async fn devices() -> Result<Vec<Device>, String> {
    let connection = zbus::Connection::session()
        .await
        .map_err(|e| e.to_string())?;
    let proxy = DaemonProxy::new(&connection)
        .await
        .map_err(|e| e.to_string())?;
    let mut devices = proxy.list_devices().await.map_err(|e| e.to_string())?;
    devices.retain(|device| device.paired && device.reachable);
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(devices)
}

pub async fn send(device: &str, bundle: &Bundle) -> Result<(), String> {
    send_inner(device, bundle)
        .await
        .map_err(|error| error.to_string())
}

async fn send_inner(
    device: &str,
    bundle: &Bundle,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let connection = zbus::Connection::session().await?;
    let proxy = DaemonProxy::new(&connection).await?;
    if !proxy
        .list_devices()
        .await?
        .iter()
        .any(|d| d.id == device && d.paired && d.reachable)
    {
        return Err("Device is offline".into());
    }
    let bus = zbus::fdo::DBusProxy::new(&connection).await?;
    let pid = bus
        .get_connection_unix_process_id(SERVICE.try_into()?)
        .await?;
    let sandboxed = match std::fs::metadata(format!("/proc/{pid}/root/.flatpak-info")) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let mut paths = bundle
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let mut exported = None;
    if sandboxed {
        let documents = Documents::new().await?;
        let app_id: AppID = SERVICE.parse()?;
        let files = paths
            .iter()
            .map(File::open)
            .collect::<Result<Vec<_>, _>>()?;
        let mount: PathBuf = documents.mount_point().await?.as_ref().into();
        let (ids, _) = documents
            .add_full(
                &files,
                Default::default(),
                Some(&app_id),
                &[Permission::Read],
            )
            .await?;
        paths = paths
            .iter()
            .zip(&ids)
            .map(|(path, id)| mount.join(id.as_ref()).join(path.file_name().unwrap()))
            .collect();
        exported = Some((documents, ids));
    }
    let paths = paths
        .iter()
        .map(|path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or("File name is not UTF-8")
        })
        .collect::<Result<Vec<_>, _>>()?;
    // SendFiles only queues work; keep session grants and cached files for the receiver.
    bundle.hand_off();
    if let Err(error) = proxy.send_files(device, paths).await {
        if let Some((documents, ids)) = exported {
            for id in ids {
                let _ = documents.delete(id).await;
            }
        }
        return Err(error.into());
    }
    Ok(())
}
