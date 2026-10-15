//! Account screen: choose how to reach Claude and sign in.

use super::{App, JobTag, ToastKind, View};
use crate::claude::auth::{self, AuthStatus, LoginFlow};
use crate::claude::cli;
use crate::config::{AppConfig, BackendKind};
use crate::jobs::JobKind;
use crate::secrets::{self, Secret};
use eframe::egui::{self, Color32, RichText};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

pub struct AuthUi {
    pub exe: Option<PathBuf>,
    pub version: String,
    pub status: Option<Result<AuthStatus, String>>,
    rx: Option<Receiver<(Result<AuthStatus, String>, String)>>,
    poll_until: Option<Instant>,
    last_check: Instant,
    pub token_input: String,
    pub key_input: String,
    pub has_token: bool,
    pub has_key: bool,
    pub test_result: Option<Result<String, String>>,
}

impl AuthUi {
    pub fn new(cfg: &AppConfig) -> Self {
        Self {
            exe: cli::resolve_claude(&cfg.claude_path),
            version: String::new(),
            status: None,
            rx: None,
            poll_until: None,
            last_check: Instant::now(),
            token_input: String::new(),
            key_input: String::new(),
            has_token: secrets::get(Secret::ClaudeOauthToken).is_some(),
            has_key: secrets::get(Secret::AnthropicApiKey).is_some() || std::env::var("ANTHROPIC_API_KEY").is_ok(),
            test_result: None,
        }
    }

    /// Re-detect Claude Code and re-check the sign-in status in the background.
    pub fn refresh(&mut self, cfg: &AppConfig) {
        self.exe = cli::resolve_claude(&cfg.claude_path);
        self.has_token = secrets::get(Secret::ClaudeOauthToken).is_some();
        self.has_key = secrets::get(Secret::AnthropicApiKey).is_some() || std::env::var("ANTHROPIC_API_KEY").is_ok();
        let Some(exe) = self.exe.clone() else {
            self.status = None;
            return;
        };
        if self.rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let kind = cfg.backend;
        std::thread::spawn(move || {
            let version = cli::version(&exe).unwrap_or_default();
            let st = auth::status(&exe, kind).map_err(|e| format!("{e:#}"));
            let _ = tx.send((st, version));
        });
        self.rx = Some(rx);
        self.last_check = Instant::now();
    }

    /// Start polling the status (after opening a sign-in window).
    pub fn watch_for_login(&mut self) {
        self.poll_until = Some(Instant::now() + Duration::from_secs(300));
    }

    pub fn checking(&self) -> bool {
        self.rx.is_some()
    }

    pub fn poll(&mut self, ctx: &egui::Context, cfg: &AppConfig) {
        if let Some(rx) = &self.rx {
            if let Ok((st, version)) = rx.try_recv() {
                self.rx = None;
                if !version.is_empty() {
                    self.version = version;
                }
                let logged_in = matches!(&st, Ok(s) if s.logged_in);
                self.status = Some(st);
                if logged_in {
                    self.poll_until = None;
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(300));
            }
        }
        if let Some(until) = self.poll_until {
            if Instant::now() > until {
                self.poll_until = None;
            } else {
                if self.rx.is_none() && self.last_check.elapsed() > Duration::from_secs(3) {
                    self.refresh(cfg);
                }
                ctx.request_repaint_after(Duration::from_secs(1));
            }
        }
    }

    pub fn logged_in(&self) -> bool {
        matches!(&self.status, Some(Ok(s)) if s.logged_in)
    }

    pub fn ready(&self, cfg: &AppConfig) -> bool {
        match cfg.backend {
            BackendKind::ClaudeCodeSubscription => self.exe.is_some() && (self.has_token || self.logged_in()),
            BackendKind::ClaudeCodeApiKey => self.exe.is_some() && (self.has_key || self.logged_in()),
            BackendKind::DirectApi => self.has_key,
        }
    }

