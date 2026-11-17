# 🚀 RocketDE — macOS-Style Desktop Shell para Wayland (Pila WM)

**RocketDE** é um ambiente de barra superior (*Menu Bar*), *Dock* inferior centralizada, *Control Center* e *Spotlight App Launcher* desenvolvido em **Rust** com **`egui` + `wgpu` + `smithay-client-toolkit` (`wlr-layer-shell`)**, projetado sob medida para máxima performance, baixíssimo uso de memória RAM (~16 MB de heap privado) e integração nativa com o Tiling Window Manager **Pila** no Wayland.

---

## ✨ Funcionalidades Implementadas

### 1. 🍎 Barra Superior Estilo macOS (`Top Bar`)
Ancorada no topo da tela via protocolo `zwlr_layer_shell_v1` com reserva de espaço (`exclusive_zone = 30px`) e suporte a *blur* nativo do compositor:
- **Esquerda (Identidade e Menu Global)**:
  - **Botão Rocket (`🚀`)**: Menu principal estilo Apple com acesso rápido às configurações, recarregamento de tema, comando para mostrar o desktop, desligar monitores ou encerrar a sessão do Pila.
  - **Nome do Aplicativo Focado em Negrito**: Detectado em tempo real via IPC do Pila (`pila msg --json focused` / `windows`).
  - **Menus Globais Reais (`Arquivo`, `Editar`, `Visualizar`, `Janela`, `Ajuda`)**:
    - Posicionados **colados na barra superior** (`y = 2px`) e alinhados exatamente à posição horizontal (`x`) do botão clicado.
    - Suporte a **troca automática no hover** (assim como no macOS: quando um menu está aberto, basta mover o cursor sobre os itens vizinhos para alternar o menu instantaneamente).
    - **Ações Reais na Janela Focada**: Utiliza o protocolo Wayland `zwp_virtual_keyboard_v1` (com keymap XKB compilado via `xkbcommon`) para enviar atalhos reais diretamente para o aplicativo ativo (`Ctrl+T`, `Ctrl+N`, `Ctrl+O`, `Ctrl+S`, `Ctrl+Shift+S`, `Ctrl+P`, `Ctrl+W`, `Ctrl+Q`, `Ctrl+Z`, `Ctrl+Shift+Z`, `Ctrl+X`, `Ctrl+C`, `Ctrl+V`, `Ctrl+A`, `Ctrl+F`, `Ctrl+H`, `Ctrl++`, `Ctrl+-`, `Ctrl+0`, `F11`, `F12`, `F1`), além de comandos diretos de gerenciamento de janelas no Pila (`maximize`, `minimize`, `restore`, `center`, `snap-left`, `snap-right`, `toggle-always-on-top`, `toggle-all-workspaces`, `move-to-workspace`).
- **Centro (Relógio & Calendário Geométrico)**:
  - **Data e Hora Centralizadas**: Renderizadas em uma região geometricamente fixada no centro exato do monitor, garantindo que a pílula do relógio nunca saia do centro independentemente do tamanho dos menus na esquerda ou dos indicadores na direita.
  - Clicar na data/hora abre o **Popup de Calendário e Janelas Abertas**, permitindo focar qualquer janela ativa com um clique.
- **Direita (Indicadores de Hardware e Sistema em Tempo Real)**:
  - **Teclado (`⌨`)**: Layout ativo reportado pelo compositor.
  - **Telemetria (`CPU %` & `RAM GB`)**: Leitura leve via `sysinfo`.
  - **Mídia MPRIS (`🎵`)**: Conectado ao barramento D-Bus (`org.mpris.MediaPlayer2.Player`) para exibir a música atual, Play/Pause no clique esquerdo e Próxima Faixa no clique direito.
  - **Rede (`📶`)**: Exibe o SSID real da rede Wi-Fi conectada (`nmcli`) ou a interface de rede ativa.
  - **Volume (`🔊`)**: Conectado ao PipeWire/PulseAudio (`pactl`). **Clique** alterna Mute/Unmute e **rolar o scroll do mouse** em cima do ícone altera o volume em tempo real!
  - **Brilho (`☀`)**: Conectado ao `/sys/class/backlight/amdgpu_bl1` e `systemd-logind` (`SetBrightness`). **Rolar o scroll do mouse** em cima do ícone altera o brilho físico da tela na hora!
  - **Bateria (`🔋` / `⚡`)**: Leitura direta de `/sys/class/power_supply/BAT0`.
  - **Spotlight (`🔍`)** e **Control Center (`🎛`)**.

---

### 2. 🎛 Control Center (Painel Rápido Superior Direito)
Colado logo abaixo da barra superior no canto direito, com fechamento automático ao clicar fora:
- **Toggles Rápidos Reais (Grid 2x2)**:
  - **Wi-Fi**: Liga/desliga o rádio Wi-Fi via `nmcli` / `rfkill`.
  - **Bluetooth**: Liga/desliga o adaptador Bluetooth via `rfkill` e BlueZ D-Bus (`org.bluez.Adapter1.Powered`).
  - **Foco / DND**: Alterna modo silencioso (`makoctl` / `dunstctl`).
  - **Energia**: Lê o perfil de frequência da CPU (`schedutil` / `powersave`) e alterna via `net.hadess.PowerProfiles`.
- **Sliders Suaves em Tempo Real**:
  - **Brilho da Tela (`2%` a `100%`)**: Controla o backlight real via D-Bus `org.freedesktop.login1.Session.SetBrightness` sem exigir `sudo`.
  - **Som / Volume (`0%` a `100%`)**: Controla o sink padrão via `pactl` com botão rápido de Mute/Unmute.
