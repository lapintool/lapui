//! Native file notifications with bounded, interruptible debounce state.

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub(crate) struct WatchTarget {
    pub directory: PathBuf,
    pub file: Option<PathBuf>,
}

fn relevant(path: &Path, targets: &[WatchTarget]) -> bool {
    targets.iter().any(|target| {
        let Ok(relative) = path.strip_prefix(&target.directory) else {
            return false;
        };
        if let Some(file) = &target.file {
            return path == file;
        }
        !relative.components().any(|part| {
            matches!(
                part.as_os_str().to_str(),
                Some("node_modules" | "target" | "book" | ".git" | ".codex" | ".agents")
            )
        }) && !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.ends_with('~')
                    || name.ends_with(".swp")
                    || name.ends_with(".tmp")
                    || name.ends_with(".log")
            })
    })
}

#[derive(Default)]
struct Debounce {
    deadline: Option<Instant>,
    stopped: bool,
}

pub(crate) struct WatchSignal {
    pub ready: Arc<AtomicBool>,
    state: Arc<(Mutex<Debounce>, Condvar)>,
    watcher: Option<RecommendedWatcher>,
    worker: Option<JoinHandle<()>>,
}

impl WatchSignal {
    pub fn new(
        targets: Vec<WatchTarget>,
        wake: impl Fn() + Send + 'static,
    ) -> Result<Self, String> {
        let state = Arc::new((Mutex::new(Debounce::default()), Condvar::new()));
        let callback_state = state.clone();
        let targets = Arc::new(targets);
        let callback_targets = targets.clone();
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<Event>| match event {
                Ok(event)
                    if !matches!(event.kind, EventKind::Access(_))
                        && (event.need_rescan()
                            || event
                                .paths
                                .iter()
                                .any(|path| relevant(path, &callback_targets))) =>
                {
                    let (mutex, changed) = &*callback_state;
                    let mut pending = mutex.lock().unwrap();
                    if !pending.stopped {
                        pending.deadline = Some(Instant::now() + Duration::from_millis(150));
                        changed.notify_one();
                    }
                }
                Err(error) => eprintln!("Lapui watch error: {error}"),
                _ => {}
            },
            notify::Config::default().with_follow_symlinks(false),
        )
        .map_err(|error| error.to_string())?;
        for target in targets.iter() {
            watcher
                .watch(
                    &target.directory,
                    if target.file.is_some() {
                        RecursiveMode::NonRecursive
                    } else {
                        RecursiveMode::Recursive
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        let ready = Arc::new(AtomicBool::new(false));
        let worker_ready = ready.clone();
        let worker_state = state.clone();
        let worker = std::thread::Builder::new()
            .name("lapui-file-debounce".into())
            .spawn(move || {
                let (mutex, changed) = &*worker_state;
                let mut pending = mutex.lock().unwrap();
                loop {
                    if pending.stopped {
                        break;
                    }
                    match pending.deadline {
                        None => pending = changed.wait(pending).unwrap(),
                        Some(deadline) if Instant::now() < deadline => {
                            pending = changed
                                .wait_timeout(
                                    pending,
                                    deadline.saturating_duration_since(Instant::now()),
                                )
                                .unwrap()
                                .0;
                        }
                        Some(_) => {
                            pending.deadline = None;
                            drop(pending);
                            worker_ready.store(true, Ordering::Release);
                            wake();
                            pending = mutex.lock().unwrap();
                        }
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            ready,
            state,
            watcher: Some(watcher),
            worker: Some(worker),
        })
    }
}

impl Drop for WatchSignal {
    fn drop(&mut self) {
        let (mutex, changed) = &*self.state;
        mutex.lock().unwrap().stopped = true;
        changed.notify_one();
        self.watcher.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_watcher_coalesces_asset_writes_ignores_build_logs_and_stops_promptly() {
        let root = std::env::temp_dir().join(format!(
            "lapui-watch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let watch = WatchSignal::new(
            vec![WatchTarget {
                directory: root.clone(),
                file: None,
            }],
            move || {
                let _ = tx.send(());
            },
        )
        .unwrap();
        let file = root.join("page.html");
        for index in 0..10 {
            std::fs::write(&file, index.to_string()).unwrap();
        }
        rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(watch.ready.swap(false, Ordering::AcqRel));
        assert!(rx.recv_timeout(Duration::from_millis(250)).is_err());
        let log = root.join("build.log");
        std::fs::write(&log, "output").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(250)).is_err());
        let before = Instant::now();
        drop(watch);
        assert!(before.elapsed() < Duration::from_secs(1));
        std::fs::remove_file(file).unwrap();
        std::fs::remove_file(log).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
