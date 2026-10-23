mod agent;
mod attachments;
mod keychain;
mod mcp;
mod provider;
mod updates;

use std::{collections::VecDeque, fs, path::PathBuf, sync::mpsc::{self, Receiver, Sender}, thread};
use eframe::egui::{self, Color32, RichText, ScrollArea, TextEdit};
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};
use agent::{Event, MAX_IMPLEMENTERS};
use attachments::{Attachment, MAX_ATTACHMENTS, MAX_TOTAL_IMAGE_BYTES};
use mcp::{ServerConfig, Transport};
use provider::{ChatMessage, Provider, Settings, ToolCall};

#[derive(Clone, Serialize, Deserialize)]
struct ProviderEntry { url: String, model: String }

#[derive(Clone, Serialize, Deserialize)]
struct RoleModel { provider: Provider, model: String }

#[derive(Clone, Serialize, Deserialize)]
struct ImplementerGroup { role: RoleModel, count: usize }

#[derive(Clone, Serialize, Deserialize)]
struct Config {
    selected: Provider,
    providers: Vec<ProviderEntry>,
    servers: Vec<ServerConfig>,
    #[serde(default)]
    auto_approve_mcp: bool,
    #[serde(default = "default_commander_mode")]
    commander_mode: bool,
    #[serde(default)]
    commander: Option<RoleModel>,
    #[serde(default)]
    implementer_groups: Option<Vec<ImplementerGroup>>,
    #[serde(default, rename = "implementer_count", skip_serializing)]
    legacy_implementer_count: Option<usize>,
}

fn default_commander_mode() -> bool { true }

impl Default for Config {
    fn default() -> Self {
        Self {
            selected: Provider::Ollama,
            providers: Provider::ALL.into_iter().map(|p| ProviderEntry {
                url: p.default_url().into(), model: p.default_model().into(),
            }).collect(),
            servers: vec![],
            auto_approve_mcp: false,
            commander_mode: true,
            commander: None,
            implementer_groups: None,
            legacy_implementer_count: None,
        }
    }
}

impl Config {
    fn normalize(&mut self) {
        for provider in Provider::ALL.into_iter().skip(self.providers.len()) {
            self.providers.push(ProviderEntry {
                url: provider.default_url().into(), model: provider.default_model().into(),
            });
        }
        let initial = RoleModel { provider: self.selected,
            model: self.providers[provider_index(self.selected)].model.clone() };
        if self.commander.is_none() { self.commander = Some(initial.clone()); }
        if self.implementer_groups.is_none() {
            let old_count = self.legacy_implementer_count.unwrap_or(3).clamp(2, MAX_IMPLEMENTERS);
            let first = (old_count + 1) / 2;
            self.implementer_groups = Some(vec![
                ImplementerGroup { role: initial.clone(), count: first },
                ImplementerGroup { role: initial.clone(), count: old_count - first },
            ]);
        }
        let groups = self.implementer_groups.as_mut().unwrap();
        if groups.is_empty() {
            groups.push(ImplementerGroup { role: initial, count: 1 });
        }
        let mut total = 0;
        groups.retain_mut(|group| {
            if total == MAX_IMPLEMENTERS { return false; }
            group.count = group.count.clamp(1, MAX_IMPLEMENTERS / 2)
                .min(MAX_IMPLEMENTERS - total);
            if group.role.model.trim().is_empty() {
                group.role.model = group.role.provider.default_model().into();
            }
            total += group.count;
            true
        });
        if let Some(commander) = &mut self.commander {
            if commander.model.trim().is_empty() {
                commander.model = commander.provider.default_model().into();
            }
        }
        self.legacy_implementer_count = None;
    }

    fn entry(&mut self) -> &mut ProviderEntry {
        let index = provider_index(self.selected);
        &mut self.providers[index]
    }

    fn groups(&self) -> &[ImplementerGroup] { self.implementer_groups.as_deref().unwrap() }
    fn groups_mut(&mut self) -> &mut Vec<ImplementerGroup> { self.implementer_groups.as_mut().unwrap() }

    fn worker_count(&self) -> usize { self.groups().iter().map(|g| g.count).sum() }
}

fn provider_index(provider: Provider) -> usize {
    match provider {
        Provider::Ollama => 0, Provider::OpenAI => 1,
        Provider::Anthropic => 2, Provider::Gemini => 3, Provider::DeepSeek => 4,
    }
}

