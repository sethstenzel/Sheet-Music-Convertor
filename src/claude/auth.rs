//! Sign-in helpers.
//!
//! Signing in to a Claude subscription is done by Claude Code's own OAuth flow
//! in the browser. The app opens a terminal window running either
//! `claude auth login --claudeai` (signs Claude Code in on this computer) or
//! `claude setup-token` (prints a long-lived token the user pastes into the
//! app, which keeps it in the OS credential store). The app then polls
//! `claude auth status --json` to see when the sign-in finished.

use super::cli::{apply_auth_env, command};
use crate::config::BackendKind;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

#[derive(Clone, Debug, Default)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub method: String,
    pub email: String,
    pub organization: String,
    pub subscription: String,
    pub raw: String,
}

impl AuthStatus {
    pub fn summary(&self) -> String {
        if !self.logged_in {
            return "Not signed in".into();
        }
        let mut s = String::from("Signed in");
        if !self.email.is_empty() {
            s.push_str(&format!(" as {}", self.email));
        }
        let mut extra = Vec::new();
        if !self.subscription.is_empty() {
            extra.push(format!("Claude {}", capitalize(&self.subscription)));
        }
        if !self.method.is_empty() {
            extra.push(self.method.clone());
        }
        if !extra.is_empty() {
            s.push_str(&format!(" ({})", extra.join(", ")));
        }
        s
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Run `claude auth status --json` with the same credentials the app will use.
pub fn status(exe: &Path, kind: BackendKind) -> Result<AuthStatus> {
    let mut cmd = command(exe);
    cmd.args(["auth", "status", "--json"]).current_dir(crate::config::claude_workdir());
    // Without a saved key, still report the CLI's own login.
    let _ = apply_auth_env(&mut cmd, kind);
    let out = cmd.output().with_context(|| format!("running {}", exe.display()))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let v: Value = serde_json::from_str(text.trim()).map_err(|_| {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow!("Unexpected output from `claude auth status`: {}{}", text.trim(), err.trim())
    })?;
    Ok(AuthStatus {
        logged_in: v["loggedIn"].as_bool().unwrap_or(false),
        method: v["authMethod"].as_str().unwrap_or("").to_string(),
        email: v["email"].as_str().unwrap_or("").to_string(),
        organization: v["orgName"].as_str().unwrap_or("").to_string(),
        subscription: v["subscriptionType"].as_str().unwrap_or("").to_string(),
        raw: text,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginFlow {
    /// `claude auth login --claudeai`: signs Claude Code in with a subscription.
    Subscription,
    /// `claude auth login --console`: signs Claude Code in with Console (API) billing.
    Console,
    /// `claude setup-token`: prints a long-lived token for this app.
    SetupToken,
}

impl LoginFlow {
    fn args(self) -> &'static [&'static str] {
        match self {
            LoginFlow::Subscription => &["auth", "login", "--claudeai"],
            LoginFlow::Console => &["auth", "login", "--console"],
            LoginFlow::SetupToken => &["setup-token"],
        }
    }
}

/// Open a visible terminal window running the sign-in flow.
pub fn open_login_terminal(exe: &Path, flow: LoginFlow) -> Result<()> {
    let args = flow.args().join(" ");
    let keep_open = flow == LoginFlow::SetupToken;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let exe_s = exe.display().to_string();
        let inner = if keep_open {
            format!("cmd /K \"\"{exe_s}\" {args}\"")
        } else {
            format!("cmd /C \"\"{exe_s}\" {args} & echo. & echo You can close this window. & timeout /t 8\"")
        };
        Command::new("cmd")
            .raw_arg(format!("/C start \"Sign in to Claude\" {inner}"))
            .current_dir(crate::config::claude_workdir())
            .spawn()
            .context("opening a terminal window")?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let _ = keep_open;
        let script = format!(
            "tell application \"Terminal\"\nactivate\ndo script \"'{}' {}\"\nend tell",
            exe.display().to_string().replace('\'', "'\\''"),
            args
        );
        Command::new("osascript").args(["-e", &script]).spawn().context("opening Terminal")?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let shell_cmd = if keep_open {
            format!("'{}' {}; echo; read -p 'Press Enter to close' _", exe.display(), args)
        } else {
            format!("'{}' {}; sleep 5", exe.display(), args)
        };
        let terms: [(&str, &[&str]); 5] = [
            ("x-terminal-emulator", &["-e", "sh", "-c"]),
            ("gnome-terminal", &["--", "sh", "-c"]),
            ("konsole", &["-e", "sh", "-c"]),
            ("xfce4-terminal", &["-x", "sh", "-c"]),
            ("xterm", &["-e", "sh", "-c"]),
        ];
        for (term, pre) in terms {
            if Command::new(term).args(pre).arg(&shell_cmd).spawn().is_ok() {
                return Ok(());
            }
        }
        Err(anyhow!("No terminal emulator found. Run `{} {}` in a terminal yourself.", exe.display(), args))
    }
}

/// `claude auth logout`
pub fn logout(exe: &Path) -> Result<()> {
    let out = command(exe).args(["auth", "logout"]).current_dir(crate::config::claude_workdir()).output()?;
    if !out.status.success() {
        return Err(anyhow!("{}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// Validate the shape of a token pasted from `claude setup-token`.
pub fn looks_like_oauth_token(t: &str) -> bool {
    let t = t.trim();
    t.starts_with("sk-ant-oat") && t.len() > 40 && !t.contains(char::is_whitespace)
}

pub fn looks_like_api_key(t: &str) -> bool {
    let t = t.trim();
    t.starts_with("sk-ant-") && t.len() > 40 && !t.contains(char::is_whitespace)
}

/// Install instructions shown when Claude Code is missing.
pub const INSTALL_HELP: &str = "Claude Code is required to use a Claude subscription.\n\
Install it from https://claude.com/claude-code — for example:\n\
  • Windows (PowerShell):  irm https://claude.ai/install.ps1 | iex\n\
  • macOS / Linux:         curl -fsSL https://claude.ai/install.sh | bash\n\
  • or with Node.js:       npm install -g @anthropic-ai/claude-code\n\
Then click “Detect again”.";
