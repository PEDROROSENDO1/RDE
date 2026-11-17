# RocketDE — Próximas Melhorias & Recomendações de Arquitetura

## 1. Arquitetura: Manter `rocketde` separado do `../pila/`

**Recomendação:** manter o `rocketde` como cliente Wayland independente (`zwlr_layer_shell_v1` + `ext_background_effect_v1` + `pila msg`), **sem** embutir a UI dentro do código-fonte do compositor `pila`.

- **Isolamento de falhas:** qualquer erro em parsing, decodificação de ícones (`resvg`/`image`), `egui` ou `wgpu` não derruba o compositor nem fecha as janelas da sessão.
- **Stack gráfico independente:** o `pila` usa Smithay GLES2 (`MultiRenderer`) direto no DRM/KMS, enquanto o `rocketde` usa `egui` + `wgpu` (Vulkan).
- **Manutenção limpa:** evita conflitos ao atualizar o repositório `../pila/` via `git pull`.
- **Custo baixo:** o `rocketde` consome ~51–73 MB de PSS em execução.

### Melhoria opcional futura no IPC do `../pila/`
- Hoje o `rocketde` move uma janela arrastada usando `pila msg focus <id>` seguido de `pila msg action move-to-workspace <ws>`.
- Para janelas do workspace atual isso não causa efeito colateral; apenas ao mover uma janela de *outro* workspace ocorre uma troca rápida de foco antes de mover.
- **Melhoria futura (opcional):** adicionar ao `pila msg` um comando direto por ID (`move-window-to-workspace <id> <ws>`) para mover qualquer janela sem alterar o foco atual.

---

## 2. Visão Multitarefa Estilo GNOME (Live Thumbnails + Gestos)

O compositor `pila` já expõe os protocolos necessários em `src/capture.rs` e `src/gestures.rs`:

1. **Miniaturas ao vivo das janelas (`ext-image-copy-capture-v1` + `zwlr-foreign-toplevel-manager-v1`):**
   - Criar sessões de captura via `wl_shm` **apenas enquanto a Visão Geral estiver aberta**, usando o *damage tracking* nativo do Pila.
   - Fazer *downscale* para texturas leves (ex.: máx. `420×260`) e destruir todas as sessões + chamar `libc::malloc_trim(0)` ao fechar a Visão Geral.
2. **Layout e Animações GNOME Activities:**
   - Posicionamento espacial proporcional das janelas no workspace selecionado, drag-and-drop contínuo para qualquer workspace no topo sem fechar a Visão Geral, e animação horizontal (*slide*) ao alternar workspaces.
3. **Gestos de 3 dedos no Touchpad (padrão natural GNOME via `[gestures]` do Pila):**
   - `swipe-3-left` → próximo workspace (com animação `workspace_slide`)
   - `swipe-3-right` → workspace anterior
   - `swipe-3-up` → `spawn rocketde --toggle-overview`
   - `swipe-3-down` → `spawn rocketde --close-launcher`
