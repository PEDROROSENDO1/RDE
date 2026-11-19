use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceScope {
    System,
    User,
}

#[derive(Debug, Clone)]
pub struct SystemdService {
    pub unit: String,
    pub scope: ServiceScope,
    pub boot_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub description: String,
}

impl SystemdService {
    pub fn starts_at_boot(&self) -> bool {
        matches!(
            self.boot_state.as_str(),
            "enabled" | "enabled-runtime" | "alias"
        )
    }

    pub fn is_active(&self) -> bool {
        self.active_state == "active"
    }

    pub fn is_failed(&self) -> bool {
        self.active_state == "failed"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
    Enable,
    Disable,
}

impl ServiceAction {
    pub fn as_arg(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "Iniciar",
            Self::Stop => "Parar",
            Self::Restart => "Reiniciar",
            Self::Enable => "Ativar no Boot",
            Self::Disable => "Desativar no Boot",
        }
    }
}

#[derive(Debug)]
pub enum ActionResult {
    Ok(String),
    NeedsPassword,
    Err(String),
}

pub fn load_systemd_services(scope: ServiceScope) -> Result<Vec<SystemdService>, String> {
    let mut boot_states: HashMap<String, String> = HashMap::new();
    let mut active_map: HashMap<String, (String, String, String)> = HashMap::new();

    let mut cmd_files = Command::new("systemctl");
    if scope == ServiceScope::User {
        cmd_files.arg("--user");
    }
    cmd_files.args(["list-unit-files", "--type=service", "--no-pager", "--no-legend"]);

    let out_files = cmd_files
        .output()
        .map_err(|e| format!("Falha ao executar systemctl: {e}"))?;

    if out_files.status.success() {
        let stdout = String::from_utf8_lossy(&out_files.stdout);
        for line in stdout.lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() >= 2 {
                boot_states.insert(cols[0].to_string(), cols[1].to_string());
            }
        }
    }

    let mut cmd_units = Command::new("systemctl");
    if scope == ServiceScope::User {
        cmd_units.arg("--user");
    }
    cmd_units.args(["list-units", "--type=service", "--all", "--no-pager", "--no-legend"]);

    let out_units = cmd_units
        .output()
        .map_err(|e| format!("Falha ao listar unidades systemd: {e}"))?;

    if out_units.status.success() {
        let stdout = String::from_utf8_lossy(&out_units.stdout);
        for line in stdout.lines() {
            let trimmed = line.trim_start_matches(['●', ' ']);
            let mut parts = trimmed.split_whitespace();
            let Some(unit) = parts.next() else { continue };
            let Some(_load) = parts.next() else { continue };
            let Some(active) = parts.next() else { continue };
            let Some(sub) = parts.next() else { continue };
            let desc = parts.collect::<Vec<_>>().join(" ");
            active_map.insert(
                unit.to_string(),
                (active.to_string(), sub.to_string(), desc),
            );
        }
    }

    let mut services = Vec::new();
    for (unit, boot_state) in &boot_states {
        let (active_state, sub_state, description) = active_map
            .remove(unit)
            .unwrap_or_else(|| ("inactive".to_string(), "dead".to_string(), String::new()));
        services.push(SystemdService {
            unit: unit.clone(),
            scope,
            boot_state: boot_state.clone(),
            active_state,
            sub_state,
            description,
        });
    }
    for (unit, (active_state, sub_state, description)) in active_map {
        services.push(SystemdService {
            unit,
            scope,
            boot_state: "transient".to_string(),
            active_state,
            sub_state,
            description,
        });
    }

    services.sort_by(|a, b| a.unit.cmp(&b.unit));
    Ok(services)
}

pub fn run_service_action(
    unit: &str,
    scope: ServiceScope,
    action: ServiceAction,
    sudo_password: Option<&str>,
) -> ActionResult {
    if scope == ServiceScope::User {
        let out = Command::new("systemctl")
            .args(["--user", action.as_arg(), unit])
            .output();
        return match out {
            Ok(o) if o.status.success() => {
                ActionResult::Ok(format!("{} executado em {unit}", action.label()))
            }
            Ok(o) => ActionResult::Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => ActionResult::Err(format!("Erro ao executar systemctl: {e}")),
        };
    }

    run_sudo_command(&["systemctl", action.as_arg(), unit], sudo_password, || {
        format!("{} executado em {unit}", action.label())
    })
}

