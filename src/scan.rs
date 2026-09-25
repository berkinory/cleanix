use crate::{
    config::Config,
    filesystem, managed,
    model::{Action, Item, Report},
    providers,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Instant,
};
#[derive(Debug)]
pub enum Event {
    Item(Box<Item>),
    Note(String),
    AccessWarning(String),
    Found(usize),
    Complete(f64),
    Cleaned(String, Result<(), String>),
    CleanupDone,
}
pub fn start(c: Config) -> (Receiver<Event>, Arc<AtomicBool>) {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    thread::spawn(move || {
        let start = Instant::now();
        if let Some(warning) = crate::platform::disk_access_warning(&c.home) {
            let _ = tx.send(Event::AccessWarning(warning));
        }
        thread::scope(|scope| {
            let txm = tx.clone();
            let cm = &c;
            let sm = &stop;
            scope.spawn(move || {
                thread::scope(|scope| {
                    for provider in 0..4 {
                        let tx = txm.clone();
                        scope.spawn(move || {
                            let (items, notes) = match provider {
                                0 => {
                                    #[cfg(target_os = "macos")]
                                    {
                                        managed::simulators(cm, sm)
                                    }
                                    #[cfg(target_os = "linux")]
                                    {
                                        (vec![], vec![])
                                    }
                                }
                                1 => crate::docker::discover(cm),
                                2 => managed::android(cm, sm),
                                _ => {
                                    #[cfg(target_os = "macos")]
                                    {
                                        crate::stores::snapshots()
                                    }
                                    #[cfg(target_os = "linux")]
                                    {
                                        (vec![], vec![])
                                    }
                                }
                            };
                            let _ = tx.send(Event::Found(items.len()));
                            for n in notes {
                                let _ = tx.send(Event::Note(n));
                            }
                            for i in items {
                                if !i.action.paths().iter().any(|p| {
                                    cm.ignore.iter().any(|ignored| {
                                        p.starts_with(ignored) || ignored.starts_with(p)
                                    })
                                }) && above_threshold(&i, cm.min_bytes)
                                {
                                    let _ = tx.send(Event::Item(Box::new(i)));
                                }
                            }
                        });
                    }
                });
            });
            let (items, notes) = providers::discover(&c, &stop);
            let _ = tx.send(Event::Found(items.len()));
            for n in notes {
                let _ = tx.send(Event::Note(n));
            }
            let index = AtomicUsize::new(0);
            thread::scope(|scope| {
                for _ in 0..4 {
                    let tx = &tx;
                    let items = &items;
                    let index = &index;
                    let stop = &stop;
                    let c = &c;
                    scope.spawn(move || {
                        loop {
                            let n = index.fetch_add(1, Ordering::Relaxed);
                            let Some(item) = items.get(n) else { break };
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                            if let Some(i) = measure_item(item.clone(), stop, tx)
                                && above_threshold(&i, c.min_bytes)
                            {
                                let _ = tx.send(Event::Item(Box::new(i)));
                            }
                        }
                    });
                }
            });
        });
        let _ = tx.send(Event::Complete(start.elapsed().as_secs_f64()));
    });
    (rx, cancel)
}
fn measure_item(mut i: Item, cancel: &AtomicBool, tx: &Sender<Event>) -> Option<Item> {
    let (Action::AgedFiles { targets, .. }
    | Action::DeletePaths { targets }
    | Action::UnifiedLogs { targets }
    | Action::InspectStorage { targets, .. }) = &mut i.action
    else {
        return Some(i);
    };
    let mut bytes = 0;
    let mut files = 0;
    let mut duplicate_bytes = 0;
    targets.retain(
        |t| match filesystem::measure(std::slice::from_ref(t), &i.excludes, cancel) {
            Ok(m) => {
                bytes += m.bytes;
                files += m.files;
                duplicate_bytes += m.hardlink_duplicates_bytes;
                true
            }
            Err(e) => {
                let _ = tx.send(Event::Note(format!("Skipped {}: {e}", t.path.display())));
                false
            }
        },
    );
    if targets.is_empty() {
        return None;
    }
    if let Action::InspectStorage {
        allocated_bytes, ..
    } = &mut i.action
    {
        *allocated_bytes = bytes;
        i.detail.push_str(&format!(
            " Allocated backing storage: {}. This is not a reclaimable-space estimate.",
            crate::model::size(bytes)
        ));
        i.bytes = None;
    } else {
        i.bytes = Some(bytes);
    }
    i.files = files;
    if duplicate_bytes > 0 {
        i.detail.push_str(&format!(" Hardlinks counted once: {} of duplicate block references excluded. This is allocated usage, not guaranteed reclaimable space.",crate::model::size(duplicate_bytes)));
    }
    Some(i)
}
pub fn collect(c: Config) -> Report {
    let mut r = Report {
        roots: c.roots.clone(),
        ..Report::default()
    };
    let (rx, _) = start(c);
    for e in rx {
        match e {
            Event::Item(i) => r.items.push(*i),
            Event::Note(n) => r.notes.push(n),
            Event::AccessWarning(warning) => {
                r.notes.push(warning.clone());
                r.access_warning = Some(warning);
            }
            Event::Complete(t) => {
                r.elapsed_seconds = t;
                r.complete = true;
                break;
            }
            _ => {}
        }
    }
    r.items.sort_by_key(|i| std::cmp::Reverse(i.bytes));
    r
}

pub fn above_threshold(item: &Item, min: u64) -> bool {
    match &item.action {
        Action::Docker { .. } | Action::DockerBuilder { .. } | Action::DockerVolumes { .. } => {
            item.bytes.is_none_or(|b| b > 0)
        }
        Action::InspectStorage {
            allocated_bytes, ..
        } => *allocated_bytes >= min,
        _ => item.bytes.is_none_or(|b| b >= min),
    }
}
