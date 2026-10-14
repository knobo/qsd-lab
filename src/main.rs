mod lab;
mod log_view;
mod qmp;

use eframe::egui::{self, Color32};
use lab::{Image, Lab, Log, log};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct App {
    labs: Vec<Lab>,
    selected: Option<String>,
    chain: Vec<Image>,
    new_name: String,
    new_mount: String,
    new_size: u64,
    confirm_delete: bool,
    problems: Vec<String>,
    log: Log,
    /// Lines of the selected lab's events.log; shown instead of the session log when `show_history`.
    history: Vec<String>,
    show_history: bool,
    explain: bool,
    raw_json: bool,
    handshake: bool,
    busy: Arc<AtomicBool>,
    dirty: Arc<AtomicBool>,
    ctx: egui::Context,
}

impl App {
    fn new(ctx: egui::Context) -> Self {
        let mut app = App {
            labs: Vec::new(),
            selected: None,
            chain: Vec::new(),
            new_name: String::new(),
            new_mount: String::new(),
            new_size: 4,
            confirm_delete: false,
            problems: lab::prereq_problems(),
            log: Log::default(),
            history: Vec::new(),
            show_history: true,
            explain: true,
            raw_json: false,
            handshake: false,
            busy: Arc::default(),
            dirty: Arc::default(),
            ctx,
        };
        app.reload();
        app
    }

    fn current(&self) -> Option<&Lab> {
        let name = self.selected.as_ref()?;
        self.labs.iter().find(|l| &l.name == name)
    }

    fn reload(&mut self) {
        self.labs = Lab::load_all();
        if self.current().is_none() {
            self.selected = None;
        }
        self.refresh_chain();
        self.refresh_history();
    }

    fn refresh_history(&mut self) {
        self.history = self
            .current()
            .and_then(|l| std::fs::read_to_string(l.events_log()).ok())
            .map(|s| {
                let lines: Vec<&str> = s.lines().collect();
                // ponytail: re-reads the whole file; tail it if logs get huge.
                lines[lines.len().saturating_sub(5000)..].iter().map(|l| l.to_string()).collect()
            })
            .unwrap_or_default();
    }

    fn refresh_chain(&mut self) {
        self.chain = match self.current() {
            // Throwaway log: refreshing the table is not an event worth recording.
            Some(l) => l.chain(&Log::default()).unwrap_or_else(|e| {
                log(&self.log, format!("ERROR: {e}"));
                Vec::new()
            }),
            None => Vec::new(),
        };
    }

    fn select(&mut self, name: String) {
        if self.selected.as_ref() != Some(&name) {
            self.selected = Some(name);
            self.confirm_delete = false;
            self.refresh_chain();
            self.refresh_history();
        }
    }

