// SPDX-License-Identifier: GPL-3.0-only

use cosmic::iced::{
    self, Subscription,
    futures::{SinkExt, StreamExt, channel::mpsc},
};
use std::{
    collections::HashSet,
    hash::{DefaultHasher, Hash, Hasher},
};

fn prefix() -> String {
    let mut display = DefaultHasher::new();
    std::env::var_os("WAYLAND_DISPLAY").hash(&mut display);
    format!("io.github.motixo.Orbit.Shelf.s{:x}.", display.finish())
}

pub fn announce() -> Subscription<()> {
    Subscription::run(|| {
        iced::stream::channel(1, |_| async {
            if let Err(error) = hold_name().await {
                eprintln!("Could not announce open Orbit: {error}");
            }
        })
    })
}

async fn hold_name() -> zbus::Result<()> {
    let connection = zbus::Connection::session().await?;
    connection
        .request_name(format!("{}p{}", prefix(), crate::sandbox::process_id()))
        .await?;
    std::future::pending::<()>().await;
    drop(connection);
    Ok(())
}

pub fn count() -> Subscription<usize> {
    Subscription::run(|| {
        iced::stream::channel(1, |mut output| async move {
            if let Err(error) = listen(&mut output).await {
                eprintln!("Could not count open Orbits: {error}");
            }
        })
    })
}

async fn listen(output: &mut mpsc::Sender<usize>) -> zbus::Result<()> {
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::fdo::DBusProxy::new(&connection).await?;
    let mut changes = proxy.receive_name_owner_changed().await?;
    let prefix = prefix();
    let mut names: HashSet<String> = proxy
        .list_names()
        .await?
        .into_iter()
        .filter(|name| name.starts_with(&prefix))
        .map(|name| name.to_string())
        .collect();
    if output.send(names.len()).await.is_err() {
        return Ok(());
    }
    while let Some(change) = changes.next().await {
        let args = change.args()?;
        if update(&mut names, &prefix, args.name(), args.new_owner().is_some())
            && output.send(names.len()).await.is_err()
        {
            break;
        }
    }
    Ok(())
}

fn update(names: &mut HashSet<String>, prefix: &str, name: &str, present: bool) -> bool {
    if !name.starts_with(prefix) {
        return false;
    }
    if present {
        names.insert(name.to_owned())
    } else {
        names.remove(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_shelves_without_duplicates_or_other_displays() {
        let prefix = "io.github.motixo.Orbit.Shelf.s1.";
        let first = format!("{prefix}p1");
        let second = format!("{prefix}p2");
        let mut names = HashSet::from([first.clone()]);
        assert!(!update(&mut names, prefix, &first, true));
        assert!(!update(
            &mut names,
            prefix,
            "io.github.motixo.Orbit.Applet",
            true
        ));
        assert!(!update(
            &mut names,
            prefix,
            "io.github.motixo.Orbit.Shelf.s2.p1",
            true
        ));
        assert!(update(&mut names, prefix, &second, true));
        assert_eq!(names.len(), 2);
        assert!(update(&mut names, prefix, &first, false));
        assert!(!update(&mut names, prefix, &first, false));
        assert_eq!(names, HashSet::from([second.clone()]));
        assert!(update(&mut names, prefix, &second, false));
        assert!(names.is_empty());
    }
}
