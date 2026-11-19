use crate::system::apps::AppIndexer;
use crate::ui::icons::{IconKind, draw_icon};
use crate::ui::{ActiveGlobalMenu, PendingCaptureAction, ShellUiState};
use crate::wayland::virtual_kbd::{
    KEY_0, KEY_A, KEY_B, KEY_C, KEY_D, KEY_DELETE, KEY_E, KEY_EQUAL, KEY_F, KEY_F1, KEY_F3,
    KEY_F5, KEY_F11, KEY_F12, KEY_G, KEY_H, KEY_I, KEY_J, KEY_K, KEY_L, KEY_MINUS, KEY_N, KEY_O,
    KEY_P, KEY_Q, KEY_R, KEY_S, KEY_T, KEY_U, KEY_V, KEY_W, KEY_X, KEY_Y, KEY_Z, ShortcutChord,
};
use crate::wm::pila::{PilaConnector, WindowInfo};
use chrono::{Datelike, Local, NaiveDate, Weekday};
use egui::{
    Align, Color32, Context, CornerRadius, Frame, Layout, Margin, Pos2, Rect, RichText, Sense,
    Stroke, Vec2,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppProfile {
    Terminal,
    Browser,
    CodeEditor,
    FileManager,
    ChatApp,
    GenericApp,
}

#[derive(Clone)]
enum MenuAction {
    Shortcut(ShortcutChord),
    Spawn(String),
    WmAction(String),
    FocusWindow(String),
    CloseWindow(String),
    ToggleLauncher,
    ToggleControlCenter,
    ToggleDock,
    Separator,
}

#[derive(Clone)]
struct MenuItemSpec {
    label: String,
    shortcut: String,
    action: MenuAction,
}

impl MenuItemSpec {
    fn item(label: impl Into<String>, shortcut: impl Into<String>, action: MenuAction) -> Self {
        Self {
            label: label.into(),
            shortcut: shortcut.into(),
            action,
        }
    }

    fn sep() -> Self {
        Self {
            label: String::new(),
            shortcut: String::new(),
            action: MenuAction::Separator,
        }
    }
}

fn detect_app_profile(win: Option<&WindowInfo>) -> AppProfile {
    let Some(w) = win else {
        return AppProfile::GenericApp;
    };
    let id = w.app_id.to_lowercase();
    let title = w.title.to_lowercase();

    if id.contains("foot")
        || id.contains("kitty")
        || id.contains("alacritty")
        || id.contains("wezterm")
        || id.contains("ghostty")
        || id.contains("terminal")
    {
        AppProfile::Terminal
    } else if id.contains("firefox")
        || id.contains("chrom")
        || id.contains("brave")
        || id.contains("zen")
        || title.contains("mozilla firefox")
    {
        AppProfile::Browser
    } else if id.contains("code")
        || id.contains("zed")
        || id.contains("codium")
        || id.contains("nvim")
        || id.contains("neovide")
        || title.contains("nvim")
        || title.contains("vim")
    {
        AppProfile::CodeEditor
    } else if id.contains("nautilus")
        || id.contains("thunar")
        || id.contains("dolphin")
        || id.contains("nemo")
        || id.contains("files")
    {
        AppProfile::FileManager
    } else if id.contains("signal")
        || id.contains("discord")
        || id.contains("vesktop")
        || id.contains("telegram")
        || id.contains("slack")
    {
        AppProfile::ChatApp
    } else {
        AppProfile::GenericApp
    }
}

/// Extracts the real working directory (`~/Projects/rocketde`, etc.) from a terminal window title like `pedro@arch:~/Projects/rocketde`
fn extract_terminal_cwd(win: Option<&WindowInfo>) -> String {
    let Some(w) = win else {
        return "~".to_string();
    };
    if let Some((_, rhs)) = w.title.split_once(':') {
        let path = rhs.trim();
        if !path.is_empty() {
            return path.to_string();
        }
    }
    "~".to_string()
}

/// Builds the exact dynamic macOS menu labels for the Top Bar based on the active application
fn top_bar_menu_titles(profile: AppProfile) -> [(&'static str, ActiveGlobalMenu); 5] {
    match profile {
        AppProfile::Terminal => [
            ("Shell", ActiveGlobalMenu::FileMenu),
            ("Editar", ActiveGlobalMenu::EditMenu),
            ("Visualizar", ActiveGlobalMenu::ViewMenu),
            ("Janela", ActiveGlobalMenu::WindowMenu),
            ("Ajuda", ActiveGlobalMenu::HelpMenu),
        ],
        AppProfile::Browser => [
            ("Arquivo", ActiveGlobalMenu::FileMenu),
            ("Editar", ActiveGlobalMenu::EditMenu),
            ("Exibir / Histórico", ActiveGlobalMenu::ViewMenu),
            ("Janela", ActiveGlobalMenu::WindowMenu),
            ("Ajuda", ActiveGlobalMenu::HelpMenu),
        ],
        AppProfile::CodeEditor => [
            ("Arquivo", ActiveGlobalMenu::FileMenu),
            ("Editar", ActiveGlobalMenu::EditMenu),
            ("Exibir / Exec", ActiveGlobalMenu::ViewMenu),
            ("Janela", ActiveGlobalMenu::WindowMenu),
            ("Ajuda", ActiveGlobalMenu::HelpMenu),
        ],
        _ => [
            ("Arquivo", ActiveGlobalMenu::FileMenu),
            ("Editar", ActiveGlobalMenu::EditMenu),
            ("Visualizar", ActiveGlobalMenu::ViewMenu),
            ("Janela", ActiveGlobalMenu::WindowMenu),
            ("Ajuda", ActiveGlobalMenu::HelpMenu),
        ],
    }
}

/// Builds the real, context-aware menu entries for the currently focused application
fn build_app_menu_items(
    menu: ActiveGlobalMenu,
    profile: AppProfile,
    focused_win: Option<&WindowInfo>,
    all_windows: &[WindowInfo],
    dock_enabled: bool,
    ws_count: usize,
) -> Vec<MenuItemSpec> {
    let win_id = focused_win.map(|w| w.id.clone()).unwrap_or_default();
    let cwd = extract_terminal_cwd(focused_win);
    let expanded_cwd = if let Some(rest) = cwd.strip_prefix('~') {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/pedro".to_string());
        format!("{}{}", home, rest)
    } else {
        cwd.clone()
    };

    match (menu, profile) {
        // ==================== TERMINAL (Foot / Kitty / Alacritty) ====================
        (ActiveGlobalMenu::FileMenu, AppProfile::Terminal) => vec![
            MenuItemSpec::item(
                format!("Nova Janela neste Diretório ({})", cwd),
                "Ctrl+Shift+N",
                MenuAction::Spawn(format!("foot -D \"{}\"", expanded_cwd)),
            ),
            MenuItemSpec::item(
                "Nova Janela Terminal (Home)",
                "Mod+Return",
                MenuAction::Spawn("foot".to_string()),
            ),
            MenuItemSpec::item(
                format!("Abrir '{}' nos Arquivos", cwd),
                "",
                MenuAction::Spawn(format!("xdg-open \"{}\"", expanded_cwd)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Limpar Tela do Terminal",
                "Ctrl+L",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_L)),
            ),
            MenuItemSpec::item(
                "Cancelar Processo em Execução (SIGINT)",
                "Ctrl+C",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_C)),
            ),
            MenuItemSpec::item(
                "Enviar EOF / Encerrar Shell",
                "Ctrl+D",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_D)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Fechar Terminal Focado",
                "Mod+Shift+Q",
                MenuAction::CloseWindow(win_id),
            ),
        ],

        (ActiveGlobalMenu::EditMenu, AppProfile::Terminal) => vec![
            MenuItemSpec::item(
                "Copiar Seleção do Terminal",
                "Ctrl+Shift+C",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_C)),
            ),
            MenuItemSpec::item(
                "Colar no Terminal",
                "Ctrl+Shift+V",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_V)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Pesquisar no Scrollback (Foot)",
                "Ctrl+Shift+R",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_R)),
            ),
            MenuItemSpec::item(
                "Selecionar URLs na Tela (Foot URL Mode)",
                "Ctrl+Shift+O",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_O)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Apagar Palavra Anterior",
                "Ctrl+W",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_W)),
            ),
            MenuItemSpec::item(
                "Apagar Linha Inteira",
                "Ctrl+U",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_U)),
            ),
        ],

        (ActiveGlobalMenu::ViewMenu, AppProfile::Terminal) => vec![
            MenuItemSpec::item(
                "Aumentar Fonte do Terminal (+)",
                "Ctrl++",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_EQUAL)),
            ),
            MenuItemSpec::item(
                "Diminuir Fonte do Terminal (-)",
                "Ctrl+-",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_MINUS)),
            ),
            MenuItemSpec::item(
                "Tamanho Padrão da Fonte (100%)",
                "Ctrl+0",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_0)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Alternar Tela Cheia (Pila)",
                "Mod+F",
                MenuAction::WmAction("fullscreen".to_string()),
            ),
            MenuItemSpec::item(
                if dock_enabled {
                    "Ocultar Dock Inferior"
                } else {
                    "Mostrar Dock Inferior"
                },
                "",
                MenuAction::ToggleDock,
            ),
        ],

        // ==================== BROWSER (Mozilla Firefox / Chromium / Brave) ====================
        (ActiveGlobalMenu::FileMenu, AppProfile::Browser) => vec![
            MenuItemSpec::item(
                "Nova Aba no Navegador",
                "Ctrl+T",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_T)),
            ),
            MenuItemSpec::item(
                "Nova Janela do Navegador",
                "Ctrl+N",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_N)),
            ),
            MenuItemSpec::item(
                "Nova Janela Privativa / Anônima",
                "Ctrl+Shift+P",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_P)),
            ),
            MenuItemSpec::item(
                "Reabrir Última Aba Fechada",
                "Ctrl+Shift+T",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_T)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Focar Barra de Endereços (URL)",
                "Ctrl+L",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_L)),
            ),
            MenuItemSpec::item(
                "Abrir Arquivo Local...",
                "Ctrl+O",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_O)),
            ),
            MenuItemSpec::item(
                "Salvar Página Como...",
                "Ctrl+S",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_S)),
            ),
            MenuItemSpec::item(
                "Imprimir Página...",
                "Ctrl+P",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_P)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Fechar Aba Atual",
                "Ctrl+W",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_W)),
            ),
            MenuItemSpec::item(
                "Fechar Janela do Navegador",
                "Ctrl+Shift+W",
                MenuAction::CloseWindow(win_id),
            ),
        ],

        (ActiveGlobalMenu::EditMenu, AppProfile::Browser) => vec![
            MenuItemSpec::item(
                "Desfazer",
                "Ctrl+Z",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_Z)),
            ),
            MenuItemSpec::item(
                "Refazer",
                "Ctrl+Shift+Z",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_Z)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Recortar",
                "Ctrl+X",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_X)),
            ),
            MenuItemSpec::item(
                "Copiar",
                "Ctrl+C",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_C)),
            ),
            MenuItemSpec::item(
                "Colar",
                "Ctrl+V",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_V)),
            ),
            MenuItemSpec::item(
                "Colar Sem Formatação",
                "Ctrl+Shift+V",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_V)),
            ),
            MenuItemSpec::item(
                "Selecionar Tudo",
                "Ctrl+A",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_A)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Localizar na Página...",
                "Ctrl+F",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_F)),
            ),
            MenuItemSpec::item(
                "Buscar Próxima Ocorrência",
                "Ctrl+G",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_G)),
            ),
        ],

        (ActiveGlobalMenu::ViewMenu, AppProfile::Browser) => vec![
            MenuItemSpec::item(
                "Recarregar Página",
                "Ctrl+R",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_R)),
            ),
            MenuItemSpec::item(
                "Recarregar Ignorando Cache",
                "Ctrl+Shift+R",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_R)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Aumentar Zoom (+)",
                "Ctrl++",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_EQUAL)),
            ),
            MenuItemSpec::item(
                "Diminuir Zoom (-)",
                "Ctrl+-",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_MINUS)),
            ),
            MenuItemSpec::item(
                "Tamanho Real (100%)",
                "Ctrl+0",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_0)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Histórico de Navegação",
                "Ctrl+H",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_H)),
            ),
            MenuItemSpec::item(
                "Downloads",
                "Ctrl+J",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_J)),
            ),
            MenuItemSpec::item(
                "Favoritos / Bookmarks",
                "Ctrl+Shift+O",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_O)),
            ),
            MenuItemSpec::item(
                "Adicionar Página aos Favoritos",
                "Ctrl+D",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_D)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Inspecionar / DevTools",
                "Ctrl+Shift+I / F12",
                MenuAction::Shortcut(ShortcutChord::key_only(KEY_F12)),
            ),
            MenuItemSpec::item(
                "Código-Fonte da Página",
                "Ctrl+U",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_U)),
            ),
            MenuItemSpec::item(
                "Modo Tela Cheia",
                "F11",
                MenuAction::Shortcut(ShortcutChord::key_only(KEY_F11)),
            ),
        ],

        // ==================== CODE EDITOR (VS Code / Zed / Neovim) ====================
        (ActiveGlobalMenu::FileMenu, AppProfile::CodeEditor) => vec![
            MenuItemSpec::item(
                "Novo Arquivo",
                "Ctrl+N",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_N)),
            ),
            MenuItemSpec::item(
                "Nova Janela do Editor",
                "Ctrl+Shift+N",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_N)),
            ),
            MenuItemSpec::item(
                "Abrir Arquivo...",
                "Ctrl+O",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_O)),
            ),
            MenuItemSpec::item(
                "Busca Rápida de Arquivos (Quick Open)",
                "Ctrl+P",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_P)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Salvar Arquivo",
                "Ctrl+S",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_S)),
            ),
            MenuItemSpec::item(
                "Salvar Como...",
                "Ctrl+Shift+S",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_S)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Fechar Editor / Aba",
                "Ctrl+W",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_W)),
            ),
            MenuItemSpec::item(
                "Fechar Janela",
                "Mod+Shift+Q",
                MenuAction::CloseWindow(win_id),
            ),
        ],

        (ActiveGlobalMenu::EditMenu, AppProfile::CodeEditor) => vec![
            MenuItemSpec::item(
                "Desfazer",
                "Ctrl+Z",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_Z)),
            ),
            MenuItemSpec::item(
                "Refazer",
                "Ctrl+Shift+Z",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_Z)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Recortar",
                "Ctrl+X",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_X)),
            ),
            MenuItemSpec::item(
                "Copiar",
                "Ctrl+C",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_C)),
            ),
            MenuItemSpec::item(
                "Colar",
                "Ctrl+V",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_V)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Buscar no Arquivo",
                "Ctrl+F",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_F)),
            ),
            MenuItemSpec::item(
                "Substituir no Arquivo",
                "Ctrl+H",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_H)),
            ),
            MenuItemSpec::item(
                "Buscar em Todo o Projeto",
                "Ctrl+Shift+F",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_F)),
            ),
        ],

        (ActiveGlobalMenu::ViewMenu, AppProfile::CodeEditor) => vec![
            MenuItemSpec::item(
                "Command Palette (Paleta de Comandos)",
                "Ctrl+Shift+P",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_P)),
            ),
            MenuItemSpec::item(
                "Explorador de Arquivos",
                "Ctrl+Shift+E",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_E)),
            ),
            MenuItemSpec::item(
                "Controle de Código-Fonte (Git)",
                "Ctrl+Shift+G",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_G)),
            ),
            MenuItemSpec::item(
                "Alternar Barra Lateral",
                "Ctrl+B",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_B)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Aumentar Zoom (+)",
                "Ctrl++",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_EQUAL)),
            ),
            MenuItemSpec::item(
                "Diminuir Zoom (-)",
                "Ctrl+-",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_MINUS)),
            ),
            MenuItemSpec::item(
                "Tela Cheia",
                "F11",
                MenuAction::Shortcut(ShortcutChord::key_only(KEY_F11)),
            ),
        ],

        // ==================== FILE MANAGER (Nautilus / Thunar / Dolphin) ====================
        (ActiveGlobalMenu::FileMenu, AppProfile::FileManager) => vec![
            MenuItemSpec::item(
                "Nova Aba de Arquivos",
                "Ctrl+T",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_T)),
            ),
            MenuItemSpec::item(
                "Nova Janela de Arquivos",
                "Ctrl+N",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_N)),
            ),
            MenuItemSpec::item(
                "Criar Nova Pasta",
                "Ctrl+Shift+N",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_N)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Abrir Terminal Nesta Pasta",
                "Mod+Return",
                MenuAction::Spawn("foot".to_string()),
            ),
            MenuItemSpec::item(
                "Propriedades do Item",
                "Alt+Enter / Ctrl+I",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_I)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Fechar Aba",
                "Ctrl+W",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_W)),
            ),
            MenuItemSpec::item(
                "Fechar Janela",
                "Ctrl+Q",
                MenuAction::CloseWindow(win_id),
            ),
        ],

        (ActiveGlobalMenu::EditMenu, AppProfile::FileManager) => vec![
            MenuItemSpec::item(
                "Desfazer Ação",
                "Ctrl+Z",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_Z)),
            ),
            MenuItemSpec::item(
                "Refazer Ação",
                "Ctrl+Y",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_Y)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Recortar Arquivo",
                "Ctrl+X",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_X)),
            ),
            MenuItemSpec::item(
                "Copiar Arquivo",
                "Ctrl+C",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_C)),
            ),
            MenuItemSpec::item(
                "Colar Arquivo",
                "Ctrl+V",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_V)),
            ),
            MenuItemSpec::item(
                "Selecionar Todos os Arquivos",
                "Ctrl+A",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_A)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Editar Caminho / Localização",
                "Ctrl+L",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_L)),
            ),
            MenuItemSpec::item(
                "Pesquisar Arquivos...",
                "Ctrl+F",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_F)),
            ),
            MenuItemSpec::item(
                "Mover para a Lixeira",
                "Delete",
                MenuAction::Shortcut(ShortcutChord::key_only(KEY_DELETE)),
            ),
        ],

        (ActiveGlobalMenu::ViewMenu, AppProfile::FileManager) => vec![
            MenuItemSpec::item(
                "Mostrar / Ocultar Arquivos Ocultos (.dotfiles)",
                "Ctrl+H",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_H)),
            ),
            MenuItemSpec::item(
                "Recarregar Pasta",
                "Ctrl+R / F5",
                MenuAction::Shortcut(ShortcutChord::key_only(KEY_F5)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Aumentar Ícones (+)",
                "Ctrl++",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_EQUAL)),
            ),
            MenuItemSpec::item(
                "Diminuir Ícones (-)",
                "Ctrl+-",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_MINUS)),
            ),
        ],

        // ==================== CHAT APPS (Signal / Discord / Telegram) & GENERIC APPS ====================
        (ActiveGlobalMenu::FileMenu, _) => vec![
            MenuItemSpec::item(
                "Nova Conversa / Novo Documento",
                "Ctrl+N",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_N)),
            ),
            MenuItemSpec::item(
                "Busca Rápida / Trocar Conversa",
                "Ctrl+K",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_K)),
            ),
            MenuItemSpec::item(
                "Abrir Arquivo...",
                "Ctrl+O",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_O)),
            ),
            MenuItemSpec::item(
                "Salvar",
                "Ctrl+S",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_S)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Novo Terminal (Foot)",
                "Mod+Return",
                MenuAction::Spawn("foot".to_string()),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Fechar Aba",
                "Ctrl+W",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_W)),
            ),
            MenuItemSpec::item(
                "Fechar Janela Focada",
                "Mod+Shift+Q",
                MenuAction::CloseWindow(win_id),
            ),
            MenuItemSpec::item(
                "Sair do Aplicativo",
                "Ctrl+Q",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_Q)),
            ),
        ],

        (ActiveGlobalMenu::EditMenu, _) => vec![
            MenuItemSpec::item(
                "Desfazer",
                "Ctrl+Z",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_Z)),
            ),
            MenuItemSpec::item(
                "Refazer",
                "Ctrl+Shift+Z",
                MenuAction::Shortcut(ShortcutChord::ctrl_shift(KEY_Z)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Recortar",
                "Ctrl+X",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_X)),
            ),
            MenuItemSpec::item(
                "Copiar",
                "Ctrl+C",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_C)),
            ),
            MenuItemSpec::item(
                "Colar",
                "Ctrl+V",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_V)),
            ),
            MenuItemSpec::item(
                "Selecionar Tudo",
                "Ctrl+A",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_A)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Localizar...",
                "Ctrl+F",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_F)),
            ),
        ],

        (ActiveGlobalMenu::ViewMenu, _) => vec![
            MenuItemSpec::item(
                "Alternar Tela Cheia (Pila)",
                "Mod+F",
                MenuAction::WmAction("fullscreen".to_string()),
            ),
            MenuItemSpec::item(
                "Recarregar View",
                "Ctrl+R",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_R)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                "Aumentar Zoom (+)",
                "Ctrl++",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_EQUAL)),
            ),
            MenuItemSpec::item(
                "Diminuir Zoom (-)",
                "Ctrl+-",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_MINUS)),
            ),
            MenuItemSpec::item(
                "Tamanho Padrão (100%)",
                "Ctrl+0",
                MenuAction::Shortcut(ShortcutChord::ctrl(KEY_0)),
            ),
            MenuItemSpec::sep(),
            MenuItemSpec::item(
                if dock_enabled {
                    "Ocultar Dock Inferior"
                } else {
                    "Mostrar Dock Inferior"
                },
                "",
                MenuAction::ToggleDock,
            ),
        ],

        // ==================== WINDOW MENU (Live Window State + Open Windows Switcher) ====================
        (ActiveGlobalMenu::WindowMenu, _) => {
            let is_max = focused_win.map(|w| w.maximized).unwrap_or(false);
            let is_full = focused_win.map(|w| w.fullscreen).unwrap_or(false);
            let is_top = focused_win.map(|w| w.always_on_top).unwrap_or(false);
            let snap_left = focused_win
                .and_then(|w| w.snapped.as_deref())
                 == Some("left");
            let snap_right = focused_win
                .and_then(|w| w.snapped.as_deref())
                 == Some("right");

            let mut list = vec![
                MenuItemSpec::item(
                    format!("{} Maximizar Janela", if is_max { "✓" } else { "  " }),
                    "Mod+M",
                    MenuAction::WmAction("maximize".to_string()),
                ),
                MenuItemSpec::item(
                    format!("{} Tela Cheia", if is_full { "✓" } else { "  " }),
                    "Mod+F",
                    MenuAction::WmAction("fullscreen".to_string()),
                ),
                MenuItemSpec::item(
                    "   Minimizar Janela",
                    "Mod+N",
                    MenuAction::WmAction("minimize".to_string()),
                ),
                MenuItemSpec::item(
                    "   Restaurar Tamanho Original",
                    "Mod+Down",
                    MenuAction::WmAction("restore".to_string()),
                ),
                MenuItemSpec::item(
                    "   Centralizar na Tela",
                    "Mod+C",
                    MenuAction::WmAction("center".to_string()),
                ),
                MenuItemSpec::sep(),
                MenuItemSpec::item(
                    format!("{} Snap Metade Esquerda", if snap_left { "✓" } else { "  " }),
                    "Mod+Left",
                    MenuAction::WmAction("snap-left".to_string()),
                ),
                MenuItemSpec::item(
                    format!(
                        "{} Snap Metade Direita",
                        if snap_right { "✓" } else { "  " }
                    ),
                    "Mod+Right",
                    MenuAction::WmAction("snap-right".to_string()),
                ),
                MenuItemSpec::sep(),
                MenuItemSpec::item(
                    format!("{} Sempre no Topo (Pin)", if is_top { "✓" } else { "  " }),
                    "Mod+T",
                    MenuAction::WmAction("toggle-always-on-top".to_string()),
                ),
            ];

            let cur_ws = focused_win.map(|w| w.workspace).unwrap_or(1);
            list.push(MenuItemSpec::sep());
            for ws_i in 1..=ws_count.min(4) {
                list.push(MenuItemSpec::item(
                    format!(
                        "{} Mover para Desktop {}",
                        if cur_ws == ws_i { "•" } else { " " },
                        ws_i
                    ),
                    format!("Mod+Shift+{}", ws_i),
                    MenuAction::WmAction(format!("move-to-workspace {}", ws_i)),
                ));
            }

            if !all_windows.is_empty() {
                list.push(MenuItemSpec::sep());
                for w in all_windows.iter().take(6) {
                    let check = if w.id == win_id { "✓" } else { " " };
                    let clean_t: String = if !w.title.is_empty() {
                        w.title.chars().take(24).collect()
                    } else {
                        w.app_id.clone()
                    };
                    list.push(MenuItemSpec::item(
                        format!("{} {}", check, clean_t),
                        format!("WS {}", w.workspace),
                        MenuAction::FocusWindow(w.id.clone()),
                    ));
                }
            }

            list
        }

        // ==================== HELP MENU (Context-Sensitive) ====================
        (ActiveGlobalMenu::HelpMenu, _) => {
            let mut list = vec![
                MenuItemSpec::item(
                    "Abrir Launchpad de Aplicativos...",
                    "Super+Space",
                    MenuAction::ToggleLauncher,
                ),
                MenuItemSpec::item(
                    "Abrir Control Center...",
                    "",
                    MenuAction::ToggleControlCenter,
                ),
                MenuItemSpec::sep(),
            ];
            match profile {
                AppProfile::Terminal => {
                    list.push(MenuItemSpec::item(
                        "Manual do Foot Terminal (man foot)",
                        "",
                        MenuAction::Spawn("foot man foot".to_string()),
                    ));
                }
                AppProfile::Browser => {
                    list.push(MenuItemSpec::item(
                        "Ajuda do Navegador",
                        "F1",
                        MenuAction::Shortcut(ShortcutChord::key_only(KEY_F1)),
                    ));
                    list.push(MenuItemSpec::item(
                        "Pesquisar na Página",
                        "F3",
                        MenuAction::Shortcut(ShortcutChord::key_only(KEY_F3)),
                    ));
                }
                _ => {
                    list.push(MenuItemSpec::item(
                        "Ajuda do Aplicativo",
                        "F1",
                        MenuAction::Shortcut(ShortcutChord::key_only(KEY_F1)),
                    ));
                }
            }
            list
        }

        _ => Vec::new(),
    }
}

