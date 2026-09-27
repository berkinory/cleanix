use super::{
    inventory::Inventory,
    plan::{self, Plan},
    view,
};
use crate::config::Config;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;
use std::{
    collections::HashSet,
    io::IsTerminal,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::Duration,
};

pub(super) enum Message {
    Inventory(Result<Inventory>),
    Plans(Result<Vec<Plan>>),
    Finished(Result<()>),
}
#[derive(PartialEq)]
pub(super) enum Page {
    Apps,
    Confirm,
}
pub(super) struct Screen {
    pub config: Config,
    pub inventory: Option<Inventory>,
    pub plans: Vec<Plan>,
    pub selected: HashSet<usize>,
    pub cursor: usize,
    pub state: ListState,
    pub page: Page,
    pub query: String,
    pub searching: bool,
    pub confirmation: String,
    pub confirm_scroll: u16,
    pub status: String,
    pub busy: bool,
    pub deleting: bool,
    pub completed: String,
    rx: Receiver<Message>,
}
impl Screen {
    fn new(config: Config) -> Self {
        let (_, rx) = mpsc::channel();
        let mut s = Self {
            config,
            inventory: None,
            plans: vec![],
            selected: HashSet::new(),
            cursor: 0,
            state: ListState::default(),
            page: Page::Apps,
            query: String::new(),
            searching: false,
            confirmation: String::new(),
            confirm_scroll: 0,
            status: String::new(),
            busy: false,
            deleting: false,
            completed: String::new(),
            rx,
        };
        s.scan();
        s
    }
    fn work(&mut self, task: impl FnOnce() -> Message + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        self.rx = rx;
        self.busy = true;
        thread::spawn(move || {
            let _ = tx.send(task());
        });
    }
    fn scan(&mut self) {
        let home = self.config.home.clone();
        self.page = Page::Apps;
        self.plans.clear();
        self.inventory = None;
        self.selected.clear();
        self.cursor = 0;
        self.state = ListState::default();

        self.status = "Reading installed applications…".into();
        self.work(move || Message::Inventory(super::metrics::scan(&home, &super::roots(&home))));
    }
    pub fn visible(&self) -> Vec<usize> {
        self.inventory
            .as_ref()
            .map(|inventory| {
                inventory
                    .apps
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| {
                        a.name.to_lowercase().contains(&self.query.to_lowercase())
                            || a.bundle_id
                                .to_lowercase()
                                .contains(&self.query.to_lowercase())
                    })
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default()
    }
    fn drain(&mut self) -> bool {
        let Ok(message) = self.rx.try_recv() else {
            return false;
        };
        self.busy = false;
        self.deleting = false;
        match message {
            Message::Inventory(Ok(inventory)) => {
                self.inventory = Some(inventory);
                self.status = if self.completed.is_empty() {
                    String::new()
                } else {
                    self.completed.clone()
                };
            }
            Message::Plans(Ok(plans)) => {
                self.plans = plans;
                self.page = Page::Confirm;
                self.confirmation.clear();
                self.confirm_scroll = 0;
                self.status.clear();
            }
            Message::Finished(Ok(())) => {
                self.completed = if self.config.dry {
                    "Dry check complete. Nothing removed."
                } else {
                    if self.plans.iter().any(|p| p.app.system_extensions) { "Apps removed. System extensions may remain; check Login Items & Extensions in System Settings." } else { "Selected applications removed." }
                }
                .into();
                self.scan();
            }
            Message::Inventory(Err(e)) | Message::Plans(Err(e)) | Message::Finished(Err(e)) => {
                self.status = format!("{e:#}");
                self.page = Page::Apps;
            }
        }
        true
    }
    pub fn confirmation_phrase(&self) -> String {
        if self.plans.len() == 1 {
            self.plans[0].app.name.clone()
        } else {
            format!("REMOVE {}", self.plans.len())
        }
    }
    fn key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return !self.deleting;
        }
        if self.busy {
            return key.code == KeyCode::Char('q') && !self.deleting;
        }
        if self.searching {
            match key.code {
                KeyCode::Esc => {
                    self.searching = false;
                    self.query.clear();
                }
                KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    self.query.pop();
                }
                KeyCode::Char(c) => self.query.push(c),
                _ => {}
            }
            self.cursor = 0;
            return false;
        }
        if self.page == Page::Confirm {
            match key.code {
                KeyCode::PageDown => {
                    self.confirm_scroll = self
                        .confirm_scroll
                        .saturating_add(4)
                        .min(self.plans.len() as u16)
                }
                KeyCode::PageUp => self.confirm_scroll = self.confirm_scroll.saturating_sub(4),
                KeyCode::Esc => {
                    self.page = Page::Apps;
                    self.confirmation.clear();
                }
                KeyCode::Backspace => {
                    self.confirmation.pop();
                }
                KeyCode::Char(c) => self.confirmation.push(c),
                KeyCode::Enter if self.confirmation == self.confirmation_phrase() => {
                    let plans = self.plans.clone();
                    let dry = self.config.dry;
                    self.status = if dry {
                        "Validating…"
                    } else {
                        "Uninstalling…"
                    }
                    .into();
                    self.deleting = true;
                    self.work(move || {
                        Message::Finished((|| {
                            for plan in &plans {
                                super::execution::validate(
                                    plan,
                                    &(0..plan.entries.len()).collect(),
                                )?;
                            }
                            let mut completed = Vec::new();
                            for plan in &plans {
                                if let Err(error) =
                                    plan::execute(plan, &(0..plan.entries.len()).collect(), dry)
                                {
                                    anyhow::bail!(
                                        "{}: {error:#}. Completed: {}",
                                        plan.app.name,
                                        if completed.is_empty() {
                                            "none".into()
                                        } else {
                                            completed.join(", ")
                                        }
                                    );
                                }
                                completed.push(plan.app.name.clone());
                            }
                            Ok(())
                        })())
                    });
                }
                _ => {}
            }
            return false;
        }
        let len = self.visible().len();
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(len.saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char('r') => {
                self.completed.clear();
                self.scan();
            }
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Esc => self.query.clear(),
            KeyCode::Char(' ') | KeyCode::Enter => {
                if let Some(&index) = self.visible().get(self.cursor) {
                    let app = &self.inventory.as_ref().unwrap().apps[index];
                    if let Some(reason) = &app.blocked {
                        self.status = reason.clone();
                    } else if !self.selected.remove(&index) {
                        self.selected.insert(index);
                    }
                }
            }
            KeyCode::Char('d') if !self.selected.is_empty() => {
                let inventory = self.inventory.clone().unwrap();
                let mut selected: Vec<_> = self.selected.iter().copied().collect();
                selected.sort_unstable();
                self.status = "Preparing confirmation…".into();
                self.work(move || {
                    Message::Plans(
                        selected
                            .into_iter()
                            .map(|i| plan::build(&inventory.apps[i], &inventory))
                            .collect(),
                    )
                });
            }
            _ => {}
        }
        false
    }
}
pub fn run(config: Config) -> Result<()> {
    anyhow::ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "Use cleanix uninstall --scan or --json outside an interactive terminal"
    );
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, stop.clone())?;
    let mut terminal = ratatui::init();
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            ratatui::restore();
        }
    }
    let _restore = Restore;
    let mut screen = Screen::new(config);
    let mut dirty = true;
    loop {
        dirty |= screen.drain();
        if stop.load(Ordering::Relaxed) && !screen.deleting {
            break;
        }
        if dirty {
            terminal.draw(|f| view::draw(f, &mut screen))?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if screen.key(key) {
                    break;
                }
                dirty = true;
            }
            Event::Resize(..) => dirty = true,
            _ => {}
        }
    }
    Ok(())
}