pub fn fetch_service_logs(
    unit: &str,
    scope: ServiceScope,
    sudo_password: Option<&str>,
) -> ActionResult {
    let mut cmd = Command::new("journalctl");
    if scope == ServiceScope::User {
        cmd.arg("--user");
    }
    cmd.args(["-u", unit, "-n", "80", "--no-pager"]);

    match cmd.output() {
        Ok(o) if o.status.success() => {
            let txt = String::from_utf8_lossy(&o.stdout).to_string();
            if txt.trim().is_empty() && scope == ServiceScope::System {
                // Muitos sistemas restringem journalctl do sistema sem sudo/grupo systemd-journal
                if sudo_password.is_none() {
                    return ActionResult::NeedsPassword;
                }
                return run_sudo_command(
                    &["journalctl", "-u", unit, "-n", "80", "--no-pager"],
                    sudo_password,
                    || String::new(),
                );
            }
            ActionResult::Ok(txt)
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).to_string();
            if scope == ServiceScope::System
                && (err.contains("Permission") || err.contains("No journal files"))
            {
                if sudo_password.is_none() {
                    return ActionResult::NeedsPassword;
                }
                return run_sudo_command(
                    &["journalctl", "-u", unit, "-n", "80", "--no-pager"],
                    sudo_password,
                    || String::new(),
                );
            }
            ActionResult::Err(err.trim().to_string())
        }
        Err(e) => ActionResult::Err(format!("Falha ao executar journalctl: {e}")),
    }
}

pub fn kill_process(pid: u32, signal: i32, sudo_password: Option<&str>) -> ActionResult {
    let rc = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if rc == 0 {
        return ActionResult::Ok(format!("Sinal {signal} enviado para PID {pid}"));
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EPERM) {
        let sig_arg = format!("-{signal}");
        let pid_arg = pid.to_string();
        return run_sudo_command(&["kill", &sig_arg, &pid_arg], sudo_password, || {
            format!("Sinal {signal} enviado (root) para PID {pid}")
        });
    }
    ActionResult::Err(format!("Falha ao sinalizar PID {pid}: {err}"))
}

pub fn run_sudo_command(
    args: &[&str],
    sudo_password: Option<&str>,
    ok_msg: impl FnOnce() -> String,
) -> ActionResult {
    // Primeiro verifica se sudo sem senha funciona (-n)
    if sudo_password.is_none() {
        let try_nopass = Command::new("sudo")
            .arg("-n")
            .args(args)
            .output();
        if let Ok(o) = try_nopass {
            if o.status.success() {
                let out_s = String::from_utf8_lossy(&o.stdout).to_string();
                let custom = ok_msg();
                return ActionResult::Ok(if custom.is_empty() { out_s } else { custom });
            }
        }
        return ActionResult::NeedsPassword;
    }

    let pass = sudo_password.unwrap_or("");
    let Ok(mut child) = Command::new("sudo")
        .args(["-S", "-k", "-p", ""])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return ActionResult::Err("Falha ao iniciar sudo".to_string());
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{pass}");
    }

    match child.wait_with_output() {
        Ok(o) if o.status.success() => {
            let out_s = String::from_utf8_lossy(&o.stdout).to_string();
            let custom = ok_msg();
            ActionResult::Ok(if custom.is_empty() { out_s } else { custom })
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if err.to_lowercase().contains("incorrect password")
                || err.to_lowercase().contains("sorry, try again")
                || err.to_lowercase().contains("senha incorreta")
            {
                ActionResult::NeedsPassword
            } else {
                ActionResult::Err(if err.is_empty() {
                    "Comando sudo falhou".to_string()
                } else {
                    err
                })
            }
        }
        Err(e) => ActionResult::Err(format!("Erro ao aguardar sudo: {e}")),
    }
}
