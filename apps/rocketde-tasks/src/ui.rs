use crate::config::TasksConfig;
use crate::systemd::{ServiceAction, ServiceScope, SystemdService};
use crate::telemetry::{MemoryStats, ProcessInfo, SystemTelemetry};
use egui::{
    Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, Pos2, Rect, RichText, Sense,
    Stroke, Ui, Vec2, pos2,
};
use rocketde_ui_common::icons::IconKind;
use std::collections::{HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TasksPage {
    Overview,
    Apps,
    Processes,
    Compute,
    Memory,
    StorageNet,
    Services,
    Settings,
}

impl TasksPage {
    pub const ALL: [Self; 8] = [
        Self::Overview,
        Self::Apps,
        Self::Processes,
        Self::Compute,
        Self::Memory,
        Self::StorageNet,
        Self::Services,
        Self::Settings,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => "Visão Geral",
            Self::Apps => "Apps Abertos",
            Self::Processes => "Processos",
            Self::Compute => "CPU & GPU",
            Self::Memory => "Memória & ZRAM",
            Self::StorageNet => "Discos & Rede",
            Self::Services => "Serviços & Boot",
            Self::Settings => "Configurações",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Self::Overview => "Resumo em tempo real de CPU, RAM real vs cache, GPU e apps",
            Self::Apps => "Processos agrupados por aplicativo aberto com soma isolada de RAM",
            Self::Processes => "Lista completa de processos com ordenação, detalhes e sinais",
            Self::Compute => "Núcleos da CPU, frequências, temperatura e telemetria de GPU",
            Self::Memory => "Uso real, Cache Inteligente reaproveitável, Swap e compressão ZRAM",
            Self::StorageNet => "Partições montadas, leitura/escrita de discos e interfaces de rede",
            Self::Services => "Unidades systemd do sistema e usuário, inicialização no boot e logs",
            Self::Settings => "Intervalo de atualização, filtros, unidades e segurança",
        }
    }

    pub fn icon(self) -> IconKind {
        match self {
            Self::Overview => IconKind::Activity,
            Self::Apps => IconKind::Layers,
            Self::Processes => IconKind::Terminal,
            Self::Compute => IconKind::Cpu,
            Self::Memory => IconKind::Memory,
            Self::StorageNet => IconKind::HardDrive,
            Self::Services => IconKind::Shield,
            Self::Settings => IconKind::Settings,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortCol {
    Cpu,
    Ram,
    Pid,
    Name,
    Io,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceFilter {
    All,
    BootEnabled,
    Active,
    Failed,
}

pub enum UiAction {
    KillPid { pid: u32, signal: i32, label: String },
    KillGroup { pids: Vec<u32>, signal: i32, app_name: String },
    ServiceOp { unit: String, scope: ServiceScope, action: ServiceAction },
    FetchLogs { unit: String, scope: ServiceScope },
    RefreshServices,
    SaveConfig,
    ClearSudoCache,
}

pub fn format_bytes(bytes: u64, iec: bool) -> String {
    let base: f64 = if iec { 1024.0 } else { 1000.0 };
    let units = if iec {
        ["B", "KiB", "MiB", "GiB", "TiB"]
    } else {
        ["B", "KB", "MB", "GB", "TB"]
    };
    let mut val = bytes as f64;
    let mut idx = 0;
    while val >= base && idx < units.len() - 1 {
        val /= base;
        idx += 1;
    }
    if idx == 0 {
        format!("{bytes} B")
    } else if val >= 100.0 {
        format!("{val:.0} {}", units[idx])
    } else if val >= 10.0 {
        format!("{val:.1} {}", units[idx])
    } else {
        format!("{val:.2} {}", units[idx])
    }
}

pub fn format_rate(bps: u64, iec: bool) -> String {
    format!("{}/s", format_bytes(bps, iec))
}

pub fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours:02}h {mins:02}m")
    } else {
        format!("{hours:02}h {mins:02}m")
    }
}

pub fn pill_btn(ui: &mut Ui, label: &str, col: Color32) -> egui::Response {
    let btn = egui::Button::new(
        RichText::new(label)
            .size(12.0)
            .strong()
            .color(Color32::WHITE),
    )
    .fill(col)
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same(7));
    ui.add(btn)
}

pub fn subtle_btn(ui: &mut Ui, label: &str) -> egui::Response {
    let btn = egui::Button::new(
        RichText::new(label)
            .size(12.0)
            .color(Color32::from_rgb(220, 226, 238)),
    )
    .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 14))
    .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22)))
    .corner_radius(CornerRadius::same(7));
    ui.add(btn)
}

fn card(ui: &mut Ui, card_bg: Color32, card_stroke: Stroke, add_contents: impl FnOnce(&mut Ui)) {
    Frame::NONE
        .fill(card_bg)
        .stroke(card_stroke)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, add_contents);
}

fn draw_progress_bar(ui: &mut Ui, frac: f32, fill_col: Color32, height: f32) {
    let w = ui.available_width().max(40.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, height), Sense::hover());
    let p = ui.painter();
    p.rect_filled(
        rect,
        CornerRadius::same((height * 0.5) as u8),
        Color32::from_rgba_unmultiplied(255, 255, 255, 16),
    );
    let f = frac.clamp(0.0, 1.0);
    if f > 0.005 {
        let fill_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width() * f, rect.height()));
        p.rect_filled(fill_rect, CornerRadius::same((height * 0.5) as u8), fill_col);
    }
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else if max_chars > 1 {
        let mut out: String = s.chars().take(max_chars - 1).collect();
        out.push('…');
        out
    } else {
        "…".to_string()
    }
}

fn state_label_and_color(state: char) -> (&'static str, Color32) {
    match state {
        'R' => ("Ativo", Color32::from_rgb(48, 209, 88)),
        'S' | 'I' => ("Dormindo", Color32::from_rgb(130, 145, 170)),
        'D' => ("I/O", Color32::from_rgb(72, 199, 234)),
        'T' | 't' => ("Pausado", Color32::from_rgb(255, 159, 10)),
        'Z' => ("Zumbi", Color32::from_rgb(255, 69, 58)),
        _ => ("Outro", Color32::from_rgb(130, 140, 160)),
    }
}