fn role_controls(ui: &mut egui::Ui, id: &str, title: &str,
    role: &mut RoleModel, ollama_models: &[String]) {
    ui.label(RichText::new(title).strong());
    let previous = role.provider;
    egui::ComboBox::from_id_salt((id, "provider"))
        .selected_text(role.provider.label()).width(ui.available_width())
        .show_ui(ui, |ui| {
            for provider in Provider::ALL {
                ui.selectable_value(&mut role.provider, provider, provider.label());
            }
        });
    if role.provider != previous { role.model = role.provider.default_model().into(); }
    ui.add(TextEdit::singleline(&mut role.model).hint_text("Model name")
        .desired_width(f32::INFINITY));
    let choices: &[String] = match role.provider {
        Provider::Ollama => ollama_models,
        _ => &[],
    };
    if role.provider == Provider::DeepSeek {
        egui::ComboBox::from_id_salt((id, "model"))
            .selected_text("Choose DeepSeek model").width(ui.available_width())
            .show_ui(ui, |ui| {
                for model in ["deepseek-flash", "deepseek-v4-pro"] {
                    if ui.selectable_label(role.model == model, model).clicked() { role.model = model.into(); }
                }
            });
    } else if !choices.is_empty() {
        egui::ComboBox::from_id_salt((id, "model"))
            .selected_text("Choose installed model").width(ui.available_width())
            .show_ui(ui, |ui| {
                for model in choices {
                    if ui.selectable_label(role.model == *model, model).clicked() { role.model = model.clone(); }
                }
            });
    }
}

struct WorkerView {
    busy: bool,
    command: String,
    status: String,
    answer: String,
}

impl Default for WorkerView {
    fn default() -> Self {
        Self { busy: false, command: String::new(),
            status: "Available".into(), answer: String::new() }
    }
}

struct App {
    config: Config,
    saved_config: String,
    api_key: String,
    key_saved: bool,
    checking_key: bool,
    key_check_generation: u64,
    key_check_status: Option<(bool, String)>,
    checking_update: bool,
    update_result: Option<Result<updates::UpdateCheck, String>>,
    history: Vec<ChatMessage>,
    attachments: Vec<Attachment>,
    input: String,
    status: String,
    busy: bool,
    stopping: bool,
    cancel: Option<watch::Sender<bool>>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    approvals: VecDeque<(String, ToolCall, oneshot::Sender<bool>)>,
    workers: Vec<WorkerView>,
    tools: Vec<String>,
    selected_server: Option<usize>,
    models: Vec<String>,
}

impl Default for App {
    fn default() -> Self {
        let config = load_config();
        let saved_config = serde_json::to_string(&config).unwrap_or_default();
        let (api_key, key_saved, status) = match keychain::load(config.selected) {
            Ok(Some(key)) => (key, true, "Ready".to_owned()),
            Ok(None) => (String::new(), false, "Ready".to_owned()),
            Err(error) => (String::new(), false, format!("Could not load API key: {error}")),
        };
        let (tx, rx) = mpsc::channel();
        Self { config, saved_config, api_key, key_saved, checking_key: false,
            key_check_generation: 0, key_check_status: None,
            checking_update: false, update_result: None, history: vec![], attachments: vec![],
            input: String::new(), status, busy: false, stopping: false,
            cancel: None, tx, rx,
            approvals: VecDeque::new(), workers: Vec::new(),
            tools: vec![], selected_server: None, models: vec![] }
    }
}