pub fn render_top_bar(ui: &mut egui::Ui, state: &mut ShellUiState) {
    let ctx = ui.ctx().clone();
    let t = &state.config.theme;
    let bar_bg = Color32::from_rgba_unmultiplied(
        t.bar_bg_rgba[0],
        t.bar_bg_rgba[1],
        t.bar_bg_rgba[2],
        t.bar_bg_rgba[3],
    );
    let accent = Color32::from_rgb(t.accent_rgb[0], t.accent_rgb[1], t.accent_rgb[2]);
    let border_col = Color32::from_rgba_unmultiplied(
        t.border_rgba[0],
        t.border_rgba[1],
        t.border_rgba[2],
        t.border_rgba[3],
    );

    let focused_win = state
        .wm
        .state
        .lock()
        .ok()
        .and_then(|g| g.focused_window.clone());
    let (battery_percent, battery_charging) = state
        .sys
        .snapshot
        .lock()
        .map(|g| (g.battery_percent, g.battery_charging))
        .unwrap_or((None, false));
    let (brightness_percent, volume_percent, muted) = state
        .hw
        .state
        .lock()
        .map(|g| (g.brightness_percent, g.volume_percent, g.muted))
        .unwrap_or((100.0, 50.0, false));
    let (media_available, media_playing, media_title) = if state.config.top_bar.show_media {
        state
            .media
            .info
            .lock()
            .map(|g| (g.available, g.playing, g.title.clone()))
            .unwrap_or((false, false, String::new()))
    } else {
        (false, false, String::new())
    };

    let profile = detect_app_profile(focused_win.as_ref());

    let bar_h = state.config.top_bar.height as f32;
    let _ = border_col;
    let bar_frame = Frame::NONE
        .fill(bar_bg)
        .inner_margin(Margin::symmetric(12, 2))
        .stroke(Stroke::NONE);

    egui::Panel::top("rocketde_macos_top_bar")
        .exact_size(bar_h)
        .frame(bar_frame)
        .show(ui, |ui| {
            let full_rect = ui.max_rect();
            // Ultra-subtle bottom hairline only (no 4-sided gray box border)
            ui.painter().line_segment(
                [
                    Pos2::new(full_rect.min.x - 12.0, full_rect.max.y + 1.5),
                    Pos2::new(full_rect.max.x + 12.0, full_rect.max.y + 1.5),
                ],
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 14)),
            );

            // ================= 1. LEFT SECTION (Rocket Logo + Active App + Dynamic App Menus) =================
            let left_rect = Rect::from_min_max(
                full_rect.min,
                Pos2::new(full_rect.center().x - 125.0, full_rect.max.y),
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(left_rect)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;

                    // Apple / Rocket Logo Menu Button (crisp vector icon)
                    let apple_selected = state.active_menu == ActiveGlobalMenu::AppleMenu;
                    let (logo_rect, logo_resp) =
                        ui.allocate_exact_size(Vec2::new(26.0, 22.0), Sense::click());
                    let logo_bg = if apple_selected {
                        accent
                    } else if logo_resp.hovered() {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 22)
                    } else {
                        Color32::TRANSPARENT
                    };
                    if logo_bg != Color32::TRANSPARENT {
                        ui.painter()
                            .rect_filled(logo_rect, CornerRadius::same(5), logo_bg);
                    }
                    draw_icon(
                        ui.painter(),
                        logo_rect.center(),
                        13.0,
                        IconKind::Rocket,
                        Color32::WHITE,
                    );
                    if logo_resp.clicked() {
                        state.active_menu_anchor_x = logo_resp.rect.min.x;
                        state.active_menu = if apple_selected {
                            ActiveGlobalMenu::None
                        } else {
                            ActiveGlobalMenu::AppleMenu
                        };
                        state.control_center_open = false;
                    } else if matches!(
                        state.active_menu,
                        ActiveGlobalMenu::AppleMenu
                            | ActiveGlobalMenu::FileMenu
                            | ActiveGlobalMenu::EditMenu
                            | ActiveGlobalMenu::ViewMenu
                            | ActiveGlobalMenu::WindowMenu
                            | ActiveGlobalMenu::HelpMenu
                    ) && logo_resp.hovered()
                    {
                        state.active_menu_anchor_x = logo_resp.rect.min.x;
                        state.active_menu = ActiveGlobalMenu::AppleMenu;
                    }

                    // Active Application Name (Bold macOS Style)
                    let active_app_name = focused_win
                        .as_ref()
                        .map(|w| {
                            if !w.app_id.is_empty() {
                                capitalize_first(&w.app_id)
                            } else if !w.title.is_empty() {
                                w.title.chars().take(16).collect()
                            } else {
                                "Área de Trabalho".to_string()
                            }
                        })
                        .unwrap_or_else(|| "Área de Trabalho".to_string());

                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(&active_app_name)
                            .strong()
                            .size(13.5)
                            .color(Color32::WHITE),
                    );
                    ui.add_space(4.0);

                    // Context-Sensitive Global App Menus (e.g. Shell/Editar/Visualizar for Terminal, Arquivo/Editar/Exibir for Browser)
                    if state.config.top_bar.show_global_menu {
                        let menus = top_bar_menu_titles(profile);
                        for (label, menu_kind) in menus {
                            let is_open = state.active_menu == menu_kind;
                            let btn = egui::Button::new(
                                RichText::new(label).size(12.5).color(if is_open {
                                    Color32::WHITE
                                } else {
                                    Color32::from_rgb(225, 229, 238)
                                }),
                            )
                            .fill(if is_open {
                                accent
                            } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(CornerRadius::same(5));

                            let resp = ui.add(btn);
                            if resp.clicked() {
                                state.active_menu_anchor_x = resp.rect.min.x;
                                state.active_menu = if is_open {
                                    ActiveGlobalMenu::None
                                } else {
                                    menu_kind
                                };
                                state.control_center_open = false;
                            } else if matches!(
                                state.active_menu,
                                ActiveGlobalMenu::AppleMenu
                                    | ActiveGlobalMenu::FileMenu
                                    | ActiveGlobalMenu::EditMenu
                                    | ActiveGlobalMenu::ViewMenu
                                    | ActiveGlobalMenu::WindowMenu
                                    | ActiveGlobalMenu::HelpMenu
                            ) && resp.hovered()
                            {
                                state.active_menu_anchor_x = resp.rect.min.x;
                                state.active_menu = menu_kind;
                            }
                        }
                    }
                },
            );

            // ================= 2. EXACT GEOMETRIC CENTER (Clock & Date) =================
            let center_w = 320.0;
            let center_rect = Rect::from_center_size(
                full_rect.center(),
                Vec2::new(center_w, full_rect.height()),
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(center_rect)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    let now_str = Local::now()
                        .format(&state.config.top_bar.clock_format)
                        .to_string();

                    let cal_open = state.active_menu == ActiveGlobalMenu::CalendarPopup;
                    let pill_fill = if cal_open {
                        accent
                    } else {
                        Color32::TRANSPARENT
                    };

                    let text_w = ui.fonts_mut(|f| {
                        f.layout_no_wrap(
                            now_str.clone(),
                            egui::FontId::proportional(13.0),
                            Color32::WHITE,
                        )
                        .size()
                        .x
                    });
                    let btn_w = (text_w + 20.0).max(96.0);
                    let btn_h = (full_rect.height() - 6.0).max(20.0);
                    let clock_rect =
                        Rect::from_center_size(full_rect.center(), Vec2::new(btn_w, btn_h));

                    let resp = ui.allocate_rect(clock_rect, Sense::click());
                    let bg = if resp.hovered() && !cal_open {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 22)
                    } else {
                        pill_fill
                    };

                    ui.painter().rect(
                        clock_rect,
                        CornerRadius::same(6),
                        bg,
                        Stroke::NONE,
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().text(
                        clock_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        &now_str,
                        egui::FontId::proportional(13.0),
                        Color32::WHITE,
                    );

                    if resp.clicked() {
                        state.active_menu_anchor_x = clock_rect.center().x - 150.0;
                        state.active_menu_rect = None;
                        if cal_open {
                            state.active_menu = ActiveGlobalMenu::None;
                        } else {
                            let today = Local::now();
                            state.calendar_year = today.year();
                            state.calendar_month = today.month();
                            state.active_menu = ActiveGlobalMenu::CalendarPopup;
                        }
                        state.control_center_open = false;
                    }
                },
            );

            // ================= 3. RIGHT SECTION (Tray / CPU / RAM / Network / Volume / Brilho / Battery / Control Center) =================
            let right_rect = Rect::from_min_max(
                Pos2::new(full_rect.center().x + 125.0, full_rect.min.y),
                full_rect.max,
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(right_rect)
                    .layout(Layout::right_to_left(Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;

                    // Control Center Toggle Button
                    let cc_resp = tray_icon_pill(
                        ui,
                        IconKind::ControlCenter,
                        "",
                        state.control_center_open,
                        accent,
                        Color32::WHITE,
                    )
                    .on_hover_text("Control Center (Volume, Brilho, Wi-Fi, Bluetooth)");

                    if cc_resp.clicked() {
                        state.control_center_open = !state.control_center_open;
                        state.active_menu = ActiveGlobalMenu::None;
                        state.launcher_open = false;
                    }

                    // Battery Pill
                    if let Some(bat) = battery_percent {
                        let bat_open = state.active_menu == ActiveGlobalMenu::BatteryPopup;
                        let bat_resp = tray_icon_pill(
                            ui,
                            IconKind::Battery(bat, battery_charging),
                            &format!("{}%", bat),
                            bat_open,
                            accent,
                            Color32::WHITE,
                        )
                        .on_hover_text("Clique para ver detalhes de bateria, consumo (W) e performance");

                        if bat_resp.clicked() {
                            state.active_menu_anchor_x = bat_resp.rect.center().x - 157.0;
                            state.active_menu_rect = None;
                            state.active_menu = if bat_open {
                                ActiveGlobalMenu::None
                            } else {
                                ActiveGlobalMenu::BatteryPopup
                            };
                            state.control_center_open = false;
                        }
                    }

                    // Brightness Indicator
                    let br_open = state.active_menu == ActiveGlobalMenu::BrightnessPopup;
                    let br_resp = tray_icon_pill(
                        ui,
                        IconKind::Sun,
                        &format!("{:.0}%", brightness_percent),
                        br_open,
                        accent,
                        Color32::WHITE,
                    )
                    .on_hover_text("Clique para ajustar o brilho ou role o scroll");

                    if br_resp.clicked() {
                        state.active_menu_anchor_x = br_resp.rect.center().x - 140.0;
                        state.active_menu_rect = None;
                        state.active_menu = if br_open {
                            ActiveGlobalMenu::None
                        } else {
                            ActiveGlobalMenu::BrightnessPopup
                        };
                        state.control_center_open = false;
                    }
                    if br_resp.hovered() {
                        let scroll_y = ctx.input(|i| i.smooth_scroll_delta.y);
                        if scroll_y.abs() > 0.5 {
                            let step = if scroll_y > 0.0 { 5.0 } else { -5.0 };
                            state.hw.set_brightness(brightness_percent + step);
                        }
                    }

                    // Volume Indicator
                    let vol_open = state.active_menu == ActiveGlobalMenu::VolumePopup;
                    let vol_kind = if muted || volume_percent <= 0.5 {
                        IconKind::VolumeMute
                    } else if volume_percent < 45.0 {
                        IconKind::VolumeLow
                    } else {
                        IconKind::VolumeHigh
                    };
                    let vol_resp = tray_icon_pill(
                        ui,
                        vol_kind,
                        &format!("{:.0}%", volume_percent),
                        vol_open,
                        accent,
                        Color32::WHITE,
                    )
                    .on_hover_text("Clique para abrir Áudio (até 150%), botão direito para Mute ou role o scroll");

                    if vol_resp.clicked() {
                        state.active_menu_anchor_x = vol_resp.rect.center().x - 147.0;
                        state.active_menu_rect = None;
                        state.active_menu = if vol_open {
                            ActiveGlobalMenu::None
                        } else {
                            ActiveGlobalMenu::VolumePopup
                        };
                        state.control_center_open = false;
                    } else if vol_resp.secondary_clicked() {
                        state.hw.toggle_mute();
                    }
                    if vol_resp.hovered() {
                        let scroll_y = ctx.input(|i| i.smooth_scroll_delta.y);
                        if scroll_y.abs() > 0.5 {
                            let step = if scroll_y > 0.0 { 4.0 } else { -4.0 };
                            state.hw.set_volume(volume_percent + step);
                        }
                    }

                    // Active Screen Recording Stop Button (next to Volume)
                    if state.recording_active {
                        let secs = state
                            .recording_started_at
                            .map(|t| t.elapsed().as_secs())
                            .unwrap_or(0);
                        let rec_label = format!("■ Parar {:02}:{:02}", secs / 60, secs % 60);
                        let rec_btn = egui::Button::new(
                            RichText::new(rec_label)
                                .size(11.5)
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(Color32::from_rgb(215, 45, 45))
                        .corner_radius(CornerRadius::same(10))
                        .min_size(Vec2::new(76.0, 20.0));
                        if ui
                            .add(rec_btn)
                            .on_hover_text("Gravando tela — clique para parar a gravação")
                            .clicked()
                        {
                            state.pending_capture_action =
                                Some(PendingCaptureAction::StopRecording);
                        }
                    }

                    // MPRIS Media Pill
                    if media_available {
                        let icon = if media_playing {
                            IconKind::Pause
                        } else {
                            IconKind::Play
                        };
                        let track: String = media_title.chars().take(18).collect();
                        let resp = tray_icon_pill(
                            ui,
                            icon,
                            &track,
                            false,
                            accent,
                            Color32::from_rgb(230, 235, 245),
                        );
                        if resp.clicked() {
                            state.media.play_pause();
                        } else if resp.secondary_clicked() {
                            state.media.next();
                        }
                    }
                },
            );
        });
}