fn draw_sparkline(
    ui: &mut Ui,
    values: &VecDeque<f32>,
    max_val: f32,
    color: Color32,
    height: f32,
) {
    let w = ui.available_width().max(80.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, height), Sense::hover());
    let p = ui.painter();
    p.rect_filled(
        rect,
        CornerRadius::same(8),
        Color32::from_rgba_unmultiplied(255, 255, 255, 7),
    );
    p.rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 14)),
        egui::StrokeKind::Inside,
    );

    // Subtle horizontal grid lines
    for frac in [0.25, 0.5, 0.75] {
        let y = rect.max.y - rect.height() * frac;
        p.line_segment(
            [pos2(rect.min.x + 2.0, y), pos2(rect.max.x - 2.0, y)],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 10)),
        );
    }

    if values.len() < 2 {
        return;
    }

    let max_v = max_val.max(0.001);
    let peak = values.iter().copied().fold(0.0f32, f32::max);
    let pad = 6.0;
    let inner_w = (rect.width() - pad * 2.0).max(1.0);
    let inner_h = (rect.height() - pad * 2.0).max(1.0);
    let n = values.len();

    let mut pts = Vec::with_capacity(n);
    for (i, &v) in values.iter().enumerate() {
        let x = rect.min.x + pad + (i as f32 / (n - 1) as f32) * inner_w;
        let norm = (v / max_v).clamp(0.0, 1.0);
        let y = rect.max.y - pad - norm * inner_h;
        pts.push(pos2(x, y));
    }

    // Area fill
    let fill_col = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 36);
    let base_y = rect.max.y - pad;
    for pair in pts.windows(2) {
        let a = pair[0];
        let b = pair[1];
        let poly = vec![a, b, pos2(b.x, base_y), pos2(a.x, base_y)];
        p.add(egui::Shape::convex_polygon(poly, fill_col, Stroke::NONE));
    }

    // Line stroke
    for pair in pts.windows(2) {
        p.line_segment([pair[0], pair[1]], Stroke::new(1.9, color));
    }

    // Glowing terminal dot at latest sample
    if let Some(&last) = pts.last() {
        p.circle_filled(
            last,
            5.5,
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 60),
        );
        p.circle_filled(last, 2.8, Color32::WHITE);
    }

    // Subtle peak label
    p.text(
        pos2(rect.min.x + 8.0, rect.min.y + 6.0),
        egui::Align2::LEFT_TOP,
        format!("Pico: {peak:.1}%"),
        FontId::proportional(10.0),
        Color32::from_rgb(145, 155, 175),
    );
}

fn draw_dual_sparkline(
    ui: &mut Ui,
    a: &VecDeque<f32>,
    b: &VecDeque<f32>,
    col_a: Color32,
    col_b: Color32,
    height: f32,
) {
    let max_v = a
        .iter()
        .chain(b.iter())
        .copied()
        .fold(1.0f32, f32::max)
        .max(1.0);
    let w = ui.available_width().max(80.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, height), Sense::hover());
    let p = ui.painter();
    p.rect_filled(
        rect,
        CornerRadius::same(8),
        Color32::from_rgba_unmultiplied(255, 255, 255, 7),
    );
    p.rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 14)),
        egui::StrokeKind::Inside,
    );

    let pad = 4.0;
    let inner_w = (rect.width() - pad * 2.0).max(1.0);
    let inner_h = (rect.height() - pad * 2.0).max(1.0);

    for (series, col) in [(a, col_a), (b, col_b)] {
        if series.len() < 2 {
            continue;
        }
        let n = series.len();
        let pts: Vec<Pos2> = series
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let x = rect.min.x + pad + (i as f32 / (n - 1) as f32) * inner_w;
                let y = rect.max.y - pad - (v / max_v).clamp(0.0, 1.0) * inner_h;
                pos2(x, y)
            })
            .collect();
        for pair in pts.windows(2) {
            p.line_segment([pair[0], pair[1]], Stroke::new(1.6, col));
        }
    }
}

pub fn draw_memory_stacked_bar(
    ui: &mut Ui,
    mem: &MemoryStats,
    accent: Color32,
    cache_col: Color32,
    iec: bool,
) {
    let total = mem.total_bytes.max(1) as f64;
    let real_frac = (mem.real_used_bytes as f64 / total).clamp(0.0, 1.0) as f32;
    let cache_frac = (mem.reclaimable_cache_bytes as f64 / total)
        .min((1.0 - real_frac as f64).max(0.0)) as f32;

    let w = ui.available_width().max(80.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 16.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(
        rect,
        CornerRadius::same(6),
        Color32::from_rgba_unmultiplied(255, 255, 255, 16),
    );

    let real_w = rect.width() * real_frac;
    if real_w > 1.0 {
        p.rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(real_w, rect.height())),
            CornerRadius::same(6),
            accent,
        );
    }
    let cache_w = rect.width() * cache_frac;
    if cache_w > 1.0 {
        p.rect_filled(
            Rect::from_min_size(
                pos2(rect.min.x + real_w, rect.min.y),
                Vec2::new(cache_w, rect.height()),
            ),
            CornerRadius::same(3),
            cache_col,
        );
    }

    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        legend_dot(
            ui,
            accent,
            &format!(
                "Uso Real (Apps + Kernel): {} ({:.1}%)",
                format_bytes(mem.real_used_bytes, iec),
                real_frac * 100.0
            ),
        );
        legend_dot(
            ui,
            cache_col,
            &format!(
                "Cache Inteligente (Liberável): {} ({:.1}%)",
                format_bytes(mem.reclaimable_cache_bytes, iec),
                cache_frac * 100.0
            ),
        );
        legend_dot(
            ui,
            Color32::from_rgba_unmultiplied(255, 255, 255, 45),
            &format!(
                "Livre: {} · Disponível (MemAvailable): {}",
                format_bytes(mem.free_bytes, iec),
                format_bytes(mem.available_bytes, iec)
            ),
        );
    });
}

fn legend_dot(ui: &mut Ui, col: Color32, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let (r, _) = ui.allocate_exact_size(Vec2::splat(9.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 4.0, col);
        ui.label(
            RichText::new(text)
                .size(11.5)
                .color(Color32::from_rgb(205, 214, 230)),
        );
    });
}