impl App {
    fn process_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::KeyCheck(generation, valid, message) => {
                    self.checking_key = false;
                    if generation == self.key_check_generation && !self.stopping {
                        self.key_check_status = Some((valid, message));
                    }
                }
                Event::UpdateCheck(result) => {
                    self.checking_update = false;
                    self.update_result = Some(result);
                }
                Event::Done if self.stopping => {
                    self.busy = false;
                    self.stopping = false;
                    self.cancel = None;
                    self.approvals.clear();
                    for worker in &mut self.workers {
                        if worker.busy { worker.busy = false; worker.status = "Stopped".into(); }
                    }
                    self.status = "Stopped".into();
                }
                _ if self.stopping => {}
                Event::Status(s) => self.status = s,
                Event::Answer(s) => {
                    self.history.push(ChatMessage { role: "assistant".into(), content: s, attachments: vec![] });
                    self.status = "Ready".into();
                }
                Event::Error(e) => self.status = format!("Error: {e}"),
                Event::Approval(source, call, sender) => {
                    self.status = format!("Approval required: {source}: {}", call.name);
                    self.approvals.push_back((source, call, sender));
                }
                Event::WorkerAssigned(id, command) => {
                    if let Some(worker) = self.workers.get_mut(id) {
                        worker.busy = true;
                        worker.command = command;
                        worker.status = "Starting...".into();
                        worker.answer.clear();
                    }
                }
                Event::WorkerStatus(id, status) => {
                    if let Some(worker) = self.workers.get_mut(id) { worker.status = status; }
                }
                Event::WorkerAnswer(id, answer) => {
                    if let Some(worker) = self.workers.get_mut(id) {
                        worker.busy = false;
                        worker.status = "Available".into();
                        worker.answer = answer;
                    }
                }
                Event::Tools(names, errors) => {
                    self.tools = names;
                    self.status = if errors.is_empty() { format!("{} MCP tools", self.tools.len()) }
                        else { errors.join(" | ") };
                }
                Event::Models(models) => {
                    self.models = models;
                    self.status = format!("{} Ollama models", self.models.len());
                }
                Event::Done => {
                    self.busy = false;
                    self.cancel = None;
                    for worker in &mut self.workers {
                        if worker.busy { worker.busy = false; worker.status = "Interrupted".into(); }
                    }
                }
            }
        }
    }

    fn launch<F>(&self, task: F)
    where F: FnOnce(Sender<Event>) + Send + 'static {
        let tx = self.tx.clone();
        thread::spawn(move || task(tx));
    }

    fn scan_tools(&mut self) {
        let servers = self.config.servers.clone();
        self.status = "Connecting to MCP servers...".into();
        self.launch(move |tx| {
            let rt = tokio::runtime::Runtime::new().expect("Tokio runtime");
            let (tools, errors) = rt.block_on(mcp::discover(&servers));
            let _ = tx.send(Event::Tools(tools.into_iter().map(|t| t.spec.name).collect(), errors));
        });
    }

    fn refresh_models(&mut self) {
        let url = self.config.providers[0].url.clone();
        self.status = "Loading Ollama models...".into();
        self.launch(move |tx| {
            let rt = tokio::runtime::Runtime::new().expect("Tokio runtime");
            match rt.block_on(provider::ollama_models(&url)) {
                Ok(models) => { let _ = tx.send(Event::Models(models)); }
                Err(e) => { let _ = tx.send(Event::Error(e.to_string())); }
            }
        });
    }

    fn check_key(&mut self) {
        let selected = self.config.selected;
        let entry = self.config.entry().clone();
        let settings = Settings {
            provider: selected, model: entry.model, base_url: entry.url,
            api_key: self.api_key.clone(),
        };
        self.key_check_generation = self.key_check_generation.wrapping_add(1);
        let generation = self.key_check_generation;
        self.checking_key = true;
        self.key_check_status = None;
        self.launch(move |tx| {
            let rt = tokio::runtime::Runtime::new().expect("Tokio runtime");
            let (valid, message) = match rt.block_on(provider::check_api_key(settings)) {
                Ok(()) => (true, "API key accepted by provider".to_owned()),
                Err(error) => (false, format!("Key check failed: {error}")),
            };
            let _ = tx.send(Event::KeyCheck(generation, valid, message));
        });
    }

    fn check_updates(&mut self) {
        self.checking_update = true;
        self.update_result = None;
        self.launch(|tx| {
            let rt = tokio::runtime::Runtime::new().expect("Tokio runtime");
            let result = rt.block_on(updates::check()).map_err(|error| format!("{error:#}"));
            let _ = tx.send(Event::UpdateCheck(result));
        });
    }

    fn settings_for(&self, role: &RoleModel) -> anyhow::Result<Settings> {
        let entry = &self.config.providers[provider_index(role.provider)];
        if role.model.trim().is_empty() { anyhow::bail!("Choose a model for {}", role.provider.label()); }
        if entry.url.trim().is_empty() { anyhow::bail!("Set the endpoint for {}", role.provider.label()); }
        let api_key = if role.provider == Provider::Ollama { String::new() }
            else if role.provider == self.config.selected && !self.api_key.trim().is_empty() {
                self.api_key.clone()
            } else { keychain::load(role.provider)?.unwrap_or_default() };
        if role.provider != Provider::Ollama && api_key.trim().is_empty() {
            anyhow::bail!("Save an API key for {} in Connections", role.provider.label());
        }
        Ok(Settings { provider: role.provider, model: role.model.trim().into(),
            base_url: entry.url.clone(), api_key })
    }

    fn send(&mut self) {
        let prompt = self.input.trim().to_owned();
        if (prompt.is_empty() && self.attachments.is_empty()) || self.busy { return; }
        let commander_role = if self.config.commander_mode {
            self.config.commander.as_ref().unwrap().clone()
        } else {
            RoleModel { provider: self.config.selected, model: self.config.providers[provider_index(self.config.selected)].model.clone() }
        };
        let commander_settings = match self.settings_for(&commander_role) {
            Ok(settings) => settings, Err(error) => { self.status = error.to_string(); return; }
        };
        let mut worker_settings = Vec::new();
        if self.config.commander_mode {
            for group in self.config.groups() {
                let settings = match self.settings_for(&group.role) {
                    Ok(settings) => settings, Err(error) => { self.status = error.to_string(); return; }
                };
                worker_settings.extend(std::iter::repeat_n(settings, group.count));
            }
        }
        let current_image = self.attachments.iter().any(Attachment::is_image);
        let commander_has_image = current_image
            || self.history.iter().flat_map(|m| &m.attachments).any(Attachment::is_image);
        let unsupported_image = |settings: &Settings| {
            settings.provider == Provider::DeepSeek && settings.model != "deepseek-flash"
        };
        if (commander_has_image && unsupported_image(&commander_settings))
            || (current_image && worker_settings.iter().any(unsupported_image)) {
            self.status = "DeepSeek images require deepseek-flash for every role using DeepSeek".into();
            return;
        }
        self.input.clear();
        self.history.push(ChatMessage {
            role: "user".into(),
            content: if prompt.is_empty() { "Please analyze the attached files.".into() } else { prompt },
            attachments: std::mem::take(&mut self.attachments),
        });
        self.busy = true;
        self.status = "Thinking...".into();
        self.workers.resize_with(worker_settings.len(), WorkerView::default);
        self.workers.truncate(worker_settings.len());
        let history = self.history.clone();
        let servers = self.config.servers.clone();
        let auto_approve_mcp = self.config.auto_approve_mcp;
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        self.cancel = Some(cancel_tx);
        self.launch(move |tx| {
            let rt = tokio::runtime::Runtime::new().expect("Tokio runtime");
            let result = rt.block_on(async {
                tokio::select! {
                    _ = cancel_rx.changed() => Ok(()),
                    result = agent::run(commander_settings, history, servers, auto_approve_mcp,
                        worker_settings, tx.clone()) => result,
                }
            });
            if let Err(e) = result {
                let _ = tx.send(Event::Error(format!("{e:#}")));
            }
            let _ = tx.send(Event::Done);
        });
    }

    fn stop(&mut self) {
        if !self.busy || self.stopping { return; }
        self.stopping = true;
        self.status = "Stopping...".into();
        self.approvals.clear();
        if let Some(cancel) = &self.cancel { let _ = cancel.send(true); }
    }

    fn pick_files(&mut self) {
        let Some(paths) = rfd::FileDialog::new().pick_files() else { return; };
        let mut added = 0;
        for path in paths {
            if self.attachments.len() >= MAX_ATTACHMENTS {
                self.status = format!("Attach at most {MAX_ATTACHMENTS} files per message");
                return;
            }
            match Attachment::load(&path) {
                Ok(attachment) => {
                    let image_bytes: usize = self.attachments.iter().map(Attachment::image_bytes).sum();
                    if image_bytes + attachment.image_bytes() > MAX_TOTAL_IMAGE_BYTES {
                        self.status = "Images in one message cannot exceed 8 MiB total".into();
                        return;
                    }
                    self.attachments.push(attachment);
                    added += 1;
                }
                Err(error) => { self.status = error.to_string(); return; }
            }
        }
        self.status = format!("{added} file(s) attached");
    }

    fn paste_clipboard(&mut self, image_only: bool) {
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(clipboard) => clipboard,
            Err(error) => { self.status = format!("Cannot access clipboard: {error}"); return; }
        };
        if image_only && clipboard.get_text().is_ok_and(|text| !text.is_empty()) {
            return; // TextEdit handles the normal text paste shortcut.
        }
        if let Ok(image) = clipboard.get_image() {
            if self.attachments.len() >= MAX_ATTACHMENTS {
                self.status = format!("Attach at most {MAX_ATTACHMENTS} files per message");
                return;
            }
            match Attachment::from_clipboard_rgba(image.width, image.height, image.bytes.as_ref()) {
                Ok(attachment) => {
                    let image_bytes: usize = self.attachments.iter().map(Attachment::image_bytes).sum();
                    if image_bytes + attachment.image_bytes() > MAX_TOTAL_IMAGE_BYTES {
                        self.status = "Images in one message cannot exceed 8 MiB total".into();
                    } else {
                        self.attachments.push(attachment);
                        self.status = "Clipboard image attached".into();
                    }
                }
                Err(error) => self.status = error.to_string(),
            }
        } else if !image_only {
            match clipboard.get_text() {
                Ok(text) if !text.is_empty() => {
                    self.input.push_str(&text);
                    self.status = "Clipboard text pasted".into();
                }
                Ok(_) => self.status = "Clipboard is empty".into(),
                Err(error) => self.status = format!("Clipboard has no text or image: {error}"),
            }
        }
    }

    fn update_controls(&mut self, ui: &mut egui::Ui) {
        let version = updates::CURRENT_VERSION.strip_suffix(".0").unwrap_or(updates::CURRENT_VERSION);
        ui.label(RichText::new(format!("MCP Commander {version}")).strong());
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.checking_update, egui::Button::new("Check for updates")).clicked() {
                self.check_updates();
            }
            if self.checking_update { ui.spinner(); }
        });
        if let Some(result) = &self.update_result {
            match result {
                Ok(updates::UpdateCheck::NoRelease) => { ui.label("No published release yet."); }
                Ok(updates::UpdateCheck::Available { version, url }) => {
                    ui.label(RichText::new(format!("Update available: {version}")).color(Color32::from_rgb(110, 210, 140)));
                    ui.hyperlink_to("Open release", url);
                }
                Ok(updates::UpdateCheck::UpToDate { .. }) => { ui.label("You have the latest release."); }
                Ok(updates::UpdateCheck::AheadOfRelease { version }) => {
                    ui.label(format!("Installed build is newer than release {version}."));
                }
                Ok(updates::UpdateCheck::UnknownVersion { tag, url }) => {
                    ui.label(format!("Cannot compare release tag {tag}."));
                    ui.hyperlink_to("Open release", url);
                }
                Err(error) => { ui.label(RichText::new(format!("Update check failed: {error}"))
                    .color(Color32::from_rgb(240, 130, 130))); }
            }
        }
        ui.hyperlink_to("All GitHub releases", updates::RELEASES_URL);
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        self.update_controls(ui);
        ui.separator();
        ui.heading("Agent roles");
        ui.add_enabled_ui(!self.busy, |ui| {
            ui.checkbox(&mut self.config.commander_mode, "Commander mode");
            if self.config.commander_mode {
                role_controls(ui, "commander", "Commander model",
                    self.config.commander.as_mut().unwrap(), &self.models);
                ui.separator();
                let mut total = self.config.worker_count();
                let group_count = self.config.groups().len();
                let mut remove_group = None;
                let mut quantities_changed = false;
                for (index, group) in self.config.groups_mut().iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Implementer group {}", index + 1)).strong());
                        if ui.add_enabled(group_count > 1, egui::Button::new("Delete")).clicked() {
                            remove_group = Some(index);
                        }
                    });
                    role_controls(ui, &format!("group_{index}"), "Model", &mut group.role, &self.models);
                    let previous_count = group.count;
                    let max_count = (MAX_IMPLEMENTERS - (total - previous_count))
                        .min(MAX_IMPLEMENTERS / 2);
                    quantities_changed |= ui.add(egui::Slider::new(&mut group.count, 1..=max_count)
                        .text("Quantity")).changed();
                    total = total - previous_count + group.count;
                    ui.separator();
                }
                if let Some(index) = remove_group {
                    self.config.groups_mut().remove(index);
                    quantities_changed = true;
                }
                if ui.add_enabled(self.config.worker_count() < MAX_IMPLEMENTERS,
                    egui::Button::new("Add group")).clicked() {
                    let role = self.config.commander.as_ref().unwrap().clone();
                    self.config.groups_mut().push(ImplementerGroup { role, count: 1 });
                    quantities_changed = true;
                }
                if quantities_changed {
                    self.workers.clear();
                }
            }
        });
        if self.config.commander_mode {
            let count = self.config.worker_count();
            self.workers.resize_with(count, WorkerView::default);
            self.workers.truncate(count);
            let available = (0..count)
                .filter(|&id| !self.workers[id].busy).count();
            ui.label(format!("Available: {available} / {count}"));
            ui.small("Tasks and results appear beside the chat.");
        }
        if !self.busy && ui.button("Refresh Ollama model list").clicked() { self.refresh_models(); }
        ui.separator();
        if self.busy {
            ui.label("Finish the current request before changing connections.");
            return;
        }
        ui.label(RichText::new("Connections").strong());
        ui.label("Provider to configure");
        let previous = self.config.selected;
        egui::ComboBox::from_id_salt("provider").selected_text(previous.label()).show_ui(ui, |ui| {
            for p in Provider::ALL { ui.selectable_value(&mut self.config.selected, p, p.label()); }
        });
        if self.config.selected != previous {
            self.key_check_generation = self.key_check_generation.wrapping_add(1);
            self.key_check_status = None;
            match keychain::load(self.config.selected) {
                Ok(Some(key)) => {
                    self.api_key = key;
                    self.key_saved = true;
                    self.status = "Provider changed; saved API key loaded".into();
                }
                Ok(None) => {
                    self.api_key.clear();
                    self.key_saved = false;
                    self.status = "Provider selected for connection settings".into();
                }
                Err(error) => {
                    self.api_key.clear();
                    self.key_saved = false;
                    self.status = format!("Could not load API key: {error}");
                }
            }
        }
        ui.add_space(8.0);
        let provider = self.config.selected;
        let entry = self.config.entry();
        ui.label("Endpoint");
        if ui.add(TextEdit::singleline(&mut entry.url).desired_width(f32::INFINITY)).changed() {
            self.key_check_generation = self.key_check_generation.wrapping_add(1);
            self.key_check_status = None;
        }
        ui.label("Default model for single-agent mode");
        if provider == Provider::DeepSeek {
            egui::ComboBox::from_id_salt("deepseek_models")
                .selected_text(&entry.model)
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut entry.model, "deepseek-flash".into(), "deepseek-flash");
                    ui.selectable_value(&mut entry.model, "deepseek-v4-pro".into(), "deepseek-v4-pro");
                });
        } else {
            ui.add(TextEdit::singleline(&mut entry.model).desired_width(f32::INFINITY));
        }
        if provider == Provider::Ollama {
            if ui.button("Refresh models").clicked() { self.refresh_models(); }
            if !self.models.is_empty() {
                let entry = self.config.entry();
                egui::ComboBox::from_id_salt("models").selected_text("Choose installed model")
                    .show_ui(ui, |ui| for model in &self.models {
                        if ui.selectable_label(entry.model == *model, model).clicked() { entry.model = model.clone(); }
                    });
            }
        } else {
            ui.label("API key (macOS Keychain)");
            if ui.add(TextEdit::singleline(&mut self.api_key).password(true).desired_width(f32::INFINITY)).changed() {
                self.key_saved = false;
                self.key_check_generation = self.key_check_generation.wrapping_add(1);
                self.key_check_status = None;
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.api_key.trim().is_empty(), egui::Button::new("Save key")).clicked() {
                    match keychain::save(provider, &self.api_key) {
                        Ok(()) => { self.key_saved = true; self.status = "API key saved in macOS Keychain".into(); }
                        Err(error) => self.status = format!("Could not save API key: {error}"),
                    }
                }
                if ui.button("Delete saved key").clicked() {
                    match keychain::delete(provider) {
                        Ok(()) => {
                            self.api_key.clear();
                            self.key_saved = false;
                            self.key_check_generation = self.key_check_generation.wrapping_add(1);
                            self.key_check_status = None;
                            self.status = "Saved API key deleted".into();
                        }
                        Err(error) => self.status = format!("Could not delete API key: {error}"),
                    }
                }
            });
            if self.key_saved { ui.small("Saved locally in macOS Keychain."); }
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.api_key.trim().is_empty() && !self.checking_key,
                    egui::Button::new("Check key")).clicked() { self.check_key(); }
                if self.checking_key { ui.spinner(); }
            });
            if let Some((valid, message)) = &self.key_check_status {
                let color = if *valid { Color32::from_rgb(110, 210, 140) }
                    else { Color32::from_rgb(240, 130, 130) };
                ui.label(RichText::new(message).color(color));
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.heading("MCP servers");
            if ui.small_button("+").clicked() {
                self.config.servers.push(ServerConfig { name: format!("server{}", self.config.servers.len()+1),
                    enabled: true, ..Default::default() });
                self.selected_server = Some(self.config.servers.len()-1);
            }
        });
        let mut remove_server = None;
        for (i, server) in self.config.servers.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.checkbox(&mut server.enabled, "");
                if ui.selectable_label(self.selected_server == Some(i), &server.name).clicked() {
                    self.selected_server = Some(i);
                }
                if ui.small_button("Delete").on_hover_text("Remove this MCP server").clicked() {
                    remove_server = Some(i);
                }
            });
        }
        if let Some(i) = remove_server {
            self.config.servers.remove(i);
            self.selected_server = match self.selected_server {
                Some(selected) if selected == i => None,
                Some(selected) if selected > i => Some(selected - 1),
                other => other,
            };
            self.tools.clear();
        }
        if let Some(i) = self.selected_server.filter(|i| *i < self.config.servers.len()) {
            ui.separator();
            let server = &mut self.config.servers[i];
            ui.label("Name");
            ui.add(TextEdit::singleline(&mut server.name).desired_width(f32::INFINITY));
            egui::ComboBox::from_id_salt("transport")
                .selected_text(match server.transport { Transport::Stdio => "Local command", Transport::Http => "HTTP URL" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut server.transport, Transport::Stdio, "Local command");
                    ui.selectable_value(&mut server.transport, Transport::Http, "HTTP URL");
                });
            match server.transport {
                Transport::Stdio => {
                    ui.label("Executable or command");
                    ui.add(TextEdit::singleline(&mut server.command).desired_width(f32::INFINITY));
                    ui.label("Arguments (one per line)");
                    ui.add(TextEdit::multiline(&mut server.args).desired_rows(3).desired_width(f32::INFINITY));
                }
                Transport::Http => {
                    ui.label("Streamable HTTP MCP URL");
                    ui.add(TextEdit::singleline(&mut server.url).desired_width(f32::INFINITY));
                }
            }
        }
        ui.separator();
        if ui.button("Check MCP connections").clicked() { self.scan_tools(); }
        ui.label(format!("{} tools available", self.tools.len()));
        ScrollArea::vertical().max_height(100.0).show(ui, |ui| for tool in &self.tools { ui.small(tool); });
        ui.add_space(6.0);
        ui.checkbox(&mut self.config.auto_approve_mcp, "Run MCP tools without asking");
        if self.config.auto_approve_mcp {
            ui.small("Enabled MCP servers can change files and projects automatically.");
        }
    }

    fn conversation(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(if self.config.commander_mode { "Commander chat" } else { "Conversation" });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("New chat").clicked() && !self.busy {
                    self.history.clear();
                    self.attachments.clear();
                    self.workers.clear();
                }
            });
        });
        ui.separator();
        let reserved = if self.attachments.is_empty() { 180.0 } else { 265.0 };
        let height = (ui.available_height() - reserved).max(150.0);
        ScrollArea::vertical().auto_shrink([false, false]).max_height(height).stick_to_bottom(true)
            .show(ui, |ui| {
                if self.history.is_empty() {
                    ui.add_space(24.0);
                    ui.label(RichText::new("Ask a model or connect an MCP server to work with local apps.").size(16.0));
                }
                for message in &self.history {
                    let user = message.role == "user";
                    let fill = if user { Color32::from_rgb(35, 75, 135) } else { Color32::from_rgb(39, 42, 48) };
                    egui::Frame::default().fill(fill).corner_radius(8.0).inner_margin(10.0).show(ui, |ui| {
                    ui.label(RichText::new(if user { "You" } else if self.config.commander_mode { "Commander-agent" } else { "Agent" }).strong());
                        ui.label(&message.content);
                        for attachment in &message.attachments {
                            ui.small(format!("Attached: {}", attachment.name()));
                        }
                    });
                    ui.add_space(6.0);
                }
            });
        ui.separator();
        if !self.attachments.is_empty() {
            let mut remove = None;
            ScrollArea::vertical().max_height(80.0).show(ui, |ui| {
                for (index, attachment) in self.attachments.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(attachment.name());
                        if ui.small_button("Remove").clicked() { remove = Some(index); }
                    });
                }
            });
            if let Some(index) = remove { self.attachments.remove(index); }
        }
        let input = ui.add(TextEdit::multiline(&mut self.input)
            .hint_text("Write a message... (Enter to send, Shift+Enter for a new line)")
            .return_key(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::Enter))
            .desired_rows(3).desired_width(f32::INFINITY));
        let enter_to_send = input.has_focus() && ui.input(|state| state.events.iter().any(|event| {
            matches!(event, egui::Event::Key {
                key: egui::Key::Enter, pressed: true, modifiers, ..
            } if *modifiers == egui::Modifiers::NONE)
        }));
        if enter_to_send { self.send(); }
        let image_paste_shortcut = !self.busy && input.has_focus() && ui.input(|state| state.events.iter().any(|event| {
            matches!(event, egui::Event::Key {
                key: egui::Key::V, pressed: false, modifiers, ..
            } if modifiers.command)
        }));
        if image_paste_shortcut { self.paste_clipboard(true); }
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.busy, egui::Button::new("Attach files")).clicked() { self.pick_files(); }
            if ui.add_enabled(!self.busy, egui::Button::new("Paste clipboard")).clicked() { self.paste_clipboard(false); }
            if ui.add_enabled(!self.busy, egui::Button::new("Send")).clicked() { self.send(); }
            if ui.add_enabled(self.busy && !self.stopping, egui::Button::new("Stop")).clicked() { self.stop(); }
            if self.busy { ui.spinner(); }
        });
        ui.small(&self.status);
        ui.small("Paste text or an image with Command+V or Paste clipboard. Up to 5 files per message.");
    }

    fn approval_dialog(&mut self, ctx: &egui::Context) {
        let mut decision = None;
        if let Some((source, call, _)) = self.approvals.front() {
            egui::Window::new("Approve MCP action").collapsible(false).resizable(true).default_width(500.0)
                .show(ctx, |ui| {
                    ui.label(source);
                    ui.label(RichText::new(&call.name).strong());
                    ui.label("The model requested this tool call:");
                    let json = serde_json::to_string_pretty(&call.arguments).unwrap_or_default();
                    ScrollArea::vertical().max_height(260.0).show(ui, |ui| { ui.code(json); });
                    ui.horizontal(|ui| {
                        if ui.button("Allow once").clicked() { decision = Some(true); }
                        if ui.button("Deny").clicked() { decision = Some(false); }
                    });
                });
        }
        if let Some(allow) = decision {
            if let Some((_, call, sender)) = self.approvals.pop_front() {
                let _ = sender.send(allow);
                self.status = if allow { format!("Running {}...", call.name) } else { format!("Denied {}", call.name) };
            }
        }
    }

    fn dashboard(&self, ui: &mut egui::Ui) {
        ui.heading("Implementers");
        let total = self.config.worker_count();
        let available = self.workers.iter().take(total).filter(|worker| !worker.busy).count();
        ui.label(format!("Available: {available} / {total}"));
        ui.separator();
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut offset = 0;
            for (row, groups) in self.config.groups().chunks(2).enumerate() {
                ui.columns(2, |columns| {
                    for (column_index, group) in groups.iter().enumerate() {
                        let group_index = row * 2 + column_index;
                        let column = &mut columns[column_index];
                        column.label(RichText::new(format!("Implementer group {}", group_index + 1)).strong());
                        column.small(format!("{} · {}", group.role.provider.label(), group.role.model));
                        let group_available = self.workers.iter().skip(offset).take(group.count)
                            .filter(|worker| !worker.busy).count();
                        column.small(format!("Available: {group_available} / {}", group.count));
                        column.add_space(8.0);
                        for id in offset..offset + group.count {
                            let worker = &self.workers[id];
                            egui::Frame::default().fill(Color32::from_rgb(34, 38, 46))
                                .corner_radius(8.0).inner_margin(10.0).show(column, |ui| {
                                    ui.label(RichText::new(format!("Agent {}", id + 1)).strong());
                                    ui.small(if worker.busy { "Working" } else { "Available" });
                                    ui.small(&worker.status);
                                    ui.separator();
                                    ui.small(RichText::new("Command from commander").strong());
                                    if worker.command.is_empty() { ui.small("No command yet."); }
                                    else { ScrollArea::vertical().max_height(85.0).show(ui, |ui| { ui.label(&worker.command); }); }
                                    if !worker.answer.is_empty() {
                                        ui.separator();
                                        ui.small(RichText::new("Result").strong());
                                        ScrollArea::vertical().max_height(110.0).show(ui, |ui| { ui.label(&worker.answer); });
                                    }
                                });
                            column.add_space(8.0);
                        }
                        offset += group.count;
                    }
                });
                ui.add_space(12.0);
            }
        });
    }

    fn persist(&mut self) {
        let Ok(now) = serde_json::to_string(&self.config) else { return; };
        if now != self.saved_config {
            match save_config(&self.config) {
                Ok(()) => self.saved_config = now,
                Err(e) => self.status = format!("Could not save settings: {e}"),
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.process_events();
        egui::Panel::left("settings").min_size(310.0).default_size(345.0).resizable(true)
            .show(ui, |ui| ScrollArea::vertical().show(ui, |ui| self.sidebar(ui)));
        if self.config.commander_mode {
            let right_width = (ui.available_width() * 0.5).max(420.0);
            egui::Panel::right("implementer_dashboard").min_size(390.0)
                .default_size(right_width).resizable(true)
                .show(ui, |ui| self.dashboard(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| self.conversation(ui));
        self.approval_dialog(ui.ctx());
        self.persist();
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
}

fn config_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/MCP Commander/config.json"))
}

fn legacy_config_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/Ollama MCP Agent/config.json"))
}

fn load_config() -> Config {
    let mut config: Config = config_path().and_then(|p| fs::read_to_string(p).ok())
        .or_else(|| legacy_config_path().and_then(|p| fs::read_to_string(p).ok()))
        .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let before = serde_json::to_string(&config).ok();
    config.normalize();
    if before != serde_json::to_string(&config).ok() || !config_path().is_some_and(|p| p.exists()) {
        let _ = save_config(&config);
    }
    config
}

fn save_config(config: &Config) -> anyhow::Result<()> {
    let path = config_path().ok_or_else(|| anyhow::anyhow!("HOME is unavailable"))?;
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(path, serde_json::to_vec_pretty(config)?)?;
    Ok(())
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1550.0, 900.0]).with_min_inner_size([980.0, 640.0]),
        ..Default::default()
    };
    let version = env!("CARGO_PKG_VERSION");
    let title = format!("MCP Commander {}", version.strip_suffix(".0").unwrap_or(version));
    eframe::run_native(&title, options, Box::new(|cc| {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        Ok(Box::new(App::default()))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_gain_deepseek_without_losing_mcp_servers() {
        let mut old = Config::default();
        old.providers.truncate(4);
        old.providers[0].model = "my-ollama-model".into();
        old.servers.push(ServerConfig {
            name: "graph-cad".into(), transport: Transport::Http,
            url: "http://127.0.0.1:8766/mcp".into(), enabled: true,
            ..Default::default()
        });
        let serialized = serde_json::to_string(&old).unwrap();
        let mut restored: Config = serde_json::from_str(&serialized).unwrap();
        restored.normalize();
        assert_eq!(restored.providers.len(), 5);
        assert_eq!(restored.providers[0].model, "my-ollama-model");
        assert_eq!(restored.providers[4].model, "deepseek-flash");
        assert_eq!(restored.servers[0].url, "http://127.0.0.1:8766/mcp");
    }

    #[test]
    fn old_settings_keep_tool_approval_enabled() {
        let mut value = serde_json::to_value(Config::default()).unwrap();
        value.as_object_mut().unwrap().remove("auto_approve_mcp");
        let restored: Config = serde_json::from_value(value).unwrap();
        assert!(!restored.auto_approve_mcp);
    }

    #[test]
    fn existing_commander_settings_gain_two_model_groups() {
        let mut old = serde_json::to_value(Config::default()).unwrap();
        old["selected"] = serde_json::json!("DeepSeek");
        old["implementer_count"] = serde_json::json!(3);
        let mut restored: Config = serde_json::from_value(old).unwrap();
        restored.normalize();
        assert_eq!(restored.commander.as_ref().unwrap().provider, Provider::DeepSeek);
        assert_eq!(restored.groups()[0].count, 2);
        assert_eq!(restored.groups()[1].count, 1);
        assert_eq!(restored.worker_count(), 3);
    }

    #[test]
    fn dynamic_groups_survive_settings_reload_and_respect_worker_limit() {
        let mut config = Config::default();
        config.normalize();
        config.groups_mut().push(ImplementerGroup {
            role: RoleModel { provider: Provider::DeepSeek, model: "deepseek-flash".into() },
            count: 4,
        });
        let mut restored: Config = serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        restored.normalize();
        assert_eq!(restored.groups().len(), 3);
        assert_eq!(restored.groups()[2].role.provider, Provider::DeepSeek);
        assert_eq!(restored.worker_count(), 7);

        restored.groups_mut().push(ImplementerGroup {
            role: RoleModel { provider: Provider::Ollama, model: "qwen3:8b".into() },
            count: 4,
        });
        restored.normalize();
        assert_eq!(restored.worker_count(), MAX_IMPLEMENTERS);
        assert_eq!(restored.groups()[3].count, 1);
    }

    #[test]
    fn empty_group_list_recovers_one_implementer() {
        let mut config = Config::default();
        config.implementer_groups = Some(vec![]);
        config.normalize();
        assert_eq!(config.groups().len(), 1);
        assert_eq!(config.worker_count(), 1);
    }
}
