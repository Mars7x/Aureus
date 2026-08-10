use std::cell::RefCell;
use std::f64::consts::{FRAC_PI_2, PI};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{DrawingArea, GestureClick};

#[derive(Clone, Debug)]
pub struct AllocationSlice {
    pub key: String,
    pub label: String,
    pub value: f64,
    pub color_index: usize,
    pub color: Option<(f64, f64, f64)>,
    pub is_cash: bool,
}

#[derive(Clone)]
pub struct AllocationRing {
    area: DrawingArea,
    state: Rc<RefCell<RingState>>,
}

#[derive(Clone, Default)]
struct RingState {
    slices: Vec<AllocationSlice>,
    currency: String,
    selected_index: Option<usize>,
    previous_selection: Option<usize>,
    transition_progress: f64,
    transition_generation: u64,
}

impl AllocationRing {
    pub fn new() -> Self {
        let area = DrawingArea::builder()
            // Keep a deliberately small minimum so the ring cannot hold the
            // entire Overview wider while the sidebar is visible. It still
            // expands normally and draws from its actual allocated width.
            .width_request(96)
            .height_request(190)
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .build();
        area.set_accessible_role(gtk::AccessibleRole::Img);

        let state = Rc::new(RefCell::new(RingState {
            slices: Vec::new(),
            currency: "CAD".into(),
            selected_index: None,
            previous_selection: None,
            transition_progress: 1.0,
            transition_generation: 0,
        }));

        {
            let state = state.clone();
            area.set_draw_func(move |area, context, width, height| {
                draw_ring(area, context, width, height, &state.borrow());
            });
        }

        let click = GestureClick::new();
        {
            let area = area.clone();
            let state = state.clone();
            click.connect_pressed(move |_, _, x, y| {
                let hit = {
                    let current = state.borrow();
                    slice_at_point(
                        &current,
                        f64::from(area.width()),
                        f64::from(area.height()),
                        x,
                        y,
                    )
                };
                let target = {
                    let current = state.borrow();
                    if hit.is_some() && hit == current.selected_index {
                        None
                    } else {
                        hit
                    }
                };
                begin_selection_transition(&area, &state, target);
            });
        }
        area.add_controller(click);

        let manager = adw::StyleManager::for_display(&area.display());
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

    pub fn set_slices(&self, slices: Vec<AllocationSlice>, currency: &str) {
        let mut state = self.state.borrow_mut();
        state.slices = slices;
        state.currency = currency.to_string();
        state.selected_index = None;
        state.previous_selection = None;
        state.transition_progress = 1.0;
        state.transition_generation = state.transition_generation.wrapping_add(1);
        drop(state);
        self.area.queue_draw();
    }

    pub fn toggle_index(&self, index: usize) {
        let target = {
            let state = self.state.borrow();
            if index >= state.slices.len() {
                return;
            }
            if state.selected_index == Some(index) {
                None
            } else {
                Some(index)
            }
        };
        begin_selection_transition(&self.area, &self.state, target);
    }
}

pub fn allocation_color(
    color_index: usize,
    color: Option<(f64, f64, f64)>,
    is_cash: bool,
    dark: bool,
) -> (f64, f64, f64) {
    if let Some(color) = color {
        return color;
    }
    if is_cash {
        return if dark {
            (0.58, 0.61, 0.64)
        } else {
            (0.43, 0.46, 0.49)
        };
    }
    // Visible securities get palette slots instead of hashing their ticker.
    // That guarantees two simultaneously-visible assets never share a color.
    // The golden-ratio step keeps neighbouring slots well separated.
    let hue = (0.07 + color_index as f64 * 0.618_033_988_75).fract();
    let saturation = if dark { 0.68 } else { 0.64 };
    let lightness = if dark { 0.61 } else { 0.47 };
    hsl_to_rgb(hue, saturation, lightness)
}

fn hsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> (f64, f64, f64) {
    if saturation <= f64::EPSILON {
        return (lightness, lightness, lightness);
    }
    let q = if lightness < 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    let p = 2.0 * lightness - q;
    (
        hue_channel(p, q, hue + 1.0 / 3.0),
        hue_channel(p, q, hue),
        hue_channel(p, q, hue - 1.0 / 3.0),
    )
}

fn hue_channel(p: f64, q: f64, mut t: f64) -> f64 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 0.5 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
}