#[allow(clippy::too_many_arguments)]
pub fn render_overview_page(
    ui: &mut Ui,
    tel: &SystemTelemetry,
    cfg: &TasksConfig,
    accent: Color32,
    card_bg: Color32,
    card_stroke: Stroke,
    on_jump_page: &mut Option<TasksPage>,
) {
    let iec = cfg.show_iec_units;
    let cache_col = Color32::from_rgb(72, 199, 234);
    let ok_col = Color32::from_rgb(48, 209, 88);
    let warn_col = Color32::from_rgb(255, 159, 10);

    // Top 4 KPI cards
    ui.columns(4, |cols| {
        card(&mut cols[0], card_bg, card_stroke, |ui| {
            ui.label(RichText::new("PROCESSADOR (CPU)").size(10.5).strong().color(Color32::from_rgb(138, 148, 168)));
            ui.label(
                RichText::new(format!("{:.1}%", tel.cpu.total_pct))
                    .size(24.0)
                    .strong()
                    .color(Color32::WHITE),
            );
            let sub = if let Some(t) = tel.cpu.temp_c {
                format!("{:.0} MHz · {:.0}°C", tel.cpu.avg_freq_mhz, t)
            } else {
                format!("{:.0} MHz · {} threads", tel.cpu.avg_freq_mhz, tel.cpu.logical_cores)
            };
            ui.label(RichText::new(sub).size(11.0).color(Color32::from_rgb(150, 160, 180)));
            ui.add_space(6.0);
            draw_progress_bar(ui, tel.cpu.total_pct / 100.0, accent, 4.0);
        });

        card(&mut cols[1], card_bg, card_stroke, |ui| {
            ui.label(RichText::new("RAM REAL (APPS)").size(10.5).strong().color(Color32::from_rgb(138, 148, 168)));
            let pct = if tel.mem.total_bytes > 0 {
                tel.mem.real_used_bytes as f32 / tel.mem.total_bytes as f32 * 100.0
            } else {
                0.0
            };
            ui.label(
                RichText::new(format!("{:.1}%", pct))
                    .size(24.0)
                    .strong()
                    .color(Color32::WHITE),
            );
            ui.label(
                RichText::new(format!(
                    "{} / {}",
                    format_bytes(tel.mem.real_used_bytes, iec),
                    format_bytes(tel.mem.total_bytes, iec)
                ))
                .size(11.0)
                .color(Color32::from_rgb(150, 160, 180)),
            );
            ui.add_space(6.0);
            draw_progress_bar(ui, pct / 100.0, ok_col, 4.0);
        });

        card(&mut cols[2], card_bg, card_stroke, |ui| {
            ui.label(RichText::new("GRÁFICOS (GPU)").size(10.5).strong().color(Color32::from_rgb(138, 148, 168)));
            if let Some(gpu) = tel.gpus.first() {
                let busy = gpu.busy_pct.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "Ativa".to_string());
                ui.label(RichText::new(busy).size(24.0).strong().color(Color32::WHITE));
                let sub = match (gpu.vram_used_bytes, gpu.vram_total_bytes) {
                    (Some(u), Some(t)) => format!("VRAM {} / {}", format_bytes(u, iec), format_bytes(t, iec)),
                    _ => gpu.driver.clone(),
                };
                ui.label(RichText::new(sub).size(11.0).color(Color32::from_rgb(150, 160, 180)));
                ui.add_space(6.0);
                let frac = gpu.busy_pct.map(|v| v / 100.0).unwrap_or(0.15);
                draw_progress_bar(ui, frac, cache_col, 4.0);
            } else {
                ui.label(RichText::new("—").size(24.0).strong().color(Color32::WHITE));
                ui.label(RichText::new("Nenhuma GPU DRM").size(11.0).color(Color32::from_rgb(150, 160, 180)));
                ui.add_space(6.0);
                draw_progress_bar(ui, 0.0, cache_col, 4.0);
            }
        });

        card(&mut cols[3], card_bg, card_stroke, |ui| {
            ui.label(RichText::new("DISCO & REDE").size(10.5).strong().color(Color32::from_rgb(138, 148, 168)));
            let disk_bps: u64 = tel.disks.iter().map(|d| d.read_bps + d.write_bps).sum();
            let net_bps: u64 = tel.networks.iter().map(|n| n.rx_bps + n.tx_bps).sum();
            ui.label(
                RichText::new(format_rate(disk_bps, iec))
                    .size(20.0)
                    .strong()
                    .color(Color32::WHITE),
            );
            ui.label(
                RichText::new(format!("Rede: {}", format_rate(net_bps, iec)))
                    .size(11.0)
                    .color(Color32::from_rgb(150, 160, 180)),
            );
            ui.add_space(6.0);
            let activity = ((disk_bps + net_bps) as f32 / (50.0 * 1024.0 * 1024.0)).clamp(0.0, 1.0);
            draw_progress_bar(ui, activity, warn_col, 4.0);
        });
    });

    ui.add_space(12.0);

    // Sparklines
    ui.columns(2, |cols| {
        card(&mut cols[0], card_bg, card_stroke, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Histórico de CPU").size(13.0).strong().color(Color32::WHITE));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("Load: {:.2} {:.2} {:.2}", tel.cpu.load_1, tel.cpu.load_5, tel.cpu.load_15))
                            .size(11.0)
                            .color(Color32::from_rgb(150, 160, 180)),
                    );
                });
            });
            ui.add_space(6.0);
            draw_sparkline(ui, &tel.cpu_history, 100.0, accent, 95.0);
        });

        card(&mut cols[1], card_bg, card_stroke, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Histórico de RAM Real").size(13.0).strong().color(Color32::WHITE));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("Uptime: {}", format_uptime(tel.cpu.uptime_secs)))
                            .size(11.0)
                            .color(Color32::from_rgb(150, 160, 180)),
                    );
                });
            });
            ui.add_space(6.0);
            draw_sparkline(ui, &tel.ram_history, 100.0, ok_col, 95.0);
        });
    });

    ui.add_space(12.0);

    // Honest RAM + Cache + ZRAM card
    card(ui, card_bg, card_stroke, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Memória RAM: Uso Real vs Cache Inteligente")
                    .size(13.5)
                    .strong()
                    .color(Color32::WHITE),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if subtle_btn(ui, "Ver detalhes RAM & ZRAM ›").clicked() {
                    *on_jump_page = Some(TasksPage::Memory);
                }
            });
        });
        ui.add_space(6.0);
        draw_memory_stacked_bar(ui, &tel.mem, accent, cache_col, iec);
        ui.add_space(6.0);
        ui.label(
            RichText::new("Nota: O Linux usa RAM ociosa como Cache Inteligente de disco para acelerar apps. Esse cache é liberado instantaneamente quando necessário e não representa falta de memória.")
                .size(11.5)
                .color(Color32::from_rgb(150, 162, 184)),
        );
        if !tel.zram_devices.is_empty() {
            ui.add_space(6.0);
            let orig: u64 = tel.zram_devices.iter().map(|z| z.orig_data_bytes).sum();
            let used: u64 = tel.zram_devices.iter().map(|z| z.mem_used_bytes).sum();
            let saved: u64 = tel.zram_devices.iter().map(|z| z.saved_bytes).sum();
            ui.label(
                RichText::new(format!(
                    "ZRAM ativo: {} comprimidos em {} de RAM (economizando {})",
                    format_bytes(orig, iec),
                    format_bytes(used, iec),
                    format_bytes(saved, iec)
                ))
                .size(11.5)
                .color(warn_col),
            );
        }
    });

    ui.add_space(12.0);

    // Top 5 Apps by RAM
    card(ui, card_bg, card_stroke, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Principais Apps Abertos (RAM Isolada Total)")
                    .size(13.5)
                    .strong()
                    .color(Color32::WHITE),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if subtle_btn(ui, "Gerenciar todos os Apps ›").clicked() {
                    *on_jump_page = Some(TasksPage::Apps);
                }
            });
        });
        ui.add_space(8.0);
        for app in tel.app_groups.iter().take(6) {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&app.name).size(12.5).strong().color(Color32::WHITE));
                ui.label(
                    RichText::new(format!("({} proc.)", app.processes.len()))
                        .size(11.0)
                        .color(Color32::from_rgb(140, 150, 170)),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format_bytes(app.total_rss_bytes, iec))
                            .size(12.5)
                            .strong()
                            .color(accent),
                    );
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(format!("{:.1}% CPU", app.total_cpu_pct))
                            .size(11.5)
                            .color(Color32::from_rgb(175, 185, 204)),
                    );
                });
            });
            ui.add_space(3.0);
        }
    });
}

