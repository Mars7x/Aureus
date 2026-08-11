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
    pub display_percent: f64,
    pub color_index: usize,
    pub color: Option<(f64, f64, f64)>,
    pub is_cash: bool,
}

type InteractionCallback = Rc<dyn Fn(Option<usize>)>;

#[derive(Clone)]
pub struct AllocationRing {
    area: DrawingArea,
    state: Rc<RefCell<RingState>>,
    interaction_callback: Rc<RefCell<Option<InteractionCallback>>>,
}

#[derive(Clone, Default)]
struct RingState {
    slices: Vec<AllocationSlice>,
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
            selected_index: None,
            previous_selection: None,
            transition_progress: 1.0,
            transition_generation: 0,
        }));
        let interaction_callback = Rc::new(RefCell::new(None));

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
            let interaction_callback = interaction_callback.clone();
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
                begin_selection_transition(&area, &state, &interaction_callback, target);
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

        Self {
            area,
            state,
            interaction_callback,
        }
    }

    pub fn widget(&self) -> &DrawingArea {
        &self.area
    }

    pub fn set_slices(&self, slices: Vec<AllocationSlice>, _currency: &str) {
        let mut state = self.state.borrow_mut();
        state.slices = slices;
        state.selected_index = None;
        state.previous_selection = None;
        state.transition_progress = 1.0;
        state.transition_generation = state.transition_generation.wrapping_add(1);
        drop(state);
        self.area.queue_draw();
        notify_interaction(&self.interaction_callback, &self.state);
    }

    pub fn set_interaction_callback<F>(&self, callback: F)
    where
        F: Fn(Option<usize>) + 'static,
    {
        *self.interaction_callback.borrow_mut() = Some(Rc::new(callback));
        notify_interaction(&self.interaction_callback, &self.state);
    }

    pub fn clear_interaction_callback(&self) {
        self.interaction_callback.borrow_mut().take();
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
        begin_selection_transition(
            &self.area,
            &self.state,
            &self.interaction_callback,
            target,
        );
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
    let line_width = (width.min(height) * 0.065).clamp(11.0, 15.0);
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

    let total: f64 = state
        .slices
        .iter()
        .map(|slice| slice.value.max(0.0))
        .sum();
    if total <= f64::EPSILON {
        // Only show the muted donut as an empty-state track. When allocation
        // slices exist, drawing this track underneath them becomes visible as
        // a gray inner crescent while the selected slice lifts outward.
        context.set_source_rgba(subdued, subdued, subdued, if dark { 0.18 } else { 0.12 });
        context.arc(center_x, center_y, radius, 0.0, PI * 2.0);
        let _ = context.stroke();
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
        let active_index = state.selected_index;
        let alpha = if active_index.is_some() && active_index != Some(index) {
            0.42
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
    interaction_callback: &Rc<RefCell<Option<InteractionCallback>>>,
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
    notify_interaction(interaction_callback, state);
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

fn notify_interaction(
    callback: &Rc<RefCell<Option<InteractionCallback>>>,
    state: &Rc<RefCell<RingState>>,
) {
    let callback = callback.borrow().clone();
    let Some(callback) = callback else {
        return;
    };
    let current = state.borrow();
    callback(current.selected_index);
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
        let percent = if slice.display_percent.is_finite() {
            slice.display_percent.max(0.0)
        } else {
            slice.value.max(0.0) / total * 100.0
        };
        draw_center_text(
            context,
            center_x,
            center_y,
            max_width,
            &slice.label,
            &format!("{percent:.1}%"),
            "",
            foreground,
            subdued,
            alpha,
        );
    } else {
        // Keep the center visually quiet until a slice is selected.
        // The surrounding Allocation heading already provides the context.
    }
}

fn fitted_font_size(
    context: &gtk::cairo::Context,
    text: &str,
    max_width: f64,
    preferred: f64,
    minimum: f64,
    weight: gtk::cairo::FontWeight,
) -> f64 {
    if text.is_empty() {
        return preferred;
    }

    context.select_font_face("Sans", gtk::cairo::FontSlant::Normal, weight);
    context.set_font_size(preferred);
    let Ok(extents) = context.text_extents(text) else {
        return preferred;
    };
    let width = extents.x_advance().max(extents.width());
    if width <= max_width || width <= f64::EPSILON {
        preferred
    } else {
        (preferred * max_width / width).clamp(minimum, preferred)
    }
}

fn draw_center_line(
    context: &gtk::cairo::Context,
    center_x: f64,
    baseline_y: f64,
    max_width: f64,
    text: &str,
    preferred_size: f64,
    minimum_size: f64,
    _width_factor: f64,
    weight: gtk::cairo::FontWeight,
    tone: f64,
    alpha: f64,
) {
    if text.is_empty() {
        return;
    }

    let font_size = fitted_font_size(
        context,
        text,
        max_width,
        preferred_size,
        minimum_size,
        weight,
    );
    context.select_font_face("Sans", gtk::cairo::FontSlant::Normal, weight);
    context.set_font_size(font_size);
    context.set_source_rgba(tone, tone, tone, alpha);

    // Keep the original vertical baselines and typography, but center using the
    // actual painted glyph bounds. The previous character-count estimate could
    // put short labels and percentages on different visual
    // centerlines even though they shared the same nominal center point.
    let x = context
        .text_extents(text)
        .map(|extents| center_x - extents.width() / 2.0 - extents.x_bearing())
        .unwrap_or(center_x);
    context.move_to(x, baseline_y);
    let _ = context.show_text(text);
}

fn draw_center_line_centered(
    context: &gtk::cairo::Context,
    center_x: f64,
    center_y: f64,
    max_width: f64,
    text: &str,
    preferred_size: f64,
    minimum_size: f64,
    weight: gtk::cairo::FontWeight,
    tone: f64,
    alpha: f64,
) {
    if text.is_empty() {
        return;
    }

    let font_size = fitted_font_size(
        context,
        text,
        max_width,
        preferred_size,
        minimum_size,
        weight,
    );
    context.select_font_face("Sans", gtk::cairo::FontSlant::Normal, weight);
    context.set_font_size(font_size);
    context.set_source_rgba(tone, tone, tone, alpha);

    if let Ok(extents) = context.text_extents(text) {
        let x = center_x - extents.width() / 2.0 - extents.x_bearing();
        let y = center_y - extents.height() / 2.0 - extents.y_bearing();
        context.move_to(x, y);
    } else {
        context.move_to(center_x, center_y);
    }
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
    // Clip as a final safety net: even unusually long ticker strings
    // can never paint outside the donut hole. Typography also scales down to
    // fit, so clipping should only matter at extremely small transient sizes.
    let clip_radius = (max_width / 2.0).max(12.0);
    let _ = context.save();
    context.arc(center_x, center_y, clip_radius, 0.0, PI * 2.0);
    context.clip();

    if secondary.is_empty() && tertiary.is_empty() {
        // With only one center label (for example the unselected Portfolio
        // state), center the painted glyph bounds on the donut's true midpoint
        // instead of retaining the old two-line baseline offset.
        draw_center_line_centered(
            context, center_x, center_y, max_width, primary, 13.5, 7.0,
            gtk::cairo::FontWeight::Bold, foreground, 0.94 * alpha,
        );
    } else if tertiary.is_empty() {
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
