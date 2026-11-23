//! Checks feeds on a timer while the app is open, and on demand.
//!
//! One background thread waits for either the next scheduled check (the
//! interval comes from Settings and is re-read every time) or a "check now"
//! request. Pages that change are announced to the UI as `page-updated`
//! events; `poll-state` says when a check starts and finishes.

use persistwise_core::feeds::{self, AlreadyPolling, PollOptions};
use persistwise_core::settings;
use persistwise_core::store::Store;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub struct Poller {
    tx: Mutex<Sender<Option<i64>>>,
    pub running: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
struct PollState {
    running: bool,
    reports: Vec<feeds::FeedReport>,
    message: Option<String>,
}

impl Poller {
    pub fn start(app: AppHandle, store: Arc<Store>) -> Self {
        let (tx, rx) = channel::<Option<i64>>();
        let running = Arc::new(AtomicBool::new(false));
        let flag = running.clone();
        std::thread::spawn(move || {
            // First scheduled check shortly after start-up.
            let mut wait = Duration::from_secs(20);
            loop {
                let only = match rx.recv_timeout(wait) {
                    Ok(only) => Some(only),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                };
                let minutes = settings::load(store.root()).feeds.poll_minutes;
                wait = if minutes == 0 {
                    Duration::from_secs(3600)
                } else {
                    Duration::from_secs(minutes as u64 * 60)
                };
                // A timer tick with scheduled checks turned off does nothing.
                let target = match only {
                    Some(target) => target,
                    None if minutes == 0 => continue,
                    None => None,
                };
                if store.feeds().map(|f| f.is_empty()).unwrap_or(true) {
                    continue;
                }
                flag.store(true, Ordering::SeqCst);
                let _ = app.emit(
                    "poll-state",
                    PollState {
                        running: true,
                        reports: vec![],
                        message: None,
                    },
                );
                let opts = PollOptions::from_settings(&store);
                let app2 = app.clone();
                let store2 = store.clone();
                let mut on_change = move |id: i64| {
                    if let Ok(Some(page)) = store2.page(id) {
                        let _ = app2.emit("page-updated", page);
                    }
                };
                let result = feeds::poll_all(&store, target, &opts, &mut on_change);
                // Scheduled and "check all" runs also embed images into a few
                // more copies imported from the Persistwise website.
                if target.is_none() && result.is_ok() {
                    persistwise_core::localize_imported(&store, 25, &opts.capture, &mut on_change);
                }
                flag.store(false, Ordering::SeqCst);
                let state = match result {
                    Ok(reports) => PollState {
                        running: false,
                        reports,
                        message: None,
                    },
                    Err(e) if e.downcast_ref::<AlreadyPolling>().is_some() => PollState {
                        running: false,
                        reports: vec![],
                        message: Some(
                            "Feeds are being checked in the background right now.".into(),
                        ),
                    },
                    Err(e) => PollState {
                        running: false,
                        reports: vec![],
                        message: Some(format!("{e:#}")),
                    },
                };
                let _ = app.emit("poll-state", state);
            }
        });
        Self {
            tx: Mutex::new(tx),
            running,
        }
    }

    /// Asks for a check now: one feed, or all of them.
    pub fn check_now(&self, only: Option<i64>) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(only);
        }
    }
}