#[allow(clippy::too_many_arguments)]
pub fn render_apps_page(
    ui: &mut Ui,
    tel: &SystemTelemetry,
    cfg: &TasksConfig,
    search: &str,
    expanded_apps: &mut HashSet<String>,
    accent: Color32,
    card_bg: Color32,
    card_stroke: Stroke,
    actions: &mut Vec<UiAction>,
) {
    let iec = cfg.show_iec_units;
    let q = search.trim().to_lowercase();
    let danger = Color32::from_rgb(255, 69, 58);

    let filtered_groups: Vec<_> = tel
        .app_groups
        .iter()
        .filter(|group| {
            q.is_empty()
                || group.name.to_lowercase().contains(&q)
                || group
                    .processes
                    .iter()
                    .any(|p| p.name.to_lowercase().contains(&q) || p.cmdline.to_lowercase().contains(&q))
        })
        .collect();

    let total_apps_ram: u64 = filtered_groups.iter().map(|g| g.total_rss_bytes).sum();
    let total_ram_sys = tel.mem.total_bytes.max(1) as f64;

    card(ui, card_bg, card_stroke, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} apps agrupados · RAM total isolada: {}",
                    filtered_groups.len(),
                    format_bytes(total_apps_ram, iec)
                ))
                .size(12.5)
                .strong()
                .color(Color32::WHITE),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if subtle_btn(ui, "Recolher Tudo").clicked() {
                    expanded_apps.clear();
                }
                if subtle_btn(ui, "Expandir Tudo").clicked() {
                    for g in &filtered_groups {
                        expanded_apps.insert(g.name.clone());
                    }
                }
            });
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new("Soma proporcional (PSS/RSS) por aplicativo, incluindo abas, renderizadores e processos filhos.")
                .size(11.5)
                .color(Color32::from_rgb(150, 160, 180)),
        );
    });
    ui.add_space(10.0);

    for group in filtered_groups {
        let is_open = expanded_apps.contains(&group.name);
        let ram_frac = (group.total_rss_bytes as f64 / total_ram_sys).clamp(0.0, 1.0) as f32;
        card(ui, card_bg, card_stroke, |ui| {
            ui.horizontal(|ui| {
                let arrow = if is_open { "▾" } else { "▸" };
                if subtle_btn(ui, arrow).clicked() {
                    if is_open {
                        expanded_apps.remove(&group.name);
                    } else {
                        expanded_apps.insert(group.name.clone());
                    }
                }
                ui.label(RichText::new(&group.name).size(13.5).strong().color(Color32::WHITE));
                Frame::NONE
                    .fill(Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 40))
                    .corner_radius(CornerRadius::same(99))
                    .inner_margin(Margin::symmetric(7, 2))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("{} proc.", group.processes.len()))
                                .size(10.5)
                                .color(Color32::WHITE),
                        );
                    });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if pill_btn(ui, "Encerrar App", danger).clicked() {
                        let pids: Vec<u32> = group.processes.iter().map(|p| p.pid).collect();
                        actions.push(UiAction::KillGroup {
                            pids,
                            signal: libc::SIGTERM,
                            app_name: group.name.clone(),
                        });
                    }
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format_bytes(group.total_rss_bytes, iec))
                            .size(13.5)
                            .strong()
                            .color(accent),
                    );
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(format!("{:.1}% CPU", group.total_cpu_pct))
                            .size(12.0)
                            .color(Color32::from_rgb(195, 205, 222)),
                    );
                    if group.total_io_bps > 0 {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(format!("I/O {}", format_rate(group.total_io_bps, iec)))
                                .size(11.5)
                                .color(Color32::from_rgb(150, 160, 180)),
                        );
                    }
                });
            });
            ui.add_space(5.0);
            draw_progress_bar(ui, ram_frac, accent, 3.5);

            if is_open {
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                for (idx, p) in group.processes.iter().enumerate() {
                    let (_, st_col) = state_label_and_color(p.state);
                    let row_frame = if idx % 2 == 1 {
                        Frame::NONE
                            .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 5))
                            .corner_radius(CornerRadius::same(5))
                            .inner_margin(Margin::symmetric(6, 3))
                    } else {
                        Frame::NONE.inner_margin(Margin::symmetric(6, 3))
                    };
                    row_frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (dot_r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                            ui.painter().circle_filled(dot_r.center(), 3.2, st_col);
                            ui.label(
                                RichText::new(format!("{:<6}", p.pid))
                                    .size(11.5)
                                    .monospace()
                                    .color(Color32::from_rgb(145, 155, 175)),
                            );
                            ui.label(
                                RichText::new(truncate_str(&p.name, 34))
                                    .size(12.0)
                                    .color(Color32::from_rgb(225, 232, 244)),
                            )
                            .on_hover_text(&p.cmdline);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if subtle_btn(ui, "Forçar").clicked() {
                                    actions.push(UiAction::KillPid {
                                        pid: p.pid,
                                        signal: libc::SIGKILL,
                                        label: format!("{} ({})", p.name, p.pid),
                                    });
                                }
                                if subtle_btn(ui, "Encerrar").clicked() {
                                    actions.push(UiAction::KillPid {
                                        pid: p.pid,
                                        signal: libc::SIGTERM,
                                        label: format!("{} ({})", p.name, p.pid),
                                    });
                                }
                                ui.add_space(8.0);
                                ui.label(
                                    RichText::new(format_bytes(p.rss_bytes, iec))
                                        .size(11.5)
                                        .strong()
                                        .color(Color32::WHITE),
                                );
                                ui.add_space(10.0);
                                ui.label(
                                    RichText::new(format!("{:.1}%", p.cpu_pct))
                                        .size(11.5)
                                        .color(Color32::from_rgb(175, 185, 204)),
                                );
                            });
                        });
                    });
                }
            }
        });
        ui.add_space(6.0);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_processes_page(
    ui: &mut Ui,
    tel: &SystemTelemetry,
    cfg: &TasksConfig,
    search: &str,
    sort_col: &mut SortCol,
    sort_desc: &mut bool,
    selected_pid: &mut Option<u32>,
    current_uid: u32,
    accent: Color32,
    card_bg: Color32,
    card_stroke: Stroke,
    actions: &mut Vec<UiAction>,
) {
    let iec = cfg.show_iec_units;
    let q = search.trim().to_lowercase();
    let danger = Color32::from_rgb(255, 69, 58);

    let mut list: Vec<&ProcessInfo> = tel
        .processes
        .iter()
        .filter(|p| {
            if !cfg.show_kernel_threads && p.is_kernel_thread {
                return false;
            }
            if cfg.only_current_user && p.uid != current_uid {
                return false;
            }
            if !q.is_empty()
                && !p.name.to_lowercase().contains(&q)
                && !p.cmdline.to_lowercase().contains(&q)
                && !p.pid.to_string().contains(&q)
                && !p.user.to_lowercase().contains(&q)
            {
                return false;
            }
            true
        })
        .collect();

    list.sort_by(|a, b| {
        let ord = match sort_col {
            SortCol::Cpu => a.cpu_pct.total_cmp(&b.cpu_pct),
            SortCol::Ram => a.rss_bytes.cmp(&b.rss_bytes),
            SortCol::Pid => a.pid.cmp(&b.pid),
            SortCol::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortCol::Io => (a.io_read_bps + a.io_write_bps).cmp(&(b.io_read_bps + b.io_write_bps)),
        };
        if *sort_desc { ord.reverse() } else { ord }
    });

    // Selected process action bar
    if let Some(spid) = *selected_pid {
        if let Some(p) = tel.processes.iter().find(|x| x.pid == spid) {
            card(ui, card_bg, card_stroke, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{} (PID {})", p.name, p.pid))
                            .size(13.0)
                            .strong()
                            .color(Color32::WHITE),
                    );
                    let cgroup_str = p.cgroup_unit.as_deref().unwrap_or("—");
                    ui.label(
                        RichText::new(format!(
                            "· App: {} · Usuário: {} · PPID: {} · Nice: {} · Unit: {}",
                            p.app_group, p.user, p.ppid, p.nice, cgroup_str
                        ))
                        .size(11.5)
                        .color(Color32::from_rgb(155, 165, 185)),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if pill_btn(ui, "SIGKILL (Forçar)", danger).clicked() {
                            actions.push(UiAction::KillPid {
                                pid: p.pid,
                                signal: libc::SIGKILL,
                                label: format!("{} ({})", p.name, p.pid),
                            });
                        }
                        if subtle_btn(ui, "SIGTERM").clicked() {
                            actions.push(UiAction::KillPid {
                                pid: p.pid,
                                signal: libc::SIGTERM,
                                label: format!("{} ({})", p.name, p.pid),
                            });
                        }
                        let (sig, lbl) = if p.state == 'T' {
                            (libc::SIGCONT, "Retomar")
                        } else {
                            (libc::SIGSTOP, "Pausar")
                        };
                        if subtle_btn(ui, lbl).clicked() {
                            actions.push(UiAction::KillPid {
                                pid: p.pid,
                                signal: sig,
                                label: format!("{} ({})", p.name, p.pid),
                            });
                        }
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(&p.cmdline)
                        .size(11.0)
                        .monospace()
                        .color(Color32::from_rgb(145, 155, 175)),
                );
            });
            ui.add_space(8.0);
        }
    }

    // Sort bar
    card(ui, card_bg, card_stroke, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} processos exibidos", list.len()))
                    .size(11.5)
                    .color(Color32::from_rgb(155, 165, 185)),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for (col, label) in [
                    (SortCol::Io, "Disco I/O"),
                    (SortCol::Pid, "PID"),
                    (SortCol::Name, "Nome"),
                    (SortCol::Cpu, "CPU %"),
                    (SortCol::Ram, "RAM"),
                ] {
                    let active = *sort_col == col;
                    let arrow = if active {
                        if *sort_desc { " ↓" } else { " ↑" }
                    } else {
                        ""
                    };
                    let btn_col = if active {
                        accent
                    } else {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 14)
                    };
                    if pill_btn(ui, &format!("{label}{arrow}"), btn_col).clicked() {
                        if active {
                            *sort_desc = !*sort_desc;
                        } else {
                            *sort_col = col;
                            *sort_desc = true;
                        }
                    }
                }
            });
        });

        ui.add_space(8.0);
        // Column headers
        let (hdr_r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
        let hp = ui.painter();
        let hcy = hdr_r.center().y;
        let hdr_col = Color32::from_rgb(125, 136, 158);
        hp.text(pos2(hdr_r.min.x + 22.0, hcy), egui::Align2::LEFT_CENTER, "PID", FontId::proportional(10.5), hdr_col);
        hp.text(pos2(hdr_r.min.x + 80.0, hcy), egui::Align2::LEFT_CENTER, "PROCESSO", FontId::proportional(10.5), hdr_col);
        hp.text(pos2(hdr_r.max.x - 320.0, hcy), egui::Align2::RIGHT_CENTER, "USUÁRIO", FontId::proportional(10.5), hdr_col);
        hp.text(pos2(hdr_r.max.x - 245.0, hcy), egui::Align2::RIGHT_CENTER, "ESTADO", FontId::proportional(10.5), hdr_col);
        hp.text(pos2(hdr_r.max.x - 180.0, hcy), egui::Align2::RIGHT_CENTER, "CPU %", FontId::proportional(10.5), hdr_col);
        hp.text(pos2(hdr_r.max.x - 95.0, hcy), egui::Align2::RIGHT_CENTER, "RAM", FontId::proportional(10.5), hdr_col);
        hp.text(pos2(hdr_r.max.x - 8.0, hcy), egui::Align2::RIGHT_CENTER, "DISCO I/O", FontId::proportional(10.5), hdr_col);

        ui.separator();
        ui.add_space(2.0);

        for (idx, p) in list.into_iter().take(250).enumerate() {
            let is_sel = *selected_pid == Some(p.pid);
            let row_bg = if is_sel {
                Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 46)
            } else if idx % 2 == 1 {
                Color32::from_rgba_unmultiplied(255, 255, 255, 4)
            } else {
                Color32::TRANSPARENT
            };
            let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 27.0), Sense::click());
            if resp.hovered() && !is_sel {
                ui.painter().rect_filled(
                    r,
                    CornerRadius::same(6),
                    Color32::from_rgba_unmultiplied(255, 255, 255, 11),
                );
            } else if row_bg != Color32::TRANSPARENT {
                ui.painter().rect_filled(r, CornerRadius::same(6), row_bg);
            }
            if resp.clicked() {
                *selected_pid = Some(p.pid);
            }
            let (st_lbl, st_col) = state_label_and_color(p.state);
            let pnt = ui.painter();
            let cy = r.center().y;
            pnt.circle_filled(pos2(r.min.x + 11.0, cy), 3.2, st_col);
            pnt.text(
                pos2(r.min.x + 22.0, cy),
                egui::Align2::LEFT_CENTER,
                format!("{:<6}", p.pid),
                FontId::monospace(11.5),
                Color32::from_rgb(145, 155, 175),
            );
            let max_name_chars = (((r.width() - 420.0) / 7.2).max(14.0)) as usize;
            pnt.text(
                pos2(r.min.x + 80.0, cy),
                egui::Align2::LEFT_CENTER,
                truncate_str(&p.name, max_name_chars),
                FontId::proportional(12.5),
                Color32::WHITE,
            );
            pnt.text(
                pos2(r.max.x - 320.0, cy),
                egui::Align2::RIGHT_CENTER,
                truncate_str(&p.user, 12),
                FontId::proportional(11.5),
                Color32::from_rgb(155, 165, 185),
            );
            pnt.text(
                pos2(r.max.x - 245.0, cy),
                egui::Align2::RIGHT_CENTER,
                st_lbl,
                FontId::proportional(11.0),
                st_col,
            );
            pnt.text(
                pos2(r.max.x - 180.0, cy),
                egui::Align2::RIGHT_CENTER,
                format!("{:.1}%", p.cpu_pct),
                FontId::proportional(12.0),
                if p.cpu_pct > 25.0 {
                    Color32::from_rgb(255, 159, 10)
                } else {
                    Color32::from_rgb(210, 220, 236)
                },
            );
            pnt.text(
                pos2(r.max.x - 95.0, cy),
                egui::Align2::RIGHT_CENTER,
                format_bytes(p.rss_bytes, iec),
                FontId::proportional(12.0),
                accent,
            );
            let io_sum = p.io_read_bps + p.io_write_bps;
            pnt.text(
                pos2(r.max.x - 8.0, cy),
                egui::Align2::RIGHT_CENTER,
                if io_sum > 0 { format_rate(io_sum, iec) } else { "—".to_string() },
                FontId::proportional(11.0),
                Color32::from_rgb(145, 155, 175),
            );
            resp.on_hover_text(&p.cmdline);
        }
    });
}