/// Renders the context-sensitive macOS dropdown menus flush against the bottom of the Top Bar (`y = 2.0`).
pub fn render_global_menu_popups(ctx: &Context, state: &mut ShellUiState) {
    if state.active_menu == ActiveGlobalMenu::None {
        state.active_menu_rect = None;
        return;
    }

    let screen = ctx.content_rect();
    let need_windows = matches!(
        state.active_menu,
        ActiveGlobalMenu::WindowMenu | ActiveGlobalMenu::CalendarPopup
    );
    let (focused_win, wm_windows) = state
        .wm
        .state
        .lock()
        .map(|g| {
            (
                g.focused_window.clone(),
                if need_windows {
                    g.windows.clone()
                } else {
                    Vec::new()
                },
            )
        })
        .unwrap_or((None, Vec::new()));
    let profile = detect_app_profile(focused_win.as_ref());

    let app_header = focused_win
        .as_ref()
        .map(|w| {
            let app = capitalize_first(&w.app_id);
            if !w.title.is_empty() && w.title.to_lowercase() != w.app_id.to_lowercase() {
                let short_t: String = w.title.chars().take(24).collect();
                format!("{} — {}", app, short_t)
            } else {
                app
            }
        })
        .unwrap_or_else(|| "Finder".to_string());

    let width = match state.active_menu {
        ActiveGlobalMenu::AppleMenu => 255.0,
        ActiveGlobalMenu::FileMenu => 295.0,
        ActiveGlobalMenu::EditMenu => 285.0,
        ActiveGlobalMenu::ViewMenu => 290.0,
        ActiveGlobalMenu::WindowMenu => 290.0,
        ActiveGlobalMenu::HelpMenu => 270.0,
        ActiveGlobalMenu::CalendarPopup => 300.0,
        ActiveGlobalMenu::VolumePopup => 295.0,
        ActiveGlobalMenu::BrightnessPopup => 280.0,
        ActiveGlobalMenu::BatteryPopup => 315.0,
        ActiveGlobalMenu::None => return,
    };

    let anchor_x = state
        .active_menu_anchor_x
        .clamp(6.0, (screen.width() - width - 8.0).max(6.0));
    let pos = egui::pos2(anchor_x, 2.0);
    let approx_menu_rect = state
        .active_menu_rect
        .unwrap_or_else(|| Rect::from_min_size(pos, Vec2::new(width, 200.0)));

    // Close menu automatically when clicking anywhere outside the popup, and refocus active window!
    if ctx.input(|i| i.pointer.any_pressed()) {
        if let Some(click_pos) = ctx.input(|i| i.pointer.interact_pos()) {
            if !approx_menu_rect.contains(click_pos) {
                state.active_menu = ActiveGlobalMenu::None;
                state.active_menu_rect = None;
                if let Some(ref w) = focused_win {
                    if !w.id.is_empty() {
                        PilaConnector::focus_window(&w.id);
                    }
                }
                return;
            }
        }
    }

    let t = &state.config.theme;
    let accent = Color32::from_rgb(t.accent_rgb[0], t.accent_rgb[1], t.accent_rgb[2]);
    let popup_frame = Frame::NONE
        .fill(Color32::from_rgba_unmultiplied(
            t.panel_bg_rgba[0],
            t.panel_bg_rgba[1],
            t.panel_bg_rgba[2],
            248,
        ))
        .corner_radius(CornerRadius {
            nw: 4,
            ne: 4,
            sw: 11,
            se: 11,
        })
        .inner_margin(Margin::symmetric(8, 8))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 48),
        ));

    let dynamic_items = if matches!(
        state.active_menu,
        ActiveGlobalMenu::AppleMenu
            | ActiveGlobalMenu::CalendarPopup
            | ActiveGlobalMenu::VolumePopup
            | ActiveGlobalMenu::BrightnessPopup
            | ActiveGlobalMenu::BatteryPopup
    ) {
        Vec::new()
    } else {
        build_app_menu_items(
            state.active_menu,
            profile,
            focused_win.as_ref(),
            &wm_windows,
            state.config.dock.enabled,
            state.config.top_bar.workspace_count,
        )
    };

    let win_resp = egui::Window::new("rocketde_global_menu_popup")
        .order(egui::Order::Foreground)
        .title_bar(false)
        .resizable(false)
        .fixed_pos(pos)
        .fixed_size(egui::vec2(width, 0.0))
        .frame(popup_frame)
        .show(ctx, |ui| {
            ui.set_min_width(width - 16.0);
            ui.spacing_mut().item_spacing.y = 2.0;

            match state.active_menu {
                ActiveGlobalMenu::AppleMenu => {
                    ui.label(
                        RichText::new("  RocketDE Desktop")
                            .size(11.5)
                            .strong()
                            .color(Color32::from_rgb(160, 170, 190)),
                    );
                    ui.separator();
                    if macos_menu_row(ui, "Visão Geral (Atividades)", "Canto Sup. Esq.", accent) {
                        state.toggle_overview();
                    }
                    if macos_menu_row(ui, "Abrir Launchpad (Tela Cheia)...", "Super+Space", accent)
                    {
                        state.toggle_launcher();
                    }
                    if macos_menu_row(ui, "Abrir Control Center...", "", accent) {
                        state.control_center_open = true;
                        state.active_menu = ActiveGlobalMenu::None;
                    }
                    if macos_menu_row(ui, "Configurações do RocketDE...", "Ctrl+,", accent) {
                        let cmd = std::env::current_exe()
                            .ok()
                            .and_then(|p| p.parent().map(|d| d.join("rocketde-settings")))
                            .filter(|p| p.exists())
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "rocketde-settings".to_string());
                        AppIndexer::launch(&cmd);
                        state.active_menu = ActiveGlobalMenu::None;
                    }
                    if macos_menu_row(ui, "Recarregar Barra & Tema", "", accent) {
                        state.config = crate::config::RocketConfig::load_or_create();
                        state.active_menu = ActiveGlobalMenu::None;
                    }
                    ui.separator();
                    if macos_menu_row(ui, "Mostrar Área de Trabalho", "Mod+H", accent) {
                        PilaConnector::run_action("show-desktop");
                        state.active_menu = ActiveGlobalMenu::None;
                    }
                    if macos_menu_row(ui, "Desligar Monitores (Economia)", "", accent) {
                        PilaConnector::run_action("power-off-monitors");
                        state.active_menu = ActiveGlobalMenu::None;
                    }
                    ui.separator();
                    if macos_menu_row(ui, "Encerrar Sessão Pila", "Mod+Shift+E", accent) {
                        PilaConnector::run_action("quit");
                        state.active_menu = ActiveGlobalMenu::None;
                    }
                }

                ActiveGlobalMenu::CalendarPopup => {
                    let now = Local::now();
                    let date_pt = format!(
                        "{}, {} de {} de {}",
                        weekday_pt(now.weekday()),
                        now.day(),
                        month_pt_lower(now.month()),
                        now.year()
                    );
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(date_pt)
                                .strong()
                                .size(13.0)
                                .color(Color32::WHITE),
                        );
                        ui.label(
                            RichText::new(now.format("%H:%M:%S").to_string())
                                .size(24.0)
                                .strong()
                                .color(accent),
                        );
                    });
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);

                    render_month_calendar(ui, state, now.date_naive(), accent);

                    ui.add_space(4.0);
                    ui.separator();
                    ui.label(RichText::new("  Janelas Abertas no Pila:").size(11.5).strong());
                    if wm_windows.is_empty() {
                        ui.label(
                            RichText::new("  Nenhuma janela aberta.")
                                .size(11.5)
                                .color(Color32::from_rgb(150, 158, 175)),
                        );
                    } else {
                        for w in wm_windows.iter().take(8) {
                            let title: String = if !w.title.is_empty() {
                                w.title.chars().take(28).collect()
                            } else {
                                w.app_id.clone()
                            };
                            let prefix = if focused_win.as_ref().map(|fw| fw.id == w.id).unwrap_or(false) {
                                "✓"
                            } else {
                                "•"
                            };
                            if macos_menu_row(
                                ui,
                                &format!("{} {}", prefix, title),
                                &format!("WS {}", w.workspace),
                                accent,
                            ) {
                                PilaConnector::focus_window(&w.id);
                                state.active_menu = ActiveGlobalMenu::None;
                            }
                        }
                    }
                }

                ActiveGlobalMenu::VolumePopup => {
                    render_volume_popup(ui, state, accent);
                }

                ActiveGlobalMenu::BrightnessPopup => {
                    render_brightness_popup(ui, state, accent);
                }

                ActiveGlobalMenu::BatteryPopup => {
                    render_battery_popup(ui, state, accent);
                }

                _ => {
                    // Render Context-Aware Application Menu Header + Dynamic Items
                    ui.label(
                        RichText::new(format!("  {}", app_header))
                            .size(11.5)
                            .strong()
                            .color(Color32::from_rgb(155, 165, 185)),
                    );
                    ui.separator();

                    for spec in dynamic_items {
                        if matches!(spec.action, MenuAction::Separator) {
                            ui.separator();
                            continue;
                        }
                        if macos_menu_row(ui, &spec.label, &spec.shortcut, accent) {
                            match spec.action {
                                MenuAction::Shortcut(chord) => {
                                    state.send_app_shortcut(chord);
                                }
                                MenuAction::Spawn(cmd) => {
                                    AppIndexer::launch(&cmd);
                                    state.active_menu = ActiveGlobalMenu::None;
                                }
                                MenuAction::WmAction(act) => {
                                    if let Some(ref w) = focused_win {
                                        PilaConnector::focus_window(&w.id);
                                    }
                                    PilaConnector::run_action(&act);
                                    state.active_menu = ActiveGlobalMenu::None;
                                }
                                MenuAction::FocusWindow(id) => {
                                    PilaConnector::focus_window(&id);
                                    state.active_menu = ActiveGlobalMenu::None;
                                }
                                MenuAction::CloseWindow(id) => {
                                    PilaConnector::close_window(&id);
                                    state.active_menu = ActiveGlobalMenu::None;
                                }
                                MenuAction::ToggleLauncher => {
                                    state.toggle_launcher();
                                }
                                MenuAction::ToggleControlCenter => {
                                    state.control_center_open = !state.control_center_open;
                                    state.active_menu = ActiveGlobalMenu::None;
                                }
                                MenuAction::ToggleDock => {
                                    state.config.dock.enabled = !state.config.dock.enabled;
                                    state.active_menu = ActiveGlobalMenu::None;
                                }
                                MenuAction::Separator => {}
                            }
                        }
                    }
                }
            }
        });

    if let Some(wr) = win_resp {
        state.active_menu_rect = Some(wr.response.rect);
    }
}

