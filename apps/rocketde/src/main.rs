use clap::Parser;
use rocketde::ipc::{IpcCommand, send_command_to_daemon};
use rocketde::wayland;

#[derive(Parser, Debug)]
#[command(
    name = "rocketde",
    version,
    about = "Barra Superior estilo macOS + Dock Central Inferior + App Launcher em Rust + egui para Wayland (Pila)"
)]
struct Cli {
    /// Alterna (abre/fecha) o Menu de Apps (Spotlight Launcher) via IPC instantâneo
    #[arg(long, short = 'l', alias = "launcher")]
    toggle_launcher: bool,

    /// Abre o Menu de Apps (Spotlight Launcher)
    #[arg(long)]
    open_launcher: bool,

    /// Fecha o Menu de Apps / Popups
    #[arg(long)]
    close_launcher: bool,

    /// Alterna a Visão Geral estilo GNOME (janelas e áreas de trabalho)
    #[arg(long, short = 'o')]
    toggle_overview: bool,

    /// Alterna o Control Center (Volume, Brilho, Wi-Fi, Bluetooth)
    #[arg(long, short = 'c')]
    toggle_control_center: bool,

    /// Alterna a visibilidade da Dock inferior
    #[arg(long, short = 'd')]
    toggle_dock: bool,

    /// Abre o overlay de captura de tela (Print) estilo GNOME
    #[arg(long, short = 's')]
    screenshot: bool,

    /// Abre o overlay de captura já no modo de gravação de tela
    #[arg(long)]
    record: bool,

    /// Para a gravação de tela em andamento
    #[arg(long)]
    stop_record: bool,

    /// Aumenta o volume do sistema em +5%
    #[arg(long)]
    volume_up: bool,

    /// Diminui o volume do sistema em -5%
    #[arg(long)]
    volume_down: bool,

    /// Alterna o Mute do volume do sistema
    #[arg(long)]
    mute: bool,

    /// Aumenta o brilho da tela em +5%
    #[arg(long)]
    brightness_up: bool,

    /// Diminui o brilho da tela em -5%
    #[arg(long)]
    brightness_down: bool,

    /// Recarrega o arquivo de configuração ~/.config/rocketde/config.toml
    #[arg(long, short = 'r')]
    reload: bool,

    /// Encerra a instância do daemon do rocketde em execução
    #[arg(long, short = 'q')]
    quit: bool,
}

fn main() {
    let cli = Cli::parse();

    let requested_cmd = if cli.toggle_launcher {
        Some(IpcCommand::ToggleLauncher)
    } else if cli.open_launcher {
        Some(IpcCommand::OpenLauncher)
    } else if cli.close_launcher {
        Some(IpcCommand::CloseLauncher)
    } else if cli.toggle_overview {
        Some(IpcCommand::ToggleOverview)
    } else if cli.toggle_control_center {
        Some(IpcCommand::ToggleControlCenter)
    } else if cli.toggle_dock {
        Some(IpcCommand::ToggleDock)
    } else if cli.screenshot {
        Some(IpcCommand::OpenScreenshot)
    } else if cli.record {
        Some(IpcCommand::OpenScreenRecord)
    } else if cli.stop_record {
        Some(IpcCommand::StopRecording)
    } else if cli.volume_up {
        Some(IpcCommand::VolumeDelta(5))
    } else if cli.volume_down {
        Some(IpcCommand::VolumeDelta(-5))
    } else if cli.mute {
        Some(IpcCommand::MuteToggle)
    } else if cli.brightness_up {
        Some(IpcCommand::BrightnessDelta(5))
    } else if cli.brightness_down {
        Some(IpcCommand::BrightnessDelta(-5))
    } else if cli.reload {
        Some(IpcCommand::ReloadConfig)
    } else if cli.quit {
        Some(IpcCommand::Quit)
    } else {
        None
    };

    // If a CLI flag was passed, try signaling an already running rocketde daemon in <1ms
    if let Some(ref cmd) = requested_cmd {
        match send_command_to_daemon(cmd) {
            Ok(true) => return,
            Ok(false) => {
                if *cmd == IpcCommand::Quit {
                    eprintln!("[rocketde] Nenhuma instância em execução.");
                    return;
                }
                // If `--toggle-launcher` or `--launcher` was called and no daemon is running yet,
                // start the daemon with the launcher immediately open!
            }
            Err(e) => {
                eprintln!("[rocketde] Aviso IPC: {e}");
            }
        }
    } else {
        // Check if daemon is already running to avoid duplicate bars
        if let Ok(true) = send_command_to_daemon(&IpcCommand::Ping) {
            println!("[rocketde] Já existe uma instância rodando! Abrindo o App Launcher...");
            let _ = send_command_to_daemon(&IpcCommand::ToggleLauncher);
            return;
        }
    }

    let start_with_launcher = matches!(
        requested_cmd,
        Some(IpcCommand::ToggleLauncher) | Some(IpcCommand::OpenLauncher)
    );

    if let Err(err) = wayland::layer_shell::run_wayland_daemon(start_with_launcher) {
        eprintln!("[rocketde] Erro ao executar: {err}");
        std::process::exit(1);
    }
}