- **Card de Reprodução de Mídia (MPRIS)**: Botões Anterior (`⏮`), Play/Pause (`▶`/`⏸`) e Próxima (`⏭`).
- **Resumo Térmico e Memória**: Uso de CPU, temperatura em `°C` dos sensores e consumo de RAM.

---

### 3. 🛸 Dock Inferior Estilo macOS (`Bottom Dock`)
Centralizada na parte inferior da tela com visual *glassmorphism* e linha de reflexo superior:
- **Região de Input Inteligente (`wl_region`)**: Toda a área da tela ao redor da Dock deixa os cliques passarem diretamente para as janelas abaixo; apenas o retângulo exato da pílula da Dock intercepta o mouse.
- **Efeito de Magnificação (Zoom Suave no Hover)**: Interpolação trigonométrica suave nos ícones próximos ao cursor com elevação vertical (*lift*).
- **Apps Favoritos + Janelas Abertas**: Mostra um ponto luminoso azul/branco abaixo dos aplicativos em execução. Clicar foca a janela existente no Pila (`pila msg focus <id>`) ou inicia o aplicativo; clique direito abre uma nova instância.

---

### 4. 🔍 Menu de Apps / Spotlight Launcher (`Super + Space`)
- **Ativação Instantânea (`< 1ms`)**:
  - Atalho global `Super + Space` ou `Alt + Space` (tanto via leitor `evdev` embutido quanto via keybinding do Pila `rocketde --toggle-launcher` por Unix Socket em `$XDG_RUNTIME_DIR/rocketde.sock`).
- **Busca Fuzzy Ultrarrápida (`SkimMatcherV2`)**: Indexa todos os arquivos `.desktop` do sistema (`/usr/share/applications`, `~/.local/share/applications`, Flatpak).
- **Calculadora Integrada**: Digite expressões matemáticas (ex: `= 144 * 12` ou `250 / 4`) para ver o resultado em destaque.
- **Executor Shell**: Digite `> comando` (ex: `> htop`) e pressione `Enter` para rodar comandos diretamente.

---

## ⚡ Desempenho e Consumo de Recursos

O `Cargo.toml` é configurado com otimizações máximas de compilação:
- `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = "symbols"`, `overflow-checks = false`.
- **Consumo de Memória RAM (Release)**:
  - **Heap Privado (`RssAnon`)**: ~**16,2 MB**
  - **PSS (`Proportional Set Size`)**: ~**65,3 MB**

---

## 🛠 Comandos de Linha de Comando (CLI & IPC)

```bash
# Inicia o daemon (ou alterna o Launcher se já estiver rodando)
rocketde

# Abre/fecha o Spotlight App Launcher instantaneamente via Unix Socket
rocketde --toggle-launcher    # ou: rocketde -l

# Abre/fecha o Control Center
rocketde --toggle-control-center  # ou: rocketde -c

# Mostra/oculta a Dock inferior
rocketde --toggle-dock            # ou: rocketde -d

# Controles rápidos de hardware via IPC (usados pelas teclas multimídia do WM)
rocketde --volume-up
rocketde --volume-down
rocketde --mute
rocketde --brightness-up
rocketde --brightness-down

# Recarrega ~/.config/rocketde/config.toml (também possui hot-reload automático ao salvar!)
rocketde --reload

# Encerra o daemon
rocketde --quit
```

---

## 🎯 O que falta para ficar 100% Perfeito (Roadmap de Evolução)

Para transformar o **RocketDE** em um ambiente desktop completo de nível comercial, os próximos passos recomendados são:

1. **Renderização de Ícones Reais `.svg` e `.png` dos Temas XDG (`hicolor` / `Papirus`)**:
   - *Atualmente*: A Dock e o Launcher usam ícones simbólicos/vetoriais categorizados em alta nitidez.
   - *Para ficar perfeito*: Carregar as texturas reais dos arquivos `.svg`/`.png` dos aplicativos instalados em `/usr/share/icons` diretamente como texturas GPU no `egui-wgpu`.
2. **Protocolo DBus `com.canonical.dbusmenu` / `org.gtk.Menus` (AppMenu Nativo)**:
   - *Atualmente*: Os menus `Arquivo`, `Editar`, `Visualizar`, `Janela` e `Ajuda` enviam os atalhos universais reais (`zwp_virtual_keyboard_v1`) para a janela ativa e controlam o WM.
   - *Para ficar perfeito*: Implementar um servidor DBus `Registrar` (`com.canonical.AppMenu.Registrar`) para que aplicativos GTK/Qt/Electron exportem sua árvore de menus interna completa (incluindo submenus específicos de cada programa) diretamente para o `RocketDE`.
3. **Protocolo `StatusNotifierWatcher` (System Tray de Ícones de Apps em Background)**:
   - Permitir que apps em segundo plano (como Discord, Signal, Steam, NM-Applet) registrem seus próprios ícones clicáveis e menus de contexto na direita da barra via D-Bus (`org.kde.StatusNotifierWatcher`).
4. **Servidor de Notificações (`org.freedesktop.Notifications`) Integrado (OSD & Toasts)**:
   - Substituir a necessidade do `mako`/`dunst` exibindo *toasts* flutuantes estilo macOS no canto superior direito e um OSD animado quando o usuário aperta as teclas de volume ou brilho.
5. **Auto-Hide Inteligente da Dock (*Dodge Windows*)**:
   - Ocultar suavemente a Dock inferior quando uma janela do Pila se aproximar ou estiver maximizada naquele monitor, reaparecendo ao encostar o mouse na borda inferior da tela.