fn macos_menu_row(ui: &mut egui::Ui, label: &str, shortcut: &str, accent: Color32) -> bool {
    let desired = Vec2::new(ui.available_width(), 24.0);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());

    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), accent);
    }

    ui.painter().text(
        Pos2::new(rect.min.x + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.5),
        Color32::WHITE,
    );

    if !shortcut.is_empty() {
        ui.painter().text(
            Pos2::new(rect.max.x - 8.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            shortcut,
            egui::FontId::proportional(11.0),
            if resp.hovered() {
                Color32::from_rgba_unmultiplied(255, 255, 255, 230)
            } else {
                Color32::from_rgb(145, 152, 168)
            },
        );
    }

    resp.clicked()
}

fn tray_icon_pill(
    ui: &mut egui::Ui,
    icon: IconKind,
    text: &str,
    active: bool,
    accent: Color32,
    text_color: Color32,
) -> egui::Response {
    let icon_sz = 13.0;
    let text_w = if text.is_empty() {
        0.0
    } else {
        text.chars().count() as f32 * 6.8 + 5.0
    };
    let total_w = 10.0 + icon_sz + text_w + 4.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, 22.0), Sense::click());

    let bg = if active {
        accent
    } else if resp.hovered() {
        Color32::from_rgba_unmultiplied(255, 255, 255, 22)
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    }

    let icon_center = if text.is_empty() {
        rect.center()
    } else {
        Pos2::new(rect.min.x + 6.0 + icon_sz * 0.5, rect.center().y)
    };
    draw_icon(ui.painter(), icon_center, icon_sz, icon, text_color);

    if !text.is_empty() {
        ui.painter().text(
            Pos2::new(rect.min.x + 6.0 + icon_sz + 5.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            text,
            egui::FontId::proportional(11.8),
            text_color,
        );
    }

    resp
}

fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

fn weekday_pt(wd: Weekday) -> &'static str {
    match wd {
        Weekday::Mon => "Segunda-feira",
        Weekday::Tue => "Terça-feira",
        Weekday::Wed => "Quarta-feira",
        Weekday::Thu => "Quinta-feira",
        Weekday::Fri => "Sexta-feira",
        Weekday::Sat => "Sábado",
        Weekday::Sun => "Domingo",
    }
}

fn month_pt_lower(month: u32) -> &'static str {
    match month {
        1 => "janeiro",
        2 => "fevereiro",
        3 => "março",
        4 => "abril",
        5 => "maio",
        6 => "junho",
        7 => "julho",
        8 => "agosto",
        9 => "setembro",
        10 => "outubro",
        11 => "novembro",
        12 => "dezembro",
        _ => "",
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_y, next_m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_y, next_m, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(30)
}

fn render_month_calendar(
    ui: &mut egui::Ui,
    state: &mut ShellUiState,
    today: NaiveDate,
    accent: Color32,
) {
    let view_y = state.calendar_year;
    let view_m = state.calendar_month.clamp(1, 12);
    let is_current_month = view_y == today.year() && view_m == today.month();

    // Header: < | Mês Ano [Hoje] | >
    let header_w = ui.available_width();
    let (hdr_rect, _) = ui.allocate_exact_size(Vec2::new(header_w, 26.0), Sense::hover());
    let prev_rect = Rect::from_min_size(hdr_rect.min, Vec2::new(26.0, 26.0));
    let next_rect = Rect::from_min_size(
        Pos2::new(hdr_rect.max.x - 26.0, hdr_rect.min.y),
        Vec2::new(26.0, 26.0),
    );

    let prev_resp = ui.interact(prev_rect, ui.scope_id().with("cal_prev"), Sense::click());
    if prev_resp.hovered() {
        ui.painter().rect_filled(
            prev_rect,
            CornerRadius::same(6),
            Color32::from_rgba_unmultiplied(255, 255, 255, 26),
        );
    }
    draw_icon(
        ui.painter(),
        prev_rect.center(),
        11.0,
        IconKind::ChevronLeft,
        Color32::WHITE,
    );
    if prev_resp.clicked() {
        if state.calendar_month <= 1 {
            state.calendar_month = 12;
            state.calendar_year -= 1;
        } else {
            state.calendar_month -= 1;
        }
    }

    let next_resp = ui.interact(next_rect, ui.scope_id().with("cal_next"), Sense::click());
    if next_resp.hovered() {
        ui.painter().rect_filled(
            next_rect,
            CornerRadius::same(6),
            Color32::from_rgba_unmultiplied(255, 255, 255, 26),
        );
    }
    draw_icon(
        ui.painter(),
        next_rect.center(),
        11.0,
        IconKind::ChevronRight,
        Color32::WHITE,
    );
    if next_resp.clicked() {
        if state.calendar_month >= 12 {
            state.calendar_month = 1;
            state.calendar_year += 1;
        } else {
            state.calendar_month += 1;
        }
    }

    let title_text = format!("{} {}", capitalize_first(month_pt_lower(view_m)), view_y);
    let title_center_rect = Rect::from_min_max(
        Pos2::new(prev_rect.max.x + 4.0, hdr_rect.min.y),
        Pos2::new(next_rect.min.x - 4.0, hdr_rect.max.y),
    );
    let title_resp = ui.interact(title_center_rect, ui.scope_id().with("cal_today"), Sense::click());
    if !is_current_month && title_resp.hovered() {
        ui.painter().rect_filled(
            title_center_rect,
            CornerRadius::same(6),
            Color32::from_rgba_unmultiplied(255, 255, 255, 18),
        );
    }
    ui.painter().text(
        title_center_rect.center(),
        egui::Align2::CENTER_CENTER,
        if is_current_month {
            title_text
        } else {
            format!("{}  (Hoje)", title_text)
        },
        egui::FontId::proportional(12.5),
        Color32::WHITE,
    );
    if title_resp.clicked() && !is_current_month {
        state.calendar_year = today.year();
        state.calendar_month = today.month();
    }

    ui.add_space(4.0);

    // Weekday headers (Dom..Sáb)
    let col_w = header_w / 7.0;
    let (wd_rect, _) = ui.allocate_exact_size(Vec2::new(header_w, 18.0), Sense::hover());
    let wd_labels = ["D", "S", "T", "Q", "Q", "S", "S"];
    for (col, lbl) in wd_labels.iter().enumerate() {
        let cx = wd_rect.min.x + (col as f32 + 0.5) * col_w;
        ui.painter().text(
            Pos2::new(cx, wd_rect.center().y),
            egui::Align2::CENTER_CENTER,
            *lbl,
            egui::FontId::proportional(11.0),
            Color32::from_rgb(145, 152, 168),
        );
    }

    // Monthly 6x7 grid
    let first_of_month = NaiveDate::from_ymd_opt(view_y, view_m, 1).unwrap_or(today);
    let start_offset = first_of_month.weekday().num_days_from_sunday() as i32;
    let total_days = days_in_month(view_y, view_m) as i32;
    let (prev_y, prev_m) = if view_m == 1 {
        (view_y - 1, 12)
    } else {
        (view_y, view_m - 1)
    };
    let prev_month_days = days_in_month(prev_y, prev_m) as i32;

    let rows = if start_offset + total_days <= 35 { 5 } else { 6 };
    let row_h = 26.0;
    let (grid_rect, _) =
        ui.allocate_exact_size(Vec2::new(header_w, rows as f32 * row_h), Sense::hover());

    for row in 0..rows {
        for col in 0..7 {
            let cell_idx = row * 7 + col;
            let day_num = cell_idx - start_offset + 1;
            let (display_day, in_month, cell_date) = if day_num < 1 {
                let d = (prev_month_days + day_num) as u32;
                (d, false, NaiveDate::from_ymd_opt(prev_y, prev_m, d))
            } else if day_num > total_days {
                let d = (day_num - total_days) as u32;
                let (ny, nm) = if view_m == 12 {
                    (view_y + 1, 1)
                } else {
                    (view_y, view_m + 1)
                };
                (d, false, NaiveDate::from_ymd_opt(ny, nm, d))
            } else {
                let d = day_num as u32;
                (d, true, NaiveDate::from_ymd_opt(view_y, view_m, d))
            };

            let cell_center = Pos2::new(
                grid_rect.min.x + (col as f32 + 0.5) * col_w,
                grid_rect.min.y + (row as f32 + 0.5) * row_h,
            );
            let is_today = cell_date == Some(today);
            if is_today {
                ui.painter().circle_filled(cell_center, 11.5, accent);
            } else if in_month {
                let cell_rect = Rect::from_center_size(cell_center, Vec2::new(24.0, 24.0));
                let hov = ui.rect_contains_pointer(cell_rect);
                if hov {
                    ui.painter().circle_filled(
                        cell_center,
                        11.5,
                        Color32::from_rgba_unmultiplied(255, 255, 255, 22),
                    );
                }
            }

            let text_col = if is_today {
                Color32::WHITE
            } else if !in_month {
                Color32::from_rgba_unmultiplied(150, 158, 175, 105)
            } else if col == 0 || col == 6 {
                Color32::from_rgb(190, 198, 214)
            } else {
                Color32::WHITE
            };

            ui.painter().text(
                cell_center,
                egui::Align2::CENTER_CENTER,
                display_day.to_string(),
                egui::FontId::proportional(12.0),
                text_col,
            );
        }
    }
}