fn draw_ring(
    area: &DrawingArea,
    context: &gtk::cairo::Context,
    width: i32,
    height: i32,
    state: &RingState,
) {
    let width = f64::from(width.max(1));
    let height = f64::from(height.max(1));
    let center_x = width / 2.0;
    let center_y = height / 2.0;
    let radius = width.min(height) * 0.34;
    // Keep the allocation donut visually light while preserving its overall diameter.
    let line_width = (width.min(height) * 0.095).clamp(16.0, 22.0);
    // Text must live inside the donut hole even when the ring is compressed.
    // Keep a small inset from the inner stroke and scale typography against
    // this actual available diameter rather than assuming a desktop size.
    let inner_radius = (radius - line_width / 2.0 - 6.0).max(14.0);
    let center_text_width = inner_radius * 2.0;
    let manager = adw::StyleManager::for_display(&area.display());
    let dark = manager.is_dark();
    let foreground = if dark { 0.94 } else { 0.12 };
    let subdued = if dark { 0.55 } else { 0.45 };

    context.set_line_width(line_width);
    context.set_line_cap(gtk::cairo::LineCap::Butt);
    context.set_source_rgba(subdued, subdued, subdued, if dark { 0.18 } else { 0.12 });
    context.arc(center_x, center_y, radius, 0.0, PI * 2.0);
    let _ = context.stroke();

    let total: f64 = state
        .slices
        .iter()
        .map(|slice| slice.value.max(0.0))
        .sum();
    if total <= f64::EPSILON {
        draw_center_text(
            context,
            center_x,
            center_y,
            center_text_width,
            "No allocation",
            "",
            "",
            foreground,
            subdued,
            1.0,
        );
        return;
    }

    let mut angle = -FRAC_PI_2;
    for (index, slice) in state.slices.iter().enumerate() {
        let value = slice.value.max(0.0);
        if value <= f64::EPSILON {
            continue;
        }
        let sweep = (value / total) * PI * 2.0;
        let (red, green, blue) =
            allocation_color(slice.color_index, slice.color, slice.is_cash, dark);
        let alpha = if state.selected_index.is_some() && state.selected_index != Some(index) {
            0.46
        } else {
            0.96
        };
        // Give the active slice a tiny radial lift. During a selection change
        // the previous slice eases back while the new one eases outward, so
        // the emphasis follows the same 160 ms transition as the center text.
        let progress = state.transition_progress.clamp(0.0, 1.0);
        let emphasis = if state.selected_index == Some(index) {
            progress
        } else if state.previous_selection == Some(index)
            && state.previous_selection != state.selected_index
        {
            1.0 - progress
        } else {
            0.0
        };
        let slice_radius = radius + emphasis * 2.5;
        context.set_source_rgba(red, green, blue, alpha);
        context.arc(center_x, center_y, slice_radius, angle, angle + sweep);
        let _ = context.stroke();
        angle += sweep;
    }

    let progress = state.transition_progress.clamp(0.0, 1.0);
    if progress < 1.0 && state.previous_selection != state.selected_index {
        draw_center_content(
            context,
            center_x,
            center_y,
            state,
            state.previous_selection,
            total,
            foreground,
            subdued,
            1.0 - progress,
            center_text_width,
        );
        draw_center_content(
            context,
            center_x,
            center_y,
            state,
            state.selected_index,
            total,
            foreground,
            subdued,
            progress,
            center_text_width,
        );
    } else {
        draw_center_content(
            context,
            center_x,
            center_y,
            state,
            state.selected_index,
            total,
            foreground,
            subdued,
            1.0,
            center_text_width,
        );
    }
}