pub fn render_compute_page(
    ui: &mut Ui,
    tel: &SystemTelemetry,
    cfg: &TasksConfig,
    accent: Color32,
    card_bg: Color32,
    card_stroke: Stroke,
) {
    let iec = cfg.show_iec_units;
    card(ui, card_bg, card_stroke, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&tel.cpu.model).size(14.5).strong().color(Color32::WHITE));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{:.1}%", tel.cpu.total_pct))
                        .size(20.0)
                        .strong()
                        .color(accent),
                );
            });
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!(
                "{} núcleos físicos · {} threads lógicas · Freq média {:.0} MHz (máx {:.0} MHz) · {} threads ativas",
                tel.cpu.physical_cores,
                tel.cpu.logical_cores,
                tel.cpu.avg_freq_mhz,
                tel.cpu.max_freq_mhz,
                tel.cpu.total_threads
            ))
            .size(11.5)
            .color(Color32::from_rgb(160, 170, 190)),
        );
        ui.add_space(8.0);
        draw_sparkline(ui, &tel.cpu_history, 100.0, accent, 110.0);
        ui.add_space(10.0);

        ui.label(RichText::new("Uso por Núcleo / Thread").size(12.5).strong().color(Color32::WHITE));
        ui.add_space(6.0);
        let cols_count = 4.min(tel.cpu.per_core_pct.len().max(1));
        for (row_idx, chunk) in tel.cpu.per_core_pct.chunks(cols_count).enumerate() {
            ui.columns(cols_count, |cols| {
                for (i, &pct) in chunk.iter().enumerate() {
                    let core_idx = row_idx * cols_count + i;
                    cols[i].horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("C{core_idx:02} {pct:4.0}%"))
                                .size(11.0)
                                .monospace()
                                .color(Color32::from_rgb(200, 210, 228)),
                        );
                        draw_progress_bar(
                            ui,
                            pct / 100.0,
                            if pct > 80.0 {
                                Color32::from_rgb(255, 69, 58)
                            } else {
                                accent
                            },
                            7.0,
                        );
                    });
                }
            });
            ui.add_space(3.0);
        }
    });

    ui.add_space(12.0);

    if tel.gpus.is_empty() {
        card(ui, card_bg, card_stroke, |ui| {
            ui.label(
                RichText::new("Nenhuma GPU DRM detectada em /sys/class/drm")
                    .size(13.0)
                    .color(Color32::from_rgb(160, 170, 190)),
            );
        });
    } else {
        for gpu in &tel.gpus {
            card(ui, card_bg, card_stroke, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&gpu.name).size(14.0).strong().color(Color32::WHITE));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(b) = gpu.busy_pct {
                            ui.label(
                                RichText::new(format!("{b:.0}%"))
                                    .size(18.0)
                                    .strong()
                                    .color(Color32::from_rgb(48, 209, 88)),
                            );
                        }
                    });
                });
                ui.add_space(4.0);
                let mut details = vec![format!("Driver: {}", gpu.driver)];
                if let Some(t) = gpu.temp_c {
                    details.push(format!("Temp: {t:.0}°C"));
                }
                if let Some(p) = gpu.power_w {
                    details.push(format!("Energia: {p:.1} W"));
                }
                if let Some(f) = gpu.freq_mhz {
                    details.push(format!("Clock: {f} MHz"));
                }
                if let Some(gtt) = gpu.gtt_used_bytes {
                    details.push(format!("GTT: {}", format_bytes(gtt, iec)));
                }
                ui.label(
                    RichText::new(details.join(" · "))
                        .size(11.5)
                        .color(Color32::from_rgb(160, 170, 190)),
                );
                if let (Some(u), Some(t)) = (gpu.vram_used_bytes, gpu.vram_total_bytes) {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("VRAM: {} / {}", format_bytes(u, iec), format_bytes(t, iec)))
                            .size(12.0)
                            .color(Color32::WHITE),
                    );
                    draw_progress_bar(ui, (u as f64 / t.max(1) as f64) as f32, Color32::from_rgb(48, 209, 88), 8.0);
                }
                if gpu.busy_pct.is_some() {
                    ui.add_space(8.0);
                    draw_sparkline(ui, &tel.gpu_history, 100.0, Color32::from_rgb(48, 209, 88), 85.0);
                }
            });
            ui.add_space(10.0);
        }
    }
}

