use super::Projection;
use crate::model::Anchor;
use crate::model::alert::rectangle::{
    RectangleAlert, RectangleCheckpoint, RectangleMode, RectanglePhase,
};
use crate::ui::icons::{self, Icon};
use crate::ui::theme;
use eframe::egui::{self, Align2, FontId, Pos2, Rect, Stroke, pos2};

const DASH: f32 = 7.0;
const GAP: f32 = 5.0;

#[derive(Clone, Copy)]
enum ZoneLabel {
    Live(RectanglePhase),
    NotWatching,
    Unsupported,
}

pub(super) fn paint(
    painter: &egui::Painter,
    projection: &Projection<'_>,
    alerts: &[RectangleAlert],
    checkpoints: &[RectangleCheckpoint],
    muted: &std::collections::HashSet<u64>,
    top_inset: f32,
) {
    let painter = painter.with_clip_rect(projection.plot);
    let now_ms = super::current_time_ms();
    for alert in alerts {
        let supported = alert.mode == RectangleMode::LiveTouch;
        let stored_phase = checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id == alert.id)
            .map(|checkpoint| checkpoint.phase);
        let phase = match stored_phase {
            Some(phase @ (RectanglePhase::Triggered | RectanglePhase::Expired)) => phase,
            _ if now_ms >= alert.end_ms => RectanglePhase::Expired,
            _ if alert.paused => RectanglePhase::Paused,
            _ if now_ms < alert.start_ms => RectanglePhase::Scheduled,
            Some(RectanglePhase::Paused) => RectanglePhase::Paused,
            _ => RectanglePhase::Active,
        };
        let not_watching = supported
            && muted.contains(&alert.id)
            && matches!(
                phase,
                RectanglePhase::Scheduled | RectanglePhase::Active | RectanglePhase::Paused
            );
        let a = projection.point(Anchor {
            time_ms: alert.start_ms,
            price: alert.high,
        });
        let b = projection.point(Anchor {
            time_ms: alert.end_ms,
            price: alert.low,
        });
        if !a.x.is_finite() || !a.y.is_finite() || !b.x.is_finite() || !b.y.is_finite() {
            continue;
        }
        let rect = Rect::from_two_pos(a, b).intersect(projection.plot);
        if !rect.is_positive() {
            continue;
        }
        let opacity = if !supported {
            0.30
        } else if not_watching {
            0.34
        } else {
            match phase {
                RectanglePhase::Scheduled => 0.62,
                RectanglePhase::Active => 0.82,
                RectanglePhase::Paused => 0.46,
                RectanglePhase::Triggered | RectanglePhase::Expired => 0.32,
            }
        };
        let fill = if !supported || not_watching {
            0.02
        } else {
            match phase {
                RectanglePhase::Active => 0.07,
                RectanglePhase::Scheduled | RectanglePhase::Paused => 0.04,
                RectanglePhase::Triggered | RectanglePhase::Expired => 0.025,
            }
        };
        let color = theme::with_alpha(theme::text_secondary(), opacity);
        painter.rect_filled(rect, 0.0, theme::with_alpha(color, fill));
        dashed(&painter, rect.left_top(), rect.right_top(), color);
        dashed(&painter, rect.right_top(), rect.right_bottom(), color);
        dashed(&painter, rect.right_bottom(), rect.left_bottom(), color);
        dashed(&painter, rect.left_bottom(), rect.left_top(), color);
        let label_state = if !supported {
            ZoneLabel::Unsupported
        } else if not_watching {
            ZoneLabel::NotWatching
        } else {
            ZoneLabel::Live(phase)
        };
        label(
            &painter,
            rect,
            projection.plot,
            top_inset,
            color,
            label_state,
        );
    }
}

fn dashed(painter: &egui::Painter, start: Pos2, end: Pos2, color: egui::Color32) {
    let delta = end - start;
    let length = delta.length();
    if !length.is_finite() || length <= 0.0 {
        return;
    }
    let direction = delta / length;
    let step = DASH + GAP;
    let mut offset = 0.0;
    while offset < length {
        let finish = (offset + DASH).min(length);
        painter.line_segment(
            [start + direction * offset, start + direction * finish],
            Stroke::new(1.25, color),
        );
        offset += step;
    }
}

fn label(
    painter: &egui::Painter,
    rect: Rect,
    plot: Rect,
    top_inset: f32,
    color: egui::Color32,
    label: ZoneLabel,
) {
    let y = (rect.top() + 10.0).max(plot.top() + top_inset + 8.0);
    if rect.width() < 82.0 || y + 8.0 > rect.bottom() {
        return;
    }
    icons::paint(
        painter,
        Rect::from_center_size(pos2(rect.left() + 9.0, y), egui::vec2(12.0, 12.0)),
        Icon::Bell,
        color,
    );
    painter.text(
        pos2(rect.left() + 18.0, y),
        Align2::LEFT_CENTER,
        match label {
            ZoneLabel::Unsupported => "Unsupported",
            ZoneLabel::NotWatching => "Not watching",
            ZoneLabel::Live(phase) => match phase {
                RectanglePhase::Scheduled => "Scheduled",
                RectanglePhase::Active => "Live touch",
                RectanglePhase::Paused => "Paused",
                RectanglePhase::Triggered => "Triggered",
                RectanglePhase::Expired => "Expired",
            },
        },
        FontId::proportional(11.0),
        color,
    );
}