fn render_volume_popup(ui: &mut egui::Ui, state: &mut ShellUiState, accent: Color32) {
    let hw_snap = state.hw.state.lock().map(|g| g.clone()).unwrap_or_default();
    let card_bg = Color32::from_rgba_unmultiplied(255, 255, 255, 14);
    let card_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22));

    let vol_kind = if hw_snap.muted || hw_snap.volume_percent <= 0.5 {
        IconKind::VolumeMute
    } else if hw_snap.volume_percent < 45.0 {
        IconKind::VolumeLow
    } else {
        IconKind::VolumeHigh
    };

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("  Áudio & Volume")
                .size(12.5)
                .strong()
                .color(Color32::WHITE),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if hw_snap.volume_percent > 100.0 && !hw_snap.muted {
                ui.label(
                    RichText::new("Boost >100%")
                        .size(10.5)
                        .strong()
                        .color(Color32::from_rgb(255, 175, 50)),
                );
            } else {
                ui.label(
                    RichText::new(&hw_snap.audio_sink_name)
                        .size(10.5)
                        .color(Color32::from_rgb(150, 158, 176)),
                );
            }
        });
    });

    ui.add_space(6.0);

    let mut toggle_mute = false;
    if let Some(next_vol) = crate::ui::control_center::render_slider_card(
        ui,
        "tb_volume_slider",
        card_bg,
        card_stroke,
        vol_kind,
        "Alto-falante",
        hw_snap.volume_percent,
        0.0..=150.0,
        accent,
        hw_snap.muted,
        None,
        Some((
            if hw_snap.muted { "Mudo" } else { "Silenciar" },
            || {
                toggle_mute = true;
            },
        )),
    ) {
        state.hw.set_volume(next_vol);
    }
    if toggle_mute {
        state.hw.toggle_mute();
    }

    ui.add_space(6.0);

    // Quick volume presets including over-amplification up to 150%
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let subtle = Color32::from_rgba_unmultiplied(255, 255, 255, 18);
        for pct in [0u32, 50, 100, 125, 150] {
            let active = !hw_snap.muted && (hw_snap.volume_percent - pct as f32).abs() < 2.5;
            let bg = if active {
                if pct > 100 {
                    Color32::from_rgb(255, 159, 10)
                } else {
                    accent
                }
            } else {
                subtle
            };
            if crate::ui::control_center::pill_action_button(
                ui,
                &format!("{}%", pct),
                bg,
                Color32::WHITE,
            )
            .clicked()
            {
                state.hw.set_volume(pct as f32);
            }
        }
    });

    ui.add_space(6.0);

    let mic_kind = if hw_snap.mic_muted || hw_snap.mic_volume_percent <= 0.5 {
        IconKind::MicMute
    } else {
        IconKind::Mic
    };
    let mut toggle_mic = false;
    if let Some(next_mic) = crate::ui::control_center::render_slider_card(
        ui,
        "tb_mic_slider",
        card_bg,
        card_stroke,
        mic_kind,
        "Microfone",
        hw_snap.mic_volume_percent,
        0.0..=100.0,
        Color32::from_rgb(48, 209, 88),
        hw_snap.mic_muted,
        None,
        Some((
            if hw_snap.mic_muted { "Mudo" } else { "Silenciar" },
            || {
                toggle_mic = true;
            },
        )),
    ) {
        state.hw.set_mic_volume(next_mic);
    }
    if toggle_mic {
        state.hw.toggle_mic_mute();
    }
}

