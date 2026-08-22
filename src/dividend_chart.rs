use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{DrawingArea, EventControllerMotion, GestureClick};

#[derive(Clone)]
pub struct DividendChart {
    area: DrawingArea,
    state: Rc<RefCell<ChartState>>,
}

#[derive(Clone, Default)]
struct ChartState {
    values: Vec<(String, f64, bool)>,
    currency: String,
    inspect_index: Option<usize>,
    message: Option<String>,
}

impl DividendChart {
    pub fn new() -> Self {
        let area = DrawingArea::builder()
            .height_request(220)
            .hexpand(true)
            .vexpand(false)
            .build();
        area.set_accessible_role(gtk::AccessibleRole::Img);

        let state = Rc::new(RefCell::new(ChartState {
            values: Vec::new(),
            currency: "USD".into(),
            inspect_index: None,
            message: Some("Loading dividend history".into()),
        }));

        {
            let state = state.clone();
            area.set_draw_func(move |area, context, width, height| {
                draw_chart(area, context, width, height, &state.borrow());
            });
        }

        let motion = EventControllerMotion::new();
        {
            let area = area.clone();
            let state = state.clone();
            motion.connect_motion(move |_, x, _| {
                let next = {
                    let state = state.borrow();
                    dividend_index_at_x(state.values.len(), f64::from(area.width().max(1)), x)
                };
                let mut state = state.borrow_mut();
                if state.inspect_index != next {
                    state.inspect_index = next;
                    drop(state);
                    area.queue_draw();
                }
            });
        }
        {
            let area = area.clone();
            let state = state.clone();
            motion.connect_leave(move |_| {
                let mut state = state.borrow_mut();
                if state.inspect_index.take().is_some() {
                    drop(state);
                    area.queue_draw();
                }
            });
        }
        area.add_controller(motion);

        let click = GestureClick::new();
        {
            let area = area.clone();
            let state = state.clone();
            click.connect_pressed(move |_, _, x, _| {
                let next = {
                    let state = state.borrow();
                    dividend_index_at_x(state.values.len(), f64::from(area.width().max(1)), x)
                };
                let mut state = state.borrow_mut();
                if state.inspect_index != next {
                    state.inspect_index = next;
                    drop(state);
                    area.queue_draw();
                }
            });
        }
        area.add_controller(click);

        let manager = adw::StyleManager::for_display(&area.display());
        {
            let area = area.clone();
            manager.connect_accent_color_rgba_notify(move |_| area.queue_draw());
        }
        {
            let area = area.clone();
            manager.connect_dark_notify(move |_| area.queue_draw());
        }
        {
            let area = area.clone();
            manager.connect_high_contrast_notify(move |_| area.queue_draw());
        }

        Self { area, state }
    }

    pub fn widget(&self) -> &DrawingArea {
        &self.area
    }

    pub fn set_values(&self, values: Vec<(String, f64, bool)>, currency: &str) {
        let mut state = self.state.borrow_mut();
        state.values = values;
        state.currency = currency.to_string();
        state.inspect_index = None;
        state.message = None;
        drop(state);
        self.area.queue_draw();
    }

    pub fn set_message(&self, message: impl Into<String>) {
        let mut state = self.state.borrow_mut();
        state.message = Some(message.into());
        state.inspect_index = None;
        drop(state);
        self.area.queue_draw();
    }
}

fn dividend_index_at_x(count: usize, width: f64, x: f64) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let plot_left = 12.0;
    let plot_right = (width - 12.0).max(plot_left + 1.0);
    let fraction = ((x - plot_left) / (plot_right - plot_left)).clamp(0.0, 0.999_999);
    Some(((fraction * count as f64).floor() as usize).min(count - 1))
}