    pub fn short_label(&self, cfg: &AppConfig) -> String {
        match cfg.backend {
            BackendKind::ClaudeCodeSubscription => match &self.status {
                Some(Ok(s)) if s.logged_in && !s.email.is_empty() => s.email.clone(),
                _ if self.has_token => "Claude (app token)".into(),
                _ => "Claude subscription".into(),
            },
            BackendKind::ClaudeCodeApiKey => "API key via Claude Code".into(),
            BackendKind::DirectApi => "Anthropic API".into(),
        }
    }
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

fn ok_line(ui: &mut egui::Ui, ok: bool, text: impl Into<String>) {
    let (sym, color) = if ok { ("✔", Color32::from_rgb(60, 190, 100)) } else { ("✖", Color32::from_rgb(220, 80, 80)) };
    ui.horizontal(|ui| {
        ui.label(RichText::new(sym).color(color).strong());
        ui.label(text.into());
    });
}

pub fn account_view(app: &mut App, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().id_salt("account").show(ui, |ui| {
        ui.set_max_width(820.0);
        ui.heading("Connect to Claude");
        ui.label(
            "Sheet Music Convertor uses Claude to read your page photos. The recommended way is your Claude subscription \
             (Pro or Max) through Claude Code, so transcriptions are included in your plan. You can also use an Anthropic API key.",
        );
        ui.add_space(8.0);

        card(ui, |ui| {
            ui.label(RichText::new("How should the app reach Claude?").strong());
            let before = app.cfg.backend;
            for kind in BackendKind::ALL {
                let desc = match kind {
                    BackendKind::ClaudeCodeSubscription => {
                        "Uses your Claude Pro / Max / Team plan. Requires Claude Code installed on this computer."
                    }
                    BackendKind::ClaudeCodeApiKey => "Uses Claude Code, billed to an Anthropic Console API key.",
                    BackendKind::DirectApi => "Calls the Anthropic API directly with an API key. Claude Code not needed.",
                };
                ui.radio_value(&mut app.cfg.backend, kind, RichText::new(kind.label()).strong());
                ui.indent(kind.label(), |ui| {
                    ui.label(RichText::new(desc).weak());
                });
            }
            if before != app.cfg.backend {
                app.save_config();
                app.auth.test_result = None;
                app.auth.refresh(&app.cfg);
            }
        });
        ui.add_space(8.0);

        if app.cfg.backend.uses_cli() {
            claude_code_card(app, ui);
            ui.add_space(8.0);
        }
        if app.cfg.backend == BackendKind::ClaudeCodeSubscription {
            subscription_card(app, ui);
        } else {
            api_key_card(app, ui);
        }
        ui.add_space(8.0);

        card(ui, |ui| {
            ui.label(RichText::new("Model").strong());
            ui.horizontal(|ui| {
                let before = (app.cfg.model.clone(), app.cfg.effort.clone());
                egui::ComboBox::from_id_salt("acct_model")
                    .selected_text(
                        crate::claude::MODEL_CHOICES.iter().find(|m| m.0 == app.cfg.model).map(|m| m.1).unwrap_or(&app.cfg.model),
                    )
                    .show_ui(ui, |ui| {
                        for (v, l) in crate::claude::MODEL_CHOICES {
                            ui.selectable_value(&mut app.cfg.model, v.to_string(), *l);
                        }
                    });
                ui.label("Effort");
                egui::ComboBox::from_id_salt("acct_effort").selected_text(app.cfg.effort.clone()).show_ui(ui, |ui| {
                    for e in crate::claude::EFFORT_CHOICES {
                        ui.selectable_value(&mut app.cfg.effort, e.to_string(), *e);
                    }
                });
                if before != (app.cfg.model.clone(), app.cfg.effort.clone()) {
                    app.save_config();
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let ready = app.backend_ready();
                if ui.add_enabled(ready, egui::Button::new("Test connection")).clicked() {
                    app.auth.test_result = None;
                    app.submit(JobTag::Test, "Connection test", JobKind::TestConnection);
                }
                if app.job_tags.values().any(|t| *t == JobTag::Test) {
                    ui.spinner();
                    ui.label("Asking Claude…");
                }
                match &app.auth.test_result {
                    Some(Ok(t)) => {
                        ui.label(RichText::new(format!("✔ {t}")).color(Color32::from_rgb(60, 190, 100)));
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(format!("✖ {e}")).color(Color32::from_rgb(220, 80, 80)));
                    }
                    None => {}
                }
            });
        });

        ui.add_space(12.0);
        if !app.cfg.first_run_done {
            if ui.add_enabled(app.backend_ready(), egui::Button::new(RichText::new("Continue  →").strong().size(16.0))).clicked() {
                app.cfg.first_run_done = true;
                app.save_config();
                app.view = if app.project.is_some() { View::Book } else { View::Home };
            }
            if !app.backend_ready() && ui.link("Skip for now").clicked() {
                app.cfg.first_run_done = true;
                app.save_config();
                app.view = View::Home;
            }
        } else if ui.button("← Back").clicked() {
            app.view = if app.project.is_some() { View::Book } else { View::Home };
        }
    });
}

fn claude_code_card(app: &mut App, ui: &mut egui::Ui) {
    card(ui, |ui| {
        ui.label(RichText::new("Claude Code").strong());
        match &app.auth.exe {
            Some(exe) => {
                let v = if app.auth.version.is_empty() { String::new() } else { format!(" — {}", app.auth.version) };
                ok_line(ui, true, format!("Found{v}"));
                ui.label(RichText::new(exe.display().to_string()).small().weak());
            }
            None => {
                ok_line(ui, false, "Claude Code was not found on this computer.");
                ui.label(auth::INSTALL_HELP);
                ui.hyperlink_to("Claude Code download page", "https://claude.com/claude-code");
            }
        }
        ui.horizontal(|ui| {
            if ui.button("Detect again").clicked() {
                app.auth.refresh(&app.cfg);
            }
            if ui.button("Choose claude executable…").clicked() {
                if let Some(p) = rfd::FileDialog::new().set_title("Locate the claude executable").pick_file() {
                    app.cfg.claude_path = p.display().to_string();
                    app.save_config();
                    app.auth.refresh(&app.cfg);
                }
            }
            if !app.cfg.claude_path.is_empty() && ui.button("Use auto-detect").clicked() {
                app.cfg.claude_path.clear();
                app.save_config();
                app.auth.refresh(&app.cfg);
            }
        });
    });
}

fn subscription_card(app: &mut App, ui: &mut egui::Ui) {
    card(ui, |ui| {
        ui.label(RichText::new("Sign in with your Claude account").strong());
        let Some(exe) = app.auth.exe.clone() else {
            ui.label(RichText::new("Install Claude Code first (see above).").weak());
            return;
        };
        match &app.auth.status {
            Some(Ok(s)) => ok_line(ui, s.logged_in, s.summary()),
            Some(Err(e)) => ok_line(ui, false, format!("Could not read the sign-in status: {e}")),
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking…");
                });
            }
        }
        if app.auth.has_token {
            ok_line(ui, true, "A Claude app token is saved in your system's credential store and will be used.");
        }
        ui.add_space(6.0);
        ui.label(
            "Click Sign in: a window opens and your browser shows the Claude sign-in page. Approve access, then come back — \
             this screen updates by itself.",
        );
        ui.horizontal(|ui| {
            if ui.button(RichText::new("🔑 Sign in with Claude…").strong()).clicked() {
                match auth::open_login_terminal(&exe, LoginFlow::Subscription) {
                    Ok(()) => {
                        app.auth.watch_for_login();
                        app.toast(ToastKind::Info, "Complete the sign-in in your browser. This screen updates automatically.");
                    }
                    Err(e) => app.toast(ToastKind::Error, format!("{e:#}")),
                }
            }
            if ui.add_enabled(!app.auth.checking(), egui::Button::new("Refresh status")).clicked() {
                app.auth.refresh(&app.cfg);
            }
            if app.auth.logged_in() && ui.button("Sign out").clicked() {
                match auth::logout(&exe) {
                    Ok(()) => app.toast(ToastKind::Info, "Signed out of Claude Code"),
                    Err(e) => app.toast(ToastKind::Error, format!("Sign-out failed: {e:#}")),
                }
                app.auth.refresh(&app.cfg);
            }
        });