pub fn render_memory_page(
    ui: &mut Ui,
    tel: &SystemTelemetry,
    cfg: &TasksConfig,
    accent: Color32,
    card_bg: Color32,
    card_stroke: Stroke,
) {
    let iec = cfg.show_iec_units;
    let cache_col = Color32::from_rgb(72, 199, 234);
    let warn_col = Color32::from_rgb(255, 159, 10);

    card(ui, card_bg, card_stroke, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Memória RAM Física (Detalhamento Honesto)")
                    .size(14.0)
                    .strong()
                    .color(Color32::WHITE),
            );
            let pressure_frac = if tel.mem.total_bytes > 0 {
                tel.mem.real_used_bytes as f32 / tel.mem.total_bytes as f32
            } else {
                0.0
            };
            let (p_lbl, p_col) = if pressure_frac > 0.85 {
                ("Pressão Alta", Color32::from_rgb(255, 69, 58))
            } else if pressure_frac > 0.65 {
                ("Pressão Moderada", Color32::from_rgb(255, 159, 10))
            } else {
                ("Pressão Baixa", Color32::from_rgb(48, 209, 88))
            };
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                Frame::NONE
                    .fill(Color32::from_rgba_unmultiplied(p_col.r(), p_col.g(), p_col.b(), 36))
                    .corner_radius(CornerRadius::same(99))
                    .inner_margin(Margin::symmetric(9, 2))
                    .show(ui, |ui| {
                        ui.label(RichText::new(p_lbl).size(11.0).strong().color(p_col));
                    });
            });
        });
        ui.add_space(8.0);
        draw_memory_stacked_bar(ui, &tel.mem, accent, cache_col, iec);
        ui.add_space(10.0);

        ui.columns(2, |cols| {
            kv_row(&mut cols[0], "Total Instalado", &format_bytes(tel.mem.total_bytes, iec));
            kv_row(&mut cols[0], "Uso Real (Apps & Kernel)", &format_bytes(tel.mem.real_used_bytes, iec));
            kv_row(&mut cols[0], "Disponível Real (MemAvailable)", &format_bytes(tel.mem.available_bytes, iec));
            kv_row(&mut cols[0], "Livre Imediato (MemFree)", &format_bytes(tel.mem.free_bytes, iec));

            kv_row(&mut cols[1], "Cache Inteligente Total", &format_bytes(tel.mem.reclaimable_cache_bytes, iec));
            kv_row(&mut cols[1], "Page Cache + Buffers", &format_bytes(tel.mem.cached_bytes.saturating_sub(tel.mem.shmem_bytes) + tel.mem.buffers_bytes, iec));
            kv_row(&mut cols[1], "Slab Reaproveitável (SReclaimable)", &format_bytes(tel.mem.sreclaimable_bytes, iec));
            kv_row(&mut cols[1], "Memória Compartilhada (Shmem/tmpfs)", &format_bytes(tel.mem.shmem_bytes, iec));
        });
    });

    ui.add_space(12.0);

    card(ui, card_bg, card_stroke, |ui| {
        ui.label(RichText::new("Swap & Memória Virtual").size(14.0).strong().color(Color32::WHITE));
        ui.add_space(6.0);
        if tel.mem.swap_total_bytes == 0 {
            ui.label(
                RichText::new("Nenhum dispositivo Swap ativo.")
                    .size(12.0)
                    .color(Color32::from_rgb(155, 165, 185)),
            );
        } else {
            let frac = (tel.mem.swap_used_bytes as f64 / tel.mem.swap_total_bytes as f64) as f32;
            ui.label(
                RichText::new(format!(
                    "Usado: {} de {} ({:.1}%)",
                    format_bytes(tel.mem.swap_used_bytes, iec),
                    format_bytes(tel.mem.swap_total_bytes, iec),
                    frac * 100.0
                ))
                .size(12.5)
                .color(Color32::WHITE),
            );
            draw_progress_bar(ui, frac, warn_col, 8.0);
            ui.add_space(8.0);
            for sw in &tel.swap_devices {
                kv_row(
                    ui,
                    &format!("{} ({})", sw.path, sw.kind),
                    &format!("{} / {} (prio {})", format_bytes(sw.used_bytes, iec), format_bytes(sw.size_bytes, iec), sw.priority),
                );
            }
        }
    });

    ui.add_space(12.0);

    card(ui, card_bg, card_stroke, |ui| {
        ui.label(RichText::new("ZRAM (Compressão em RAM)").size(14.0).strong().color(Color32::WHITE));
        ui.add_space(6.0);
        if tel.zram_devices.is_empty() {
            ui.label(
                RichText::new("ZRAM não está configurado neste sistema.")
                    .size(12.0)
                    .color(Color32::from_rgb(155, 165, 185)),
            );
        } else {
            for z in &tel.zram_devices {
                ui.label(
                    RichText::new(format!("{} · Algoritmo: {} · Tamanho disco: {}", z.name, z.algorithm, format_bytes(z.disksize_bytes, iec)))
                        .size(12.5)
                        .strong()
                        .color(accent),
                );
                ui.add_space(4.0);
                kv_row(ui, "Dados originais guardados", &format_bytes(z.orig_data_bytes, iec));
                kv_row(ui, "Dados comprimidos (payload)", &format_bytes(z.compr_data_bytes, iec));
                kv_row(ui, "RAM física realmente consumida", &format_bytes(z.mem_used_bytes, iec));
                kv_row(ui, "Taxa de compressão", &format!("{:.2}x", z.ratio));
                kv_row(ui, "RAM economizada pelo ZRAM", &format_bytes(z.saved_bytes, iec));
                ui.add_space(6.0);
            }
        }
    });
}