fn begin_selection_transition(
    area: &DrawingArea,
    state: &Rc<RefCell<RingState>>,
    target: Option<usize>,
) {
    let generation = {
        let mut current = state.borrow_mut();
        if current.selected_index == target && current.transition_progress >= 1.0 {
            return;
        }
        current.previous_selection = current.selected_index;
        current.selected_index = target;
        current.transition_progress = 0.0;
        current.transition_generation = current.transition_generation.wrapping_add(1);
        current.transition_generation
    };

    area.queue_draw();
    let area = area.clone();
    let state = state.clone();
    let started = Instant::now();
    gtk::glib::timeout_add_local(Duration::from_millis(16), move || {
        let elapsed = started.elapsed().as_secs_f64();
        let linear = (elapsed / 0.16).clamp(0.0, 1.0);
        let eased = linear * linear * (3.0 - 2.0 * linear);
        {
            let mut current = state.borrow_mut();
            if current.transition_generation != generation {
                return gtk::glib::ControlFlow::Break;
            }
            current.transition_progress = eased;
            if linear >= 1.0 {
                current.previous_selection = current.selected_index;
            }
        }
        area.queue_draw();
        if linear >= 1.0 {
            gtk::glib::ControlFlow::Break
        } else {
            gtk::glib::ControlFlow::Continue
        }
    });
}

fn draw_center_content(
    context: &gtk::cairo::Context,
    center_x: f64,
    center_y: f64,
    state: &RingState,
    selection: Option<usize>,
    total: f64,
    foreground: f64,
    subdued: f64,
    alpha: f64,
    max_width: f64,
) {
    if alpha <= f64::EPSILON {
        return;
    }
    if let Some(index) = selection.filter(|index| *index < state.slices.len()) {
        let slice = &state.slices[index];
        let percent = slice.value.max(0.0) / total * 100.0;
        draw_center_text(
            context,
            center_x,
            center_y,
            max_width,
            &slice.label,
            &format!("{percent:.1}%"),
            &format_currency(slice.value, &state.currency),
            foreground,
            subdued,
            alpha,
        );
    } else {
        draw_center_text(
            context,
            center_x,
            center_y,
            max_width,
            "Portfolio",
            &format_currency(total, &state.currency),
            "",
            foreground,
            subdued,
            alpha,
        );
    }
}

fn fitted_font_size(text: &str, max_width: f64, preferred: f64, minimum: f64, factor: f64) -> f64 {
    if text.is_empty() {
        return preferred;
    }
    let estimated = text.chars().count() as f64 * preferred * factor;
    if estimated <= max_width || estimated <= f64::EPSILON {
        preferred
    } else {
        (preferred * max_width / estimated).clamp(minimum, preferred)
    }
}

fn estimated_text_width(text: &str, font_size: f64, factor: f64) -> f64 {
    text.chars().count() as f64 * font_size * factor
}

fn draw_center_line(
    context: &gtk::cairo::Context,
    center_x: f64,
    baseline_y: f64,
    max_width: f64,
    text: &str,
    preferred_size: f64,
    minimum_size: f64,
    width_factor: f64,
    weight: gtk::cairo::FontWeight,
    tone: f64,
    alpha: f64,
) {
    if text.is_empty() {
        return;
    }
    let font_size = fitted_font_size(text, max_width, preferred_size, minimum_size, width_factor);
    context.select_font_face("Sans", gtk::cairo::FontSlant::Normal, weight);
    context.set_font_size(font_size);
    context.set_source_rgba(tone, tone, tone, alpha);
    let estimated = estimated_text_width(text, font_size, width_factor);
    context.move_to(center_x - estimated / 2.0, baseline_y);
    let _ = context.show_text(text);
}