    fn spawn(
        &self,
        title: &str,
        lab_name: &str,
        f: impl FnOnce(&Log) -> Result<(), String> + Send + 'static,
    ) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (log_, busy, dirty, ctx) = (
            self.log.to_file(lab::data_root().join(lab_name).join("events.log")),
            self.busy.clone(),
            self.dirty.clone(),
            self.ctx.clone(),
        );
        let title = title.to_string();
        std::thread::spawn(move || {
            log(&log_, format!("== {title} =="));
            if let Err(e) = f(&log_) {
                log(&log_, format!("ERROR: {e}"));
            }
            busy.store(false, Ordering::SeqCst);
            dirty.store(true, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }

    fn problems_ui(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(Color32::from_rgb(80, 20, 20))
            .inner_margin(8)
            .show(ui, |ui| {
                for p in &self.problems {
                    ui.colored_label(Color32::from_rgb(255, 170, 170), p);
                }
                if ui.button("Check again").clicked() {
                    self.problems = lab::prereq_problems();
                }
            });
    }

    fn side_ui(&mut self, ui: &mut egui::Ui, busy: bool) {
        ui.heading("Labs");
        let mut clicked = None;
        for l in &self.labs {
            ui.horizontal(|ui| {
                let color = match (l.qsd_running(), l.mounted()) {
                    (true, true) => Color32::GREEN,
                    (false, false) => Color32::GRAY,
                    _ => Color32::YELLOW,
                };
                let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 5.0, color);
                let sel = self.selected.as_ref() == Some(&l.name);
                if ui.selectable_label(sel, &l.name).clicked() {
                    clicked = Some(l.name.clone());
                }
            });
        }
        if let Some(name) = clicked {
            self.select(name);
        }

        ui.separator();
        ui.heading("New lab");
        ui.label("Name");
        ui.text_edit_singleline(&mut self.new_name);
        ui.label("Mountpoint (empty directory)");
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.new_mount);
            if ui.button("Choose…").clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    self.new_mount = p.display().to_string();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Size");
            ui.add(egui::DragValue::new(&mut self.new_size).range(1..=1024).suffix(" GiB"));
        });
        let ok = !busy && !self.new_name.trim().is_empty() && !self.new_mount.trim().is_empty();
        if ui.add_enabled(ok, egui::Button::new("Create")).clicked() {
            let name = self.new_name.trim().to_string();
            let mount = PathBuf::from(self.new_mount.trim());
            let size = self.new_size;
            self.selected = Some(name.clone());
            self.spawn(&format!("create {name}"), &name.clone(), move |log| {
                Lab::create(&name, &mount, size, log).map(|_| ())
            });
        }
    }

    fn lab_ui(&mut self, ui: &mut egui::Ui, busy: bool) {
        let Some(l) = self.current().cloned() else {
            ui.label("Select or create a lab.");
            return;
        };
        let (running, mounted) = (l.qsd_running(), l.mounted());
        let stopped = !running && !mounted;
        let yes_no = |b| if b { "yes" } else { "no" };

        ui.heading(&l.name);
        egui::Grid::new("info").num_columns(2).show(ui, |ui| {
            ui.label("Mountpoint");
            ui.label(l.mountpoint.display().to_string());
            ui.end_row();
            ui.label("Size");
            ui.label(format!("{} GiB", l.size_gib));
            ui.end_row();
            ui.label("qsd running");
            ui.label(yes_no(running));
            ui.end_row();
            ui.label("Mounted");
            ui.label(yes_no(mounted));
            ui.end_row();
        });

        ui.horizontal(|ui| {
            if !running && ui.add_enabled(!busy, egui::Button::new("Start")).clicked() {
                let mut l = l.clone();
                self.spawn(&format!("start {}", l.name), &l.name.clone(), move |log| l.start(log));
            }
            if (running || mounted) && ui.add_enabled(!busy, egui::Button::new("Stop")).clicked() {
                let l = l.clone();
                self.spawn(&format!("stop {}", l.name), &l.name.clone(), move |log| l.stop(log));
            }
            if ui.add_enabled(!busy, egui::Button::new("Snapshot")).clicked() {
                let mut l = l.clone();
                self.spawn(&format!("snapshot {}", l.name), &l.name.clone(), move |log| l.snapshot(log));
            }
            let text = if self.confirm_delete { "Click again to confirm" } else { "Delete" };
            if stopped && ui.add_enabled(!busy, egui::Button::new(text)).clicked() {
                if self.confirm_delete {
                    self.confirm_delete = false;
                    let l = l.clone();
                    self.spawn(&format!("delete {}", l.name), &l.name.clone(), move |log| l.delete(log));
                } else {
                    self.confirm_delete = true;
                }
            }
        });

        ui.separator();
        ui.heading("Backing chain");
        ui.label(
            "Snapshot freezes the current top image and puts a new empty overlay on top. \
             Rollback puts a new overlay on top of an older snapshot.",
        );
        let mut rollback = None;
        egui::Grid::new("chain").num_columns(4).striped(true).show(ui, |ui| {
            for img in &self.chain {
                ui.monospace(&img.file);
                ui.label(if img.active { "active" } else { "snapshot" });
                ui.label(format_age(img.age_secs));
                if !img.active
                    && ui.add_enabled(!busy && stopped, egui::Button::new("Rollback to this")).clicked()
                {
                    rollback = Some(img.file.clone());
                }
                ui.end_row();
            }
        });
        if let Some(file) = rollback {
            let mut l = l.clone();
            self.spawn(&format!("rollback {} to {file}", l.name), &l.name.clone(), move |log| l.rollback(&file, log));
        }
    }

    fn log_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Log");
            ui.selectable_value(&mut self.show_history, true, "Lab history")
                .on_hover_text("events.log of the selected lab, kept across sessions");
            ui.selectable_value(&mut self.show_history, false, "Session")
                .on_hover_text("Everything since the app started, all labs");
            ui.separator();
            ui.checkbox(&mut self.explain, "Explain");
            ui.checkbox(&mut self.raw_json, "Raw JSON");
            ui.checkbox(&mut self.handshake, "QMP handshake");
            if !self.show_history && ui.button("Clear").clicked() {
                self.log.clear();
            }
        });
        let lines = if self.show_history { self.history.clone() } else { self.log.lines() };
        let opts = (self.explain, self.raw_json, self.handshake);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(ui, |ui| log_view::show(ui, &lines, opts));
    }
}

fn format_age(s: u64) -> String {
    match s {
        0..60 => format!("{s}s ago"),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.dirty.swap(false, Ordering::SeqCst) {
            self.reload();
        }
        let busy = self.busy.load(Ordering::SeqCst);
        if busy {
            // Follow the worker as it writes events.log.
            self.refresh_history();
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }

        if !self.problems.is_empty() {
            egui::Panel::top("problems").show(ui, |ui| self.problems_ui(ui));
        }
        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(220.0)
            .show(ui, |ui| self.log_ui(ui));
        egui::Panel::left("labs")
            .resizable(true)
            .show(ui, |ui| self.side_ui(ui, busy));
        egui::CentralPanel::default().show(ui, |ui| self.lab_ui(ui, busy));

        ui.ctx().request_repaint_after(Duration::from_secs(2));
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("qsd-lab")
            .with_app_id("qsd-lab")
            .with_inner_size([1000.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "qsd-lab",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc.egui_ctx.clone())))),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn age() {
        assert_eq!(super::format_age(12), "12s ago");
        assert_eq!(super::format_age(300), "5m ago");
        assert_eq!(super::format_age(3 * 3600), "3h ago");
        assert_eq!(super::format_age(2 * 86400 + 5), "2d ago");
    }
}