pub fn render_storage_net_page(
    ui: &mut Ui,
    tel: &SystemTelemetry,
    cfg: &TasksConfig,
    accent: Color32,
    card_bg: Color32,
    card_stroke: Stroke,
) {
    let iec = cfg.show_iec_units;
    let ok_col = Color32::from_rgb(48, 209, 88);
    let warn_col = Color32::from_rgb(255, 159, 10);

    card(ui, card_bg, card_stroke, |ui| {
        ui.label(RichText::new("Sistemas de Arquivos Montados").size(14.0).strong().color(Color32::WHITE));
        ui.add_space(8.0);
        for m in &tel.mounts {
            let frac = if m.total_bytes > 0 {
                (m.used_bytes as f64 / m.total_bytes as f64) as f32
            } else {
                0.0
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(&m.mount_point).size(12.5).strong().color(Color32::WHITE));
                ui.label(
                    RichText::new(format!("{} ({})", m.device, m.fs_type))
                        .size(11.0)
                        .color(Color32::from_rgb(145, 155, 175)),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} livre · {} / {}",
                            format_bytes(m.avail_bytes, iec),
                            format_bytes(m.used_bytes, iec),
                            format_bytes(m.total_bytes, iec)
                        ))
                        .size(11.5)
                        .color(Color32::from_rgb(200, 210, 228)),
                    );
                });
            });
            draw_progress_bar(ui, frac, if frac > 0.9 { Color32::from_rgb(255, 69, 58) } else { accent }, 7.0);
            ui.add_space(6.0);
        }
    });

    ui.add_space(12.0);

    ui.columns(2, |cols| {
        card(&mut cols[0], card_bg, card_stroke, |ui| {
            ui.label(RichText::new("Discos Físicos (Leitura / Escrita)").size(13.5).strong().color(Color32::WHITE));
            ui.add_space(6.0);
            draw_dual_sparkline(ui, &tel.disk_read_history, &tel.disk_write_history, ok_col, warn_col, 90.0);
            ui.add_space(6.0);
            for d in &tel.disks {
                kv_row(
                    ui,
                    &format!("{} ({} IOPS)", d.name, d.iops),
                    &format!("L: {}  E: {}", format_rate(d.read_bps, iec), format_rate(d.write_bps, iec)),
                );
            }
        });

        card(&mut cols[1], card_bg, card_stroke, |ui| {
            ui.label(RichText::new("Rede (Download / Upload)").size(13.5).strong().color(Color32::WHITE));
            ui.add_space(6.0);
            draw_dual_sparkline(ui, &tel.net_rx_history, &tel.net_tx_history, accent, warn_col, 90.0);
            ui.add_space(6.0);
            for n in &tel.networks {
                kv_row(
                    ui,
                    &n.name,
                    &format!(
                        "↓ {} ({})  ↑ {} ({})",
                        format_rate(n.rx_bps, iec),
                        format_bytes(n.total_rx_bytes, iec),
                        format_rate(n.tx_bps, iec),
                        format_bytes(n.total_tx_bytes, iec)
                    ),
                );
            }
        });
    });
}

