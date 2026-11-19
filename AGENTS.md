# AGENTS.md — RocketDE (RDE)

Guia oficial de arquitetura e boas práticas para qualquer alteração neste repositório.
O objetivo principal é manter o projeto **modular, leve, sem duplicação de código e sem overengineering**.

---

## 1. Estrutura do Cargo Workspace

O repositório usa um **Cargo Workspace** (`Cargo.toml` na raiz, `edition = "2024"`, `resolver = "3"`):

```text
RDE/
├── Cargo.toml                  # [workspace], [workspace.dependencies] e [profile.*]
├── crates/
│   ├── rocketde-core/          # Lógica e dados compartilhados (SEM dependência de egui/Wayland)
│   │   └── src/
│   │       ├── config.rs       # RocketConfig (~/.config/rocketde/config.toml)
│   │       ├── design.rs       # ThemeConfig / Design Tokens (~/.rocket/theme/theme.ron)
│   │       ├── hardware.rs     # HardwareController (áudio/brilho)
│   │       └── ipc.rs          # IpcCommand e comunicação Unix socket
│   └── rocketde-ui-common/     # Camada visual compartilhada (egui + rocketde-core)
│       └── src/
│           ├── theme.rs        # apply_design() / apply_macos_theme() e fontes do sistema
│           ├── components.rs   # Helpers e frames reutilizáveis (cards, títulos, cores)
│           └── icons.rs        # IconKind e draw_icon()
└── apps/
    ├── rocketde/               # Shell principal (top_bar, dock, launcher, overview, control_center, wm/pila, wayland/*)
    ├── rocketde-settings/      # Aplicativo de Configurações
    └── rocketde-files/         # Gerenciador de Arquivos
```

---

## 2. Regras de Organização (Anti-Duplicação)

1. **Não duplique lógica compartilhada em `apps/*`:**
   * Config/IPC/Hardware → `crates/rocketde-core`
   * Tema, fontes, ícones, cores e widgets reutilizáveis → `crates/rocketde-ui-common`
   * Antes de criar uma função de UI, ícone ou helper de estilo em `rocketde`, `rocketde-settings` ou `rocketde-files`, verifique se já existe (ou deve morar) em `rocketde-ui-common`.
2. **Design e Tema exclusivamente em `.ron`:**
   * Todo token visual (cores, superfícies, raios de borda, espaçamentos, tipografia) deve ficar em `ThemeConfig` (`crates/rocketde-core/src/design.rs`), salvo em **`~/.rocket/theme/theme.ron`**.
   * **Não** coloque configurações visuais/de tema no `config.toml` (`RocketConfig.theme` usa `#[serde(skip)]`).
   * Toda nova struct/campo de design ou config deve usar `#[serde(default)]` para manter compatibilidade com arquivos existentes.
3. **Dependências isoladas por crate:**
   * Declare novas dependências primeiro em `[workspace.dependencies]` no `Cargo.toml` da raiz.
   * Adicione `crate.workspace = true` **apenas** no `Cargo.toml` dos membros que realmente usam aquela dependência. Não polua `rocketde-core`, `rocketde-settings` ou `rocketde-files` com crates exclusivos do shell (`sysinfo`, `wayland-protocols-wlr`, `calloop`, etc.).

---

## 3. Regras de Implementação (Anti-Overengineering)

* **Simplicidade primeiro:** resolva o problema de forma direta. Não crie traits genéricos, fábricas, macros ou camadas extras de abstração para algo que tem apenas um ou dois usos concretos.
* **Respeite os limites dos crates:**
  * `rocketde-core` não deve depender de `egui` nem de `wayland-client`.
  * `rocketde-ui-common` não deve depender de lógica de WM (`pila`), `layer_shell` ou regras de negócio específicas de um único app.
* **Falhas seguras em I/O e parsing:** ao ler `.toml` ou `.ron` do usuário, nunca dê `unwrap()`/`panic!`; logue o erro em `stderr` e faça fallback para `Default`.
* **Consistência visual:** use `rocketde_ui_common::components` e `ThemeConfig` (`theme.ron`) em vez de espalhar cores e tamanhos hardcoded pelos apps.

---

## 4. Build, Testes e Execução

```bash
# Verificação rápida e testes de todo o workspace
cargo check --workspace
cargo test --workspace

# Build de produção (binários em target/release/)
cargo build --release --workspace

# Build isolado de um app específico
cargo build -p rocketde-files
cargo build -p rocketde-settings
```

* O binário principal em produção é `/home/pedro/Projects/RDE/target/release/rocketde`, iniciado pelo compositor `pila` via `~/.config/pila/config.toml`.