fn draw_center_text(
    context: &gtk::cairo::Context,
    center_x: f64,
    center_y: f64,
    max_width: f64,
    primary: &str,
    secondary: &str,
    tertiary: &str,
    foreground: f64,
    subdued: f64,
    alpha: f64,
) {
    // Clip as a final safety net: even unusually long currency/ticker strings
    // can never paint outside the donut hole. Typography also scales down to
    // fit, so clipping should only matter at extremely small transient sizes.
    let clip_radius = (max_width / 2.0).max(12.0);
    let _ = context.save();
    context.arc(center_x, center_y, clip_radius, 0.0, PI * 2.0);
    context.clip();

    if tertiary.is_empty() {
        draw_center_line(
            context, center_x, center_y - 3.0, max_width, primary, 13.5, 7.0, 0.57,
            gtk::cairo::FontWeight::Bold, foreground, 0.94 * alpha,
        );
        draw_center_line(
            context, center_x, center_y + 16.0, max_width, secondary, 10.5, 6.0, 0.54,
            gtk::cairo::FontWeight::Normal, subdued, 0.95 * alpha,
        );
    } else {
        draw_center_line(
            context, center_x, center_y - 13.0, max_width, primary, 13.5, 7.0, 0.57,
            gtk::cairo::FontWeight::Bold, foreground, 0.94 * alpha,
        );
        draw_center_line(
            context, center_x, center_y + 3.0, max_width, secondary, 10.0, 6.0, 0.54,
            gtk::cairo::FontWeight::Normal, subdued, 0.95 * alpha,
        );
        draw_center_line(
            context, center_x, center_y + 17.0, max_width, tertiary, 9.5, 5.5, 0.54,
            gtk::cairo::FontWeight::Normal, subdued, 0.95 * alpha,
        );
    }

    let _ = context.restore();
}

fn slice_at_point(state: &RingState, width: f64, height: f64, x: f64, y: f64) -> Option<usize> {
    let center_x = width / 2.0;
    let center_y = height / 2.0;
    let radius = width.min(height) * 0.34;
    // Keep the larger historical hit target so the thinner visual ring is still easy to click.
    let line_width = (width.min(height) * 0.13).clamp(22.0, 30.0);
    let dx = x - center_x;
    let dy = y - center_y;
    let distance = (dx * dx + dy * dy).sqrt();
    if distance < radius - line_width / 2.0 || distance > radius + line_width / 2.0 {
        return None;
    }

    let total: f64 = state
        .slices
        .iter()
        .map(|slice| slice.value.max(0.0))
        .sum();
    if total <= f64::EPSILON {
        return None;
    }

    let mut angle = dy.atan2(dx) + FRAC_PI_2;
    if angle < 0.0 {
        angle += PI * 2.0;
    }
    let target = angle / (PI * 2.0);
    let mut cumulative = 0.0;
    for (index, slice) in state.slices.iter().enumerate() {
        cumulative += slice.value.max(0.0) / total;
        if target <= cumulative || index + 1 == state.slices.len() {
            return Some(index);
        }
    }
    None
}

fn format_money_number(value: f64) -> String {
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let raw = format!("{:.2}", value.abs());
    let (whole, fraction) = raw.split_once('.').unwrap_or((raw.as_str(), "00"));
    if whole.len() < 5 {
        return format!("{sign}{raw}");
    }

    let mut grouped = String::with_capacity(whole.len() + whole.len() / 3);
    let first = whole.len() % 3;
    if first > 0 {
        grouped.push_str(&whole[..first]);
        if first < whole.len() {
            grouped.push(',');
        }
    }
    for (index, chunk) in whole[first..].as_bytes().chunks(3).enumerate() {
        if index > 0 {
            grouped.push(',');
        }
        grouped.push_str(std::str::from_utf8(chunk).unwrap_or_default());
    }
    format!("{sign}{grouped}.{fraction}")
}

fn format_currency(value: f64, currency: &str) -> String {
    let number = format_money_number(value);
    match currency {
        "CAD" => format!("C${number}"),
        "USD" => format!("US${number}"),
        "EUR" => format!("€{number}"),
        "GBP" => format!("£{number}"),
        _ => format!("{number} {currency}"),
    }
}