#[allow(clippy::too_many_arguments)]
pub fn render_services_page(
    ui: &mut Ui,
    services: &[SystemdService],
    scope: &mut ServiceScope,
    filter: &mut ServiceFilter,
    search: &str,
    loading: bool,
    err_msg: Option<&str>,
    accent: Color32,
    card_bg: Color32,
    card_stroke: Stroke,
    actions: &mut Vec<UiAction>,
) {
    let q = search.trim().to_lowercase();
    let ok_col = Color32::from_rgb(48, 209, 88);
    let danger_col = Color32::from_rgb(255, 69, 58);

    card(ui, card_bg, card_stroke, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (sc, lbl) in [
                (ServiceScope::System, "Sistema (--system)"),
                (ServiceScope::User, "Usuário (--user)"),
            ] {
                let col = if *scope == sc {
                    accent
                } else {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 14)
                };
                if pill_btn(ui, lbl, col).clicked() && *scope != sc {
                    *scope = sc;
                    actions.push(UiAction::RefreshServices);
                }
            }

            ui.add_space(12.0);
            for (fl, lbl) in [
                (ServiceFilter::All, "Todos"),
                (ServiceFilter::BootEnabled, "Iniciam no Boot"),
                (ServiceFilter::Active, "Ativos"),
                (ServiceFilter::Failed, "Falhos"),
            ] {
                let col = if *filter == fl {
                    Color32::from_rgb(65, 95, 165)
                } else {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 14)
                };
                if pill_btn(ui, lbl, col).clicked() {
                    *filter = fl;
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if subtle_btn(ui, if loading { "Atualizando..." } else { "Recarregar" }).clicked() {
                    actions.push(UiAction::RefreshServices);
                }
            });
        });

        if let Some(err) = err_msg {
            ui.add_space(6.0);
            ui.label(RichText::new(err).size(12.0).color(danger_col));
        }
    });

    ui.add_space(10.0);

    let filtered: Vec<&SystemdService> = services
        .iter()
        .filter(|s| {
            let match_filter = match filter {
                ServiceFilter::All => true,
                ServiceFilter::BootEnabled => s.starts_at_boot(),
                ServiceFilter::Active => s.is_active(),
                ServiceFilter::Failed => s.is_failed(),
            };
            if !match_filter {
                return false;
            }
            if !q.is_empty()
                && !s.unit.to_lowercase().contains(&q)
                && !s.description.to_lowercase().contains(&q)
                && !s.boot_state.to_lowercase().contains(&q)
            {
                return false;
            }
            true
        })
        .collect();

    let boot_count = services.iter().filter(|s| s.starts_at_boot()).count();
    let active_count = services.iter().filter(|s| s.is_active()).count();
    let failed_count = services.iter().filter(|s| s.is_failed()).count();

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{} exibidos · {} iniciam no boot · {} ativos",
                filtered.len(),
                boot_count,
                active_count
            ))
            .size(11.5)
            .color(Color32::from_rgb(150, 160, 180)),
        );
        if failed_count > 0 {
            ui.label(
                RichText::new(format!("· {failed_count} falhos"))
                    .size(11.5)
                    .strong()
                    .color(danger_col),
            );
        }
    });
    ui.add_space(6.0);

    for s in filtered.into_iter().take(200) {
        card(ui, card_bg, card_stroke, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&s.unit).size(13.0).strong().color(Color32::WHITE));
                        let boot_col = if s.starts_at_boot() {
                            ok_col
                        } else {
                            Color32::from_rgb(130, 140, 160)
                        };
                        ui.label(
                            RichText::new(format!("[boot: {}]", s.boot_state))
                                .size(11.0)
                                .color(boot_col),
                        );
                        let act_col = if s.is_failed() {
                            danger_col
                        } else if s.is_active() {
                            accent
                        } else {
                            Color32::from_rgb(130, 140, 160)
                        };
                        ui.label(
                            RichText::new(format!("{} ({})", s.active_state, s.sub_state))
                                .size(11.0)
                                .color(act_col),
                        );
                    });
                    if !s.description.is_empty() {
                        ui.label(
                            RichText::new(&s.description)
                                .size(11.5)
                                .color(Color32::from_rgb(155, 165, 185)),
                        );
                    }
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if subtle_btn(ui, "Logs").clicked() {
                        actions.push(UiAction::FetchLogs {
                            unit: s.unit.clone(),
                            scope: s.scope,
                        });
                    }
                    if s.starts_at_boot() {
                        if subtle_btn(ui, "Desativar Boot").clicked() {
                            actions.push(UiAction::ServiceOp {
                                unit: s.unit.clone(),
                                scope: s.scope,
                                action: ServiceAction::Disable,
                            });
                        }
                    } else if s.boot_state == "disabled" {
                        if subtle_btn(ui, "Ativar Boot").clicked() {
                            actions.push(UiAction::ServiceOp {
                                unit: s.unit.clone(),
                                scope: s.scope,
                                action: ServiceAction::Enable,
                            });
                        }
                    }
                    if s.is_active() {
                        if subtle_btn(ui, "Parar").clicked() {
                            actions.push(UiAction::ServiceOp {
                                unit: s.unit.clone(),
                                scope: s.scope,
                                action: ServiceAction::Stop,
                            });
                        }
                        if subtle_btn(ui, "Reiniciar").clicked() {
                            actions.push(UiAction::ServiceOp {
                                unit: s.unit.clone(),
                                scope: s.scope,
                                action: ServiceAction::Restart,
                            });
                        }
                    } else if subtle_btn(ui, "Iniciar").clicked() {
                        actions.push(UiAction::ServiceOp {
                            unit: s.unit.clone(),
                            scope: s.scope,
                            action: ServiceAction::Start,
                        });
                    }
                });
            });
        });
        ui.add_space(5.0);
    }
}

pub fn render_settings_page(
    ui: &mut Ui,
    cfg: &mut TasksConfig,
    has_sudo_cached: bool,
    accent: Color32,
    card_bg: Color32,
    card_stroke: Stroke,
    actions: &mut Vec<UiAction>,
) {
    card(ui, card_bg, card_stroke, |ui| {
        ui.label(RichText::new("Coleta & Desempenho").size(14.0).strong().color(Color32::WHITE));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Intervalo de atualização:").size(12.5).color(Color32::from_rgb(200, 210, 228)));
            for (ms, lbl) in [(500, "0.5s"), (1000, "1s"), (2000, "2s"), (5000, "5s")] {
                let col = if cfg.refresh_ms == ms {
                    accent
                } else {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 14)
                };
                if pill_btn(ui, lbl, col).clicked() {
                    cfg.refresh_ms = ms;
                    actions.push(UiAction::SaveConfig);
                }
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Amostras nos gráficos:").size(12.5).color(Color32::from_rgb(200, 210, 228)));
            for n in [30usize, 60, 120] {
                let col = if cfg.history_samples == n {
                    accent
                } else {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 14)
                };
                if pill_btn(ui, &format!("{n}"), col).clicked() {
                    cfg.history_samples = n;
                    actions.push(UiAction::SaveConfig);
                }
            }
        });
    });

    ui.add_space(12.0);

    card(ui, card_bg, card_stroke, |ui| {
        ui.label(RichText::new("Processos & Segurança").size(14.0).strong().color(Color32::WHITE));
        ui.add_space(8.0);
        if ui.checkbox(&mut cfg.confirm_before_kill, "Confirmar antes de encerrar processos ou apps").changed() {
            actions.push(UiAction::SaveConfig);
        }
        if ui.checkbox(&mut cfg.show_kernel_threads, "Mostrar threads do Kernel Linux na lista de processos").changed() {
            actions.push(UiAction::SaveConfig);
        }
        if ui.checkbox(&mut cfg.only_current_user, "Mostrar apenas processos do meu usuário").changed() {
            actions.push(UiAction::SaveConfig);
        }
        if ui.checkbox(&mut cfg.show_iec_units, "Usar unidades binárias (KiB, MiB, GiB)").changed() {
            actions.push(UiAction::SaveConfig);
        }
        if ui.checkbox(&mut cfg.remember_sudo_session, "Manter senha sudo em memória durante esta sessão").changed() {
            actions.push(UiAction::SaveConfig);
        }
        if has_sudo_cached {
            ui.add_space(6.0);
            if subtle_btn(ui, "Limpar senha sudo da memória agora").clicked() {
                actions.push(UiAction::ClearSudoCache);
            }
        }
    });
}

fn kv_row(ui: &mut Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .size(12.0)
                .color(Color32::from_rgb(160, 170, 190)),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(value)
                    .size(12.0)
                    .strong()
                    .color(Color32::WHITE),
            );
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_iec_and_si() {
        assert_eq!(format_bytes(1024, true), "1.00 KiB");
        assert_eq!(format_bytes(1000, false), "1.00 KB");
        assert_eq!(format_bytes(512, true), "512 B");
    }
}