fn render_brightness_popup(ui: &mut egui::Ui, state: &mut ShellUiState, accent: Color32) {
    let hw_snap = state.hw.state.lock().map(|g| g.clone()).unwrap_or_default();
    let card_bg = Color32::from_rgba_unmultiplied(255, 255, 255, 14);
    let card_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22));

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("  Brilho da Tela")
                .size(12.5)
                .strong()
                .color(Color32::WHITE),
        );
        if !hw_snap.backlight_device.is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(&hw_snap.backlight_device)
                        .size(10.5)
                        .color(Color32::from_rgb(150, 158, 176)),
                );
            });
        }
    });

    ui.add_space(6.0);

    if let Some(next_br) = crate::ui::control_center::render_slider_card(
        ui,
        "tb_brightness_slider",
        card_bg,
        card_stroke,
        IconKind::Sun,
        "Luminosidade",
        hw_snap.brightness_percent,
        2.0..=100.0,
        accent,
        false,
        None,
        None::<(&str, fn())>,
    ) {
        state.hw.set_brightness(next_br);
    }

    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let subtle = Color32::from_rgba_unmultiplied(255, 255, 255, 18);
        for pct in [25u32, 50, 75, 100] {
            let active = (hw_snap.brightness_percent - pct as f32).abs() < 2.5;
            let bg = if active { accent } else { subtle };
            if crate::ui::control_center::pill_action_button(
                ui,
                &format!("{}%", pct),
                bg,
                Color32::WHITE,
            )
            .clicked()
            {
                state.hw.set_brightness(pct as f32);
            }
        }
    });
}