fn draw_chart(
    area: &DrawingArea,
    context: &gtk::cairo::Context,
    width: i32,
    height: i32,
    state: &ChartState,
) {
    let width = f64::from(width.max(1));
    let height = f64::from(height.max(1));
    let manager = adw::StyleManager::for_display(&area.display());
    let accent = manager.accent_color_rgba();
    let dark = manager.is_dark();
    let foreground = if dark { 0.93 } else { 0.12 };

    if let Some(message) = state.message.as_deref() {
        context.set_source_rgba(foreground, foreground, foreground, 0.72);
        context.select_font_face(
            "Sans",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Normal,
        );
        context.set_font_size(14.0);
        context.move_to(18.0, height / 2.0);
        let _ = context.show_text(message);
        return;
    }

    if state.values.is_empty() {
        return;
    }

    let plot_left = 12.0;
    let plot_right = (width - 12.0).max(plot_left + 1.0);
    let plot_top = 14.0;
    let plot_bottom = (height - 30.0).max(plot_top + 1.0);
    let max_value = state
        .values
        .iter()
        .map(|(_, value, _)| *value)
        .fold(0.0_f64, f64::max);
    let scale_max = if max_value <= f64::EPSILON {
        1.0
    } else {
        max_value * 1.08
    };

    let count = state.values.len();
    let slot = (plot_right - plot_left) / count as f64;
    let bar_width = (slot * 0.58).clamp(5.0, 34.0);

    for (index, (_, value, estimated)) in state.values.iter().enumerate() {
        let center = plot_left + slot * (index as f64 + 0.5);
        let bar_height = (*value / scale_max) * (plot_bottom - plot_top);
        let x = center - bar_width / 2.0;
        let y = plot_bottom - bar_height;
        context.rectangle(x, y, bar_width, bar_height.max(1.0));
        // Received/confirmed income stays at the normal accent strength.
        // Future estimates use the same accent at lower opacity so the chart
        // communicates certainty without inventing a second dividend color.
        let alpha = match (state.inspect_index == Some(index), *estimated) {
            (true, true) => 0.64,
            (false, true) => 0.44,
            (true, false) => 1.0,
            (false, false) => 0.90,
        };
        context.set_source_rgba(
            f64::from(accent.red()),
            f64::from(accent.green()),
            f64::from(accent.blue()),
            alpha,
        );
        let _ = context.fill();
    }

    context.select_font_face(
        "Sans",
        gtk::cairo::FontSlant::Normal,
        gtk::cairo::FontWeight::Normal,
    );
    // Calendar-year views always show every month label. Scale the label text
    // slightly at narrow widths rather than dropping alternating months, so the
    // x-axis remains complete on phones as well as desktop.
    let label_size = if count >= 12 {
        (slot * 0.42).clamp(7.5, 10.5)
    } else {
        10.5
    };
    context.set_font_size(label_size);
    context.set_source_rgba(foreground, foreground, foreground, 0.58);
    for (index, (label, _, _)) in state.values.iter().enumerate() {
        let center = plot_left + slot * (index as f64 + 0.5);
        let x = context
            .text_extents(label)
            .map(|extents| center - extents.width() / 2.0 - extents.x_bearing())
            .unwrap_or(center);
        context.move_to(x.max(1.0), height - 8.0);
        let _ = context.show_text(label);
    }

    let Some(index) = state.inspect_index.filter(|index| *index < count) else {
        return;
    };
    let (label, value, estimated) = &state.values[index];
    let center = plot_left + slot * (index as f64 + 0.5);
    let bar_height = (*value / scale_max) * (plot_bottom - plot_top);
    let y = plot_bottom - bar_height;
    let text = if *estimated {
        format!("{} · {} · estimated", label, format_currency(*value, &state.currency))
    } else {
        format!("{} · {}", label, format_currency(*value, &state.currency))
    };
    let popup_width = (text.chars().count() as f64 * 7.0 + 20.0).clamp(110.0, 210.0);
    let popup_height = 32.0;
    let popup_x = (center - popup_width / 2.0)
        .max(4.0)
        .min((width - popup_width - 4.0).max(4.0));
    let popup_y = (y - popup_height - 8.0)
        .max(4.0)
        .min((height - popup_height - 4.0).max(4.0));

    if dark {
        context.set_source_rgba(32.0 / 255.0, 32.0 / 255.0, 32.0 / 255.0, 0.97);
    } else {
        context.set_source_rgba(1.0, 1.0, 1.0, 0.97);
    }
    rounded_rectangle(context, popup_x, popup_y, popup_width, popup_height, 8.0);
    let _ = context.fill();
    context.set_source_rgba(foreground, foreground, foreground, 0.95);
    context.set_font_size(11.5);
    context.move_to(popup_x + 10.0, popup_y + 20.0);
    let _ = context.show_text(&text);
}

fn rounded_rectangle(
    context: &gtk::cairo::Context,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    radius: f64,
) {
    let right = x + width;
    let bottom = y + height;
    context.new_sub_path();
    context.arc(
        right - radius,
        y + radius,
        radius,
        -std::f64::consts::FRAC_PI_2,
        0.0,
    );
    context.arc(
        right - radius,
        bottom - radius,
        radius,
        0.0,
        std::f64::consts::FRAC_PI_2,
    );
    context.arc(
        x + radius,
        bottom - radius,
        radius,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    context.arc(
        x + radius,
        y + radius,
        radius,
        std::f64::consts::PI,
        std::f64::consts::PI * 1.5,
    );
    context.close_path();
}

fn format_currency(value: f64, currency: &str) -> String {
    crate::currency::format_value(value, currency)
}

#[cfg(test)]
mod tests {
    use super::dividend_index_at_x;

    #[test]
    fn pointer_lookup_maps_to_bar_slots() {
        assert_eq!(dividend_index_at_x(4, 124.0, 12.0), Some(0));
        assert_eq!(dividend_index_at_x(4, 124.0, 62.0), Some(2));
        assert_eq!(dividend_index_at_x(4, 124.0, 112.0), Some(3));
    }
}
