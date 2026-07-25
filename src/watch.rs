//! Watch a config file for replacement.
//!
//! Watching the file itself is useless here: NixOS points it at the store and
//! swaps the symlink on every switch, so the inode being watched simply stops
//! being the config. Watching the parent directory catches the swap, and
//! editors that write-and-rename look identical.

use std::path::Path;
use std::sync::mpsc::Sender;

use inotify::{Inotify, WatchMask};
use log::{debug, warn};

/// Spawn a thread that reports whenever the config may have changed.
pub fn spawn(path: &Path, tx: Sender<()>) {
    let Some(dir) = path.parent().map(Path::to_path_buf) else {
        warn!("config path has no parent, live reload off");
        return;
    };
    let Some(name) = path.file_name().map(|n| n.to_os_string()) else {
        return;
    };

    std::thread::spawn(move || {
        let mut inotify = match Inotify::init() {
            Ok(inotify) => inotify,
            Err(err) => {
                warn!("cannot init inotify, live reload off: {err}");
                return;
            }
        };

        let mask = WatchMask::CREATE
            | WatchMask::MOVED_TO
            | WatchMask::DELETE
            | WatchMask::MODIFY
            | WatchMask::ATTRIB;

        if let Err(err) = inotify.watches().add(&dir, mask) {
            warn!(
                "cannot watch {}, live reload off: {err}",
                dir.display()
            );
            return;
        }

        let mut buffer = [0u8; 4096];
        loop {
            let events = match inotify.read_events_blocking(&mut buffer) {
                Ok(events) => events,
                Err(err) => {
                    warn!("inotify read failed, live reload off: {err}");
                    return;
                }
            };

            let touched = events
                .filter_map(|event| event.name.map(|n| n.to_os_string()))
                .any(|n| n == name);

            if touched {
                debug!("config touched, reloading");
                if tx.send(()).is_err() {
                    return;
                }
            }
        }
    });
}