fn render_battery_popup(ui: &mut egui::Ui, state: &mut ShellUiState, accent: Color32) {
    let sys_snap = state
        .sys
        .snapshot
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let hw_snap = state.hw.state.lock().map(|g| g.clone()).unwrap_or_default();
    let bat_pct = sys_snap.battery_percent.unwrap_or(0);
    let det = &sys_snap.battery_details;

    let status_pt = match det.status.as_str() {
        "Charging" => "Carregando",
        "Discharging" => "Descarregando",
        "Full" => "Carga Completa",
        "Not charging" => "Conectada (Sem carga)",
        other if !other.is_empty() => other,
        _ if sys_snap.battery_charging => "Carregando",
        _ => "Desconhecido",
    };

    let status_col = if det.status.eq_ignore_ascii_case("Charging") {
        Color32::from_rgb(48, 209, 88)
    } else if det.status.eq_ignore_ascii_case("Full") {
        accent
    } else if bat_pct <= 20 {
        Color32::from_rgb(255, 69, 58)
    } else {
        Color32::from_rgb(255, 159, 10)
    };

    let card_bg = Color32::from_rgba_unmultiplied(255, 255, 255, 14);
    let card_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22));

    // Header Card: Percentage + Status + Charge bar
    Frame::NONE
        .fill(card_bg)
        .corner_radius(CornerRadius::same(11))
        .inner_margin(Margin::same(10))
        .stroke(card_stroke)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (icon_r, _) = ui.allocate_exact_size(Vec2::new(22.0, 22.0), Sense::hover());
                draw_icon(
                    ui.painter(),
                    icon_r.center(),
                    16.0,
                    IconKind::Battery(bat_pct, sys_snap.battery_charging),
                    status_col,
                );
                ui.label(
                    RichText::new(format!("{}%", bat_pct))
                        .size(20.0)
                        .strong()
                        .color(Color32::WHITE),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    Frame::NONE
                        .fill(Color32::from_rgba_unmultiplied(
                            status_col.r(),
                            status_col.g(),
                            status_col.b(),
                            46,
                        ))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(8, 3))
                        .show(ui, |ui| {
                            ui.label(RichText::new(status_pt).size(11.0).strong().color(status_col));
                        });
                });
            });

            ui.add_space(6.0);
            let (bar_rect, _) = ui.allocate_exact_size(
                Vec2::new(ui.available_width().max(80.0), 7.0),
                Sense::hover(),
            );
            ui.painter().rect_filled(
                bar_rect,
                CornerRadius::same(4),
                Color32::from_rgba_unmultiplied(255, 255, 255, 20),
            );
            let frac = (bat_pct as f32 / 100.0).clamp(0.0, 1.0);
            if frac > 0.0 {
                let fill_rect = Rect::from_min_size(
                    bar_rect.min,
                    Vec2::new((bar_rect.width() * frac).max(6.0), bar_rect.height()),
                );
                ui.painter()
                    .rect_filled(fill_rect, CornerRadius::same(4), status_col);
            }
        });

    ui.add_space(6.0);

    // Electrical & Discharge Telemetry Card
    Frame::NONE
        .fill(card_bg)
        .corner_radius(CornerRadius::same(11))
        .inner_margin(Margin::same(10))
        .stroke(card_stroke)
        .show(ui, |ui| {
            let power_txt = det
                .power_w
                .map(|w| {
                    if det.status.eq_ignore_ascii_case("Charging") {
                        format!("+{:.1} W (carregando)", w)
                    } else if det.status.eq_ignore_ascii_case("Discharging") {
                        format!("-{:.1} W (descarregando)", w)
                    } else {
                        format!("{:.1} W", w)
                    }
                })
                .unwrap_or_else(|| "N/D".to_string());
            battery_info_row(ui, "Potência / Consumo", &power_txt, status_col);

            let time_txt = det
                .time_remaining_mins
                .map(|m| {
                    let h = m / 60;
                    let rem = m % 60;
                    if det.status.eq_ignore_ascii_case("Charging") {
                        if h > 0 {
                            format!("{}h {:02}min p/ 100%", h, rem)
                        } else {
                            format!("{} min p/ 100%", rem)
                        }
                    } else if h > 0 {
                        format!("{}h {:02}min restantes", h, rem)
                    } else {
                        format!("{} min restantes", rem)
                    }
                })
                .unwrap_or_else(|| {
                    if det.status.eq_ignore_ascii_case("Full") {
                        "Bateria cheia".to_string()
                    } else {
                        "Calculando…".to_string()
                    }
                });
            battery_info_row(ui, "Tempo estimado", &time_txt, Color32::WHITE);

            let ac_txt = match det.ac_online {
                Some(true) => "Conectado na tomada",
                Some(false) => "Na bateria",
                None => "N/D",
            };
            battery_info_row(ui, "Fonte de energia", ac_txt, Color32::WHITE);

            if let Some(v) = det.voltage_v {
                battery_info_row(ui, "Tensão atual", &format!("{:.2} V", v), Color32::WHITE);
            }

            if let (Some(now_wh), Some(full_wh)) = (det.energy_now_wh, det.energy_full_wh) {
                battery_info_row(
                    ui,
                    "Capacidade atual",
                    &format!("{:.1} / {:.1} Wh", now_wh, full_wh),
                    Color32::WHITE,
                );
            }

            if let Some(health) = det.health_percent {
                let cycles_suffix = det
                    .cycle_count
                    .map(|c| format!(" ({} ciclos)", c))
                    .unwrap_or_default();
                let health_col = if health < 65.0 {
                    Color32::from_rgb(255, 159, 10)
                } else {
                    Color32::from_rgb(48, 209, 88)
                };
                battery_info_row(
                    ui,
                    "Saúde da bateria",
                    &format!("{:.0}%{}", health, cycles_suffix),
                    health_col,
                );
            }
        });

    ui.add_space(6.0);

    // Performance & Power Profile Card
    Frame::NONE
        .fill(card_bg)
        .corner_radius(CornerRadius::same(11))
        .inner_margin(Margin::same(10))
        .stroke(card_stroke)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("Perfil de Performance")
                            .size(11.5)
                            .strong()
                            .color(Color32::WHITE),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{} • CPU {:.0}% ({:.0}°C)",
                            hw_snap.power_profile_name, sys_snap.cpu_usage, sys_snap.cpu_temp_c
                        ))
                        .size(10.5)
                        .color(Color32::from_rgb(160, 170, 188)),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let btn_bg = if hw_snap.power_saver {
                        Color32::from_rgb(243, 156, 18)
                    } else {
                        accent
                    };
                    let btn_label = if hw_snap.power_saver {
                        "Economia"
                    } else {
                        "Alternar"
                    };
                    if crate::ui::control_center::pill_action_button(
                        ui,
                        btn_label,
                        btn_bg,
                        Color32::WHITE,
                    )
                    .clicked()
                    {
                        state.hw.toggle_power_saver();
                    }
                });
            });
        });
}

fn battery_info_row(ui: &mut egui::Ui, label: &str, value: &str, value_col: Color32) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .size(11.5)
                .color(Color32::from_rgb(162, 172, 190)),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(value).size(11.5).strong().color(value_col));
        });
    });
}
