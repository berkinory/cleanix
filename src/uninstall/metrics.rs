use super::{
    inventory::{self, Inventory},
    plan,
};
use crate::{filesystem, process};
use anyhow::Result;
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub fn scan(home: &Path, roots: &[PathBuf]) -> Result<Inventory> {
    let mut inventory = inventory::scan(home, roots)?;
    let results = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                let inventory = &inventory;
                scope.spawn(move || {
                    inventory
                        .apps
                        .iter()
                        .enumerate()
                        .skip(worker)
                        .step_by(4)
                        .map(|(index, app)| {
                            let bytes = plan::build(app, inventory)
                                .map(|p| p.entries.iter().map(|e| e.bytes).sum())
                                .or_else(|_| {
                                    filesystem::measure(
                                        std::slice::from_ref(&app.bundle),
                                        &[],
                                        &AtomicBool::new(false),
                                    )
                                    .map(|m| m.bytes)
                                })
                                .ok();
                            let last_used = process::query(
                                Path::new("/usr/bin/mdls"),
                                &[
                                    "-raw",
                                    "-name",
                                    "kMDItemLastUsedDate",
                                    app.bundle.path.to_str().unwrap_or(""),
                                ],
                            )
                            .ok()
                            .filter(|s| {
                                s.len() >= 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-'
                            });
                            (index, bytes, last_used)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("Application measurement worker failed"))
            })
            .collect::<Result<Vec<_>>>()
    })?;
    for (index, bytes, last_used) in results.into_iter().flatten() {
        inventory.apps[index].bytes = bytes;
        inventory.apps[index].last_used = if inventory.apps[index].running {
            Some("now".into())
        } else {
            last_used.map(|date| relative_date(&date))
        };
    }
    inventory.apps.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(inventory)
}

fn relative_date(value: &str) -> String {
    let format = time::format_description::parse_borrowed::<2>(
        "[year]-[month]-[day] [hour]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]",
    )
    .unwrap();
    let Ok(used) = time::OffsetDateTime::parse(value, &format) else {
        return "unknown".into();
    };
    fn local_date(timestamp: i64) -> Option<time::Date> {
        let timestamp = timestamp as libc::time_t;
        let mut local: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&timestamp, &mut local) }.is_null() {
            return None;
        }
        let month = time::Month::try_from((local.tm_mon + 1) as u8).ok()?;
        time::Date::from_calendar_date(local.tm_year + 1900, month, local.tm_mday as u8).ok()
    }
    let Some(date) = local_date(used.unix_timestamp()) else {
        return "unknown".into();
    };
    let Some(today) = local_date(time::OffsetDateTime::now_utc().unix_timestamp()) else {
        return "unknown".into();
    };
    match (today - date).whole_days() {
        ..=-1 => "unknown",
        0 => "today",
        1 => "yesterday",
        2..=7 => "last wk",
        8..=31 => "last mo",
        _ => "last yr",
    }
    .into()
}
