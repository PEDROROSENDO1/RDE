use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};
use std::f32::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Rocket,
    Search,
    ControlCenter,
    Wifi,
    WifiOff,
    Bluetooth,
    Moon,
    Bolt,
    Sun,
    VolumeMute,
    VolumeLow,
    VolumeHigh,
    Battery(u8, bool),
    MusicNote,
    Play,
    Pause,
    Next,
    Prev,
    Keyboard,
    Close,
    Trash,
    OpenApp,
    Star,
    StarOutline,
    Terminal,
    Calculator,
    ChevronLeft,
    ChevronRight,
    Camera,
    VideoRecord,
    StopRecord,
    Cursor,
    Mic,
    MicMute,
}

/// Draws a crisp, anti-aliased vector icon centered at `center` within `size x size`.
pub fn draw_icon(painter: &Painter, center: Pos2, size: f32, kind: IconKind, color: Color32) {
    let s = size;
    let r = s * 0.5;
    let stroke_w = (s * 0.105).clamp(1.1, 2.2);
    let stroke = Stroke::new(stroke_w, color);

    match kind {
        IconKind::Rocket => {
            // Minimalist Apple/Rocket geometric mark: sleek upward delta/rocket silhouette
            let top = pos2(center.x, center.y - r * 0.85);
            let right_wing = pos2(center.x + r * 0.65, center.y + r * 0.68);
            let notch = pos2(center.x, center.y + r * 0.34);
            let left_wing = pos2(center.x - r * 0.65, center.y + r * 0.68);
            painter.add(Shape::convex_polygon(
                vec![top, right_wing, notch, left_wing],
                color,
                Stroke::NONE,
            ));
        }

        IconKind::Search => {
            let lens_center = pos2(center.x - r * 0.16, center.y - r * 0.16);
            let lens_r = r * 0.48;
            painter.circle_stroke(lens_center, lens_r, stroke);
            let dir = vec2(0.707, 0.707);
            let handle_start = lens_center + dir * lens_r;
            let handle_end = lens_center + dir * (lens_r + r * 0.56);
            painter.line_segment([handle_start, handle_end], Stroke::new(stroke_w * 1.15, color));
        }

        IconKind::ControlCenter => {
            // Two horizontal toggle pills (macOS Control Center glyph)
            let w = s * 0.82;
            let x0 = center.x - w * 0.5;
            let x1 = center.x + w * 0.5;
            let y_top = center.y - s * 0.20;
            let y_bot = center.y + s * 0.20;
            let knob_r = (s * 0.15).max(2.0);

            painter.line_segment([pos2(x0, y_top), pos2(x1 - knob_r * 1.3, y_top)], stroke);
            painter.circle_filled(pos2(x1 - knob_r, y_top), knob_r, color);

            painter.line_segment([pos2(x0 + knob_r * 1.3, y_bot), pos2(x1, y_bot)], stroke);
            painter.circle_stroke(pos2(x0 + knob_r, y_bot), knob_r * 0.9, stroke);
        }

        IconKind::Wifi | IconKind::WifiOff => {
            let origin = pos2(center.x, center.y + r * 0.52);
            painter.circle_filled(origin, (s * 0.085).max(1.3), color);
            for frac in [0.46, 0.82, 1.18] {
                let arc_r = r * frac;
                draw_arc(
                    painter,
                    origin,
                    arc_r,
                    -PI * 0.75,
                    -PI * 0.25,
                    stroke,
                );
            }
            if matches!(kind, IconKind::WifiOff) {
                painter.line_segment(
                    [
                        pos2(center.x - r * 0.7, center.y - r * 0.7),
                        pos2(center.x + r * 0.7, center.y + r * 0.7),
                    ],
                    Stroke::new(stroke_w * 1.1, color),
                );
            }
        }

        IconKind::Bluetooth => {
            let x = center.x;
            let y = center.y;
            let dx = r * 0.44;
            let dy = r * 0.38;
            let pts = [
                pos2(x - dx * 0.8, y - dy * 0.85),
                pos2(x + dx, y + dy),
                pos2(x, y + dy * 1.9),
                pos2(x, y - dy * 1.9),
                pos2(x + dx, y - dy),
                pos2(x - dx * 0.8, y + dy * 0.85),
            ];
            for win in pts.windows(2) {
                painter.line_segment([win[0], win[1]], stroke);
            }
        }

        IconKind::Moon => {
            let outer_r = r * 0.68;
            painter.circle_stroke(center, outer_r, stroke);
            // Inner crescent arc
            draw_arc(
                painter,
                pos2(center.x + r * 0.35, center.y - r * 0.25),
                outer_r * 0.78,
                PI * 0.65,
                PI * 1.35,
                stroke,
            );
        }

        IconKind::Bolt => {
            let pts = vec![
                pos2(center.x + r * 0.14, center.y - r * 0.82),
                pos2(center.x - r * 0.44, center.y + r * 0.08),
                pos2(center.x - r * 0.02, center.y + r * 0.08),
                pos2(center.x - r * 0.16, center.y + r * 0.82),
                pos2(center.x + r * 0.44, center.y - r * 0.08),
                pos2(center.x + r * 0.02, center.y - r * 0.08),
            ];
            painter.add(Shape::Path(egui::epaint::PathShape::convex_polygon(
                pts,
                color,
                Stroke::NONE,
            )));
        }

        IconKind::Sun => {
            let core_r = r * 0.36;
            painter.circle_filled(center, core_r, color);
            for i in 0..8 {
                let ang = (i as f32) * (PI * 0.25);
                let dir = vec2(ang.cos(), ang.sin());
                painter.line_segment(
                    [center + dir * (r * 0.56), center + dir * (r * 0.84)],
                    stroke,
                );
            }
        }

        IconKind::VolumeMute | IconKind::VolumeLow | IconKind::VolumeHigh => {
            let bx = center.x - r * 0.58;
            let bw = r * 0.34;
            let bh = r * 0.34;
            let cone_pts = vec![
                pos2(bx, center.y - bh),
                pos2(bx + bw, center.y - bh),
                pos2(center.x + r * 0.04, center.y - r * 0.62),
                pos2(center.x + r * 0.04, center.y + r * 0.62),
                pos2(bx + bw, center.y + bh),
                pos2(bx, center.y + bh),
            ];
            painter.add(Shape::convex_polygon(cone_pts, color, Stroke::NONE));

            match kind {
                IconKind::VolumeMute => {
                    let xc = center.x + r * 0.48;
                    let xr = r * 0.28;
                    painter.line_segment(
                        [pos2(xc - xr, center.y - xr), pos2(xc + xr, center.y + xr)],
                        stroke,
                    );
                    painter.line_segment(
                        [pos2(xc - xr, center.y + xr), pos2(xc + xr, center.y - xr)],
                        stroke,
                    );
                }
                IconKind::VolumeLow => {
                    draw_arc(
                        painter,
                        pos2(center.x + r * 0.08, center.y),
                        r * 0.42,
                        -PI * 0.32,
                        PI * 0.32,
                        stroke,
                    );
                }
                IconKind::VolumeHigh => {
                    draw_arc(
                        painter,
                        pos2(center.x + r * 0.08, center.y),
                        r * 0.38,
                        -PI * 0.32,
                        PI * 0.32,
                        stroke,
                    );
                    draw_arc(
                        painter,
                        pos2(center.x + r * 0.08, center.y),
                        r * 0.68,
                        -PI * 0.34,
                        PI * 0.34,
                        stroke,
                    );
                }
                _ => {}
            }
        }

        IconKind::Battery(pct, charging) => {
            let bw = s * 0.96;
            let bh = s * 0.56;
            let body = Rect::from_center_size(pos2(center.x - s * 0.05, center.y), vec2(bw, bh));
            painter.rect_stroke(body, 3.0, stroke, egui::StrokeKind::Inside);

            // Terminal nub
            let nub = Rect::from_min_size(
                pos2(body.max.x + 1.0, center.y - bh * 0.24),
                vec2(2.0, bh * 0.48),
            );
            painter.rect_filled(nub, 1.0, color);

            // Fill level
            let frac = (pct.min(100) as f32) / 100.0;
            let inner = body.shrink(2.2);
            let fill_w = (inner.width() * frac).max(1.5);
            let fill_rect = Rect::from_min_size(inner.min, vec2(fill_w, inner.height()));
            let fill_col = if charging {
                Color32::from_rgb(48, 209, 88)
            } else if pct <= 20 {
                Color32::from_rgb(255, 69, 58)
            } else {
                color
            };
            painter.rect_filled(fill_rect, 1.5, fill_col);
        }

        IconKind::MusicNote => {
            let head_l = pos2(center.x - r * 0.34, center.y + r * 0.46);
            let head_r = pos2(center.x + r * 0.36, center.y + r * 0.32);
            painter.circle_filled(head_l, r * 0.24, color);
            painter.circle_filled(head_r, r * 0.24, color);

            let top_l = pos2(head_l.x + r * 0.14, center.y - r * 0.54);
            let top_r = pos2(head_r.x + r * 0.14, center.y - r * 0.68);
            painter.line_segment([pos2(head_l.x + r * 0.14, head_l.y), top_l], stroke);
            painter.line_segment([pos2(head_r.x + r * 0.14, head_r.y), top_r], stroke);
            painter.line_segment([top_l, top_r], Stroke::new(stroke_w * 1.4, color));
        }

        IconKind::Play => {
            let pts = vec![
                pos2(center.x - r * 0.42, center.y - r * 0.58),
                pos2(center.x + r * 0.58, center.y),
                pos2(center.x - r * 0.42, center.y + r * 0.58),
            ];
            painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
        }

        IconKind::Pause => {
            let bar_w = (r * 0.34).max(2.2);
            let bar_h = r * 1.1;
            let l_rect = Rect::from_center_size(pos2(center.x - r * 0.32, center.y), vec2(bar_w, bar_h));
            let r_rect = Rect::from_center_size(pos2(center.x + r * 0.32, center.y), vec2(bar_w, bar_h));
            painter.rect_filled(l_rect, 1.2, color);
            painter.rect_filled(r_rect, 1.2, color);
        }

        IconKind::Next => {
            let pts = vec![
                pos2(center.x - r * 0.54, center.y - r * 0.5),
                pos2(center.x + r * 0.22, center.y),
                pos2(center.x - r * 0.54, center.y + r * 0.5),
            ];
            painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
            let bar = Rect::from_center_size(pos2(center.x + r * 0.42, center.y), vec2((r * 0.22).max(1.8), r * 1.0));
            painter.rect_filled(bar, 1.0, color);
        }

        IconKind::Prev => {
            let pts = vec![
                pos2(center.x + r * 0.54, center.y - r * 0.5),
                pos2(center.x - r * 0.22, center.y),
                pos2(center.x + r * 0.54, center.y + r * 0.5),
            ];
            painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
            let bar = Rect::from_center_size(pos2(center.x - r * 0.42, center.y), vec2((r * 0.22).max(1.8), r * 1.0));
            painter.rect_filled(bar, 1.0, color);
        }

        IconKind::Keyboard => {
            let rect = Rect::from_center_size(center, vec2(s * 0.92, s * 0.62));
            painter.rect_stroke(rect, 2.5, stroke, egui::StrokeKind::Inside);
            let y1 = center.y - s * 0.08;
            let y2 = center.y + s * 0.12;
            for dx in [-0.26, 0.0, 0.26] {
                painter.circle_filled(pos2(center.x + s * dx, y1), 1.1, color);
            }
            painter.line_segment(
                [pos2(center.x - s * 0.24, y2), pos2(center.x + s * 0.24, y2)],
                Stroke::new(stroke_w * 0.9, color),
            );
        }

        IconKind::Close => {
            let d = r * 0.54;
            painter.line_segment([pos2(center.x - d, center.y - d), pos2(center.x + d, center.y + d)], stroke);
            painter.line_segment([pos2(center.x - d, center.y + d), pos2(center.x + d, center.y - d)], stroke);
        }

        IconKind::Trash => {
            let lid_y = center.y - r * 0.45;
            painter.line_segment([pos2(center.x - r * 0.56, lid_y), pos2(center.x + r * 0.56, lid_y)], stroke);
            painter.line_segment([pos2(center.x - r * 0.22, lid_y - r * 0.22), pos2(center.x + r * 0.22, lid_y - r * 0.22)], stroke);
            let box_rect = Rect::from_min_max(
                pos2(center.x - r * 0.42, lid_y + r * 0.14),
                pos2(center.x + r * 0.42, center.y + r * 0.68),
            );
            painter.rect_stroke(box_rect, 2.0, stroke, egui::StrokeKind::Inside);
        }

        IconKind::OpenApp => {
            let box_rect = Rect::from_center_size(pos2(center.x - r * 0.08, center.y + r * 0.08), vec2(s * 0.64, s * 0.64));
            painter.rect_stroke(box_rect, 2.2, stroke, egui::StrokeKind::Inside);
            let tip = pos2(center.x + r * 0.62, center.y - r * 0.62);
            painter.line_segment([center, tip], stroke);
            painter.line_segment([tip, pos2(tip.x - r * 0.4, tip.y)], stroke);
            painter.line_segment([tip, pos2(tip.x, tip.y + r * 0.4)], stroke);
        }

        IconKind::Star | IconKind::StarOutline => {
            let mut pts = Vec::with_capacity(10);
            for i in 0..10 {
                let angle = -PI * 0.5 + (i as f32) * (PI / 5.0);
                let rad = if i % 2 == 0 { r * 0.78 } else { r * 0.34 };
                pts.push(pos2(center.x + angle.cos() * rad, center.y + angle.sin() * rad));
            }
            if matches!(kind, IconKind::Star) {
                painter.add(Shape::Path(egui::epaint::PathShape::closed_line(pts.clone(), stroke)));
                // Fill inner star triangles from center
                for i in 0..10 {
                    let a = pts[i];
                    let b = pts[(i + 1) % 10];
                    painter.add(Shape::convex_polygon(vec![center, a, b], color, Stroke::NONE));
                }
            } else {
                painter.add(Shape::Path(egui::epaint::PathShape::closed_line(pts, stroke)));
            }
        }

        IconKind::Terminal => {
            let box_rect = Rect::from_center_size(center, vec2(s * 0.88, s * 0.72));
            painter.rect_stroke(box_rect, 2.5, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [pos2(center.x - r * 0.44, center.y - r * 0.22), pos2(center.x - r * 0.14, center.y)],
                stroke,
            );
            painter.line_segment(
                [pos2(center.x - r * 0.44, center.y + r * 0.22), pos2(center.x - r * 0.14, center.y)],
                stroke,
            );
            painter.line_segment(
                [pos2(center.x + r * 0.04, center.y + r * 0.22), pos2(center.x + r * 0.44, center.y + r * 0.22)],
                stroke,
            );
        }

        IconKind::Calculator => {
            let box_rect = Rect::from_center_size(center, vec2(s * 0.76, s * 0.90));
            painter.rect_stroke(box_rect, 2.5, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [pos2(center.x - r * 0.28, center.y - r * 0.28), pos2(center.x + r * 0.28, center.y - r * 0.28)],
                stroke,
            );
            painter.line_segment(
                [pos2(center.x - r * 0.24, center.y + r * 0.14), pos2(center.x + r * 0.24, center.y + r * 0.14)],
                stroke,
            );
            painter.line_segment(
                [pos2(center.x - r * 0.24, center.y + r * 0.42), pos2(center.x + r * 0.24, center.y + r * 0.42)],
                stroke,
            );
        }

        IconKind::ChevronLeft => {
            let d = r * 0.48;
            painter.line_segment([pos2(center.x + d * 0.5, center.y - d), pos2(center.x - d * 0.5, center.y)], stroke);
            painter.line_segment([pos2(center.x - d * 0.5, center.y), pos2(center.x + d * 0.5, center.y + d)], stroke);
        }

        IconKind::ChevronRight => {
            let d = r * 0.48;
            painter.line_segment([pos2(center.x - d * 0.5, center.y - d), pos2(center.x + d * 0.5, center.y)], stroke);
            painter.line_segment([pos2(center.x + d * 0.5, center.y), pos2(center.x - d * 0.5, center.y + d)], stroke);
        }

        IconKind::Camera => {
            // Minimalist camera body + top viewfinder + center lens
            let body = Rect::from_center_size(pos2(center.x, center.y + r * 0.1), vec2(s * 0.84, s * 0.58));
            painter.rect_stroke(body, (s * 0.14).max(2.0), stroke, egui::StrokeKind::Inside);
            let bump = Rect::from_center_size(pos2(center.x, center.y - r * 0.44), vec2(s * 0.34, s * 0.16));
            painter.rect_filled(bump, 1.5, color);
            painter.circle_stroke(pos2(center.x, center.y + r * 0.1), r * 0.27, stroke);
        }

        IconKind::VideoRecord => {
            // Camcorder rectangle + lens triangle
            let body = Rect::from_center_size(pos2(center.x - r * 0.18, center.y), vec2(s * 0.54, s * 0.52));
            painter.rect_stroke(body, (s * 0.12).max(2.0), stroke, egui::StrokeKind::Inside);
            let tip_x = center.x + r * 0.34;
            let far_x = center.x + r * 0.78;
            let tri = vec![
                pos2(tip_x, center.y),
                pos2(far_x, center.y - r * 0.36),
                pos2(far_x, center.y + r * 0.36),
            ];
            painter.add(Shape::convex_polygon(tri, color, Stroke::NONE));
        }

        IconKind::StopRecord => {
            let sq = Rect::from_center_size(center, vec2(s * 0.58, s * 0.58));
            painter.rect_filled(sq, (s * 0.12).max(2.0), color);
        }

        IconKind::Cursor => {
            let p0 = pos2(center.x - r * 0.42, center.y - r * 0.68);
            let p1 = pos2(center.x - r * 0.42, center.y + r * 0.56);
            let p2 = pos2(center.x - r * 0.05, center.y + r * 0.24);
            let p3 = pos2(center.x + r * 0.46, center.y + r * 0.24);
            painter.add(Shape::convex_polygon(vec![p0, p1, p2, p3], color, Stroke::NONE));
        }

        IconKind::Mic | IconKind::MicMute => {
            let cap = Rect::from_center_size(pos2(center.x, center.y - r * 0.22), vec2(s * 0.32, s * 0.52));
            painter.rect_stroke(cap, s * 0.16, stroke, egui::StrokeKind::Inside);
            draw_arc(painter, pos2(center.x, center.y - r * 0.1), r * 0.42, 0.15, PI - 0.15, stroke);
            painter.line_segment(
                [pos2(center.x, center.y + r * 0.32), pos2(center.x, center.y + r * 0.72)],
                stroke,
            );
            painter.line_segment(
                [pos2(center.x - r * 0.28, center.y + r * 0.72), pos2(center.x + r * 0.28, center.y + r * 0.72)],
                stroke,
            );
            if matches!(kind, IconKind::MicMute) {
                painter.line_segment(
                    [pos2(center.x - r * 0.65, center.y - r * 0.65), pos2(center.x + r * 0.65, center.y + r * 0.65)],
                    stroke,
                );
            }
        }
    }
}

fn draw_arc(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    start_angle: f32,
    end_angle: f32,
    stroke: Stroke,
) {
    let steps = 10;
    let pts: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let a = start_angle + (end_angle - start_angle) * t;
            pos2(center.x + a.cos() * radius, center.y + a.sin() * radius)
        })
        .collect();
    painter.add(Shape::line(pts, stroke));
}