        ui.add_space(6.0);
        egui::CollapsingHeader::new("Advanced: use a dedicated app token instead").id_salt("token_adv").show(ui, |ui| {
            ui.label(
                "`claude setup-token` creates a long-lived token tied to your subscription. The app stores it in your \
                 operating system's credential manager and uses it instead of Claude Code's own sign-in (useful if you \
                 use a different account in Claude Code, or on shared computers).",
            );
            ui.horizontal(|ui| {
                if ui.button("Generate token…").clicked() {
                    match auth::open_login_terminal(&exe, LoginFlow::SetupToken) {
                        Ok(()) => app
                            .toast(ToastKind::Info, "Finish in the browser, then copy the token shown in the terminal and paste it below."),
                        Err(e) => app.toast(ToastKind::Error, format!("{e:#}")),
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut app.auth.token_input).password(true).hint_text("sk-ant-oat…").desired_width(360.0));
                let valid = auth::looks_like_oauth_token(&app.auth.token_input);
                if ui.add_enabled(valid, egui::Button::new("Save token")).clicked() {
                    match secrets::set(Secret::ClaudeOauthToken, &app.auth.token_input) {
                        Ok(secure) => {
                            app.toast(
                                ToastKind::Success,
                                if secure {
                                    "Token saved in the system credential store"
                                } else {
                                    "Token saved (credential store unavailable — saved in the settings folder)"
                                },
                            );
                            app.auth.token_input.clear();
                            app.auth.refresh(&app.cfg);
                        }
                        Err(e) => app.toast(ToastKind::Error, format!("{e:#}")),
                    }
                }
                if !app.auth.token_input.is_empty() && !valid {
                    ui.label(RichText::new("That doesn't look like a setup token").weak());
                }
            });
            if app.auth.has_token {
                ui.horizontal(|ui| {
                    if let Some(t) = secrets::get(Secret::ClaudeOauthToken) {
                        ui.label(format!("Saved token: {}", secrets::mask(&t)));
                    }
                    if ui.button("Remove token").clicked() {
                        secrets::delete(Secret::ClaudeOauthToken);
                        app.auth.refresh(&app.cfg);
                    }
                });
            }
        });
    });
}

fn api_key_card(app: &mut App, ui: &mut egui::Ui) {
    card(ui, |ui| {
        ui.label(RichText::new("Anthropic API key").strong());
        ui.label("Create a key at the Anthropic Console. It is stored in your operating system's credential manager.");
        ui.hyperlink_to("console.anthropic.com → API keys", "https://console.anthropic.com/settings/keys");
        if app.auth.has_key {
            ui.horizontal(|ui| {
                let shown =
                    secrets::get(Secret::AnthropicApiKey).map(|k| secrets::mask(&k)).unwrap_or_else(|| "from ANTHROPIC_API_KEY".into());
                ok_line(ui, true, format!("Key saved: {shown}"));
                if ui.button("Remove").clicked() {
                    secrets::delete(Secret::AnthropicApiKey);
                    app.auth.refresh(&app.cfg);
                }
            });
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut app.auth.key_input).password(true).hint_text("sk-ant-api…").desired_width(360.0));
            let valid = auth::looks_like_api_key(&app.auth.key_input);
            if ui.add_enabled(valid, egui::Button::new("Save key")).clicked() {
                match secrets::set(Secret::AnthropicApiKey, &app.auth.key_input) {
                    Ok(_) => {
                        app.toast(ToastKind::Success, "API key saved");
                        app.auth.key_input.clear();
                        app.auth.refresh(&app.cfg);
                    }
                    Err(e) => app.toast(ToastKind::Error, format!("{e:#}")),
                }
            }
        });
        if app.cfg.backend == BackendKind::ClaudeCodeApiKey {
            if let Some(exe) = app.auth.exe.clone() {
                ui.add_space(4.0);
                ui.label(RichText::new("Alternatively sign Claude Code in to your Console account:").weak());
                if ui.button("Sign in to Anthropic Console…").clicked() {
                    match auth::open_login_terminal(&exe, LoginFlow::Console) {
                        Ok(()) => app.auth.watch_for_login(),
                        Err(e) => app.toast(ToastKind::Error, format!("{e:#}")),
                    }
                }
            }
        }
    });
}
