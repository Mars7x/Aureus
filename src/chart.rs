use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{DrawingArea, EventControllerMotion, GestureClick, Stack};

use crate::market_data::HistoryRange;
use crate::model::PricePoint;

#[derive(Clone)]
pub struct PriceChart {
    stack: Stack,
    areas: [DrawingArea; 2],
    states: [Rc<RefCell<ChartState>>; 2],
    active: Rc<Cell<usize>>,
}

#[derive(Clone)]
struct ChartState {
    points: Vec<PricePoint>,
    currency: String,
    range: HistoryRange,
    inspect_index: Option<usize>,
    trend_override: Option<f64>,
    exchange_gmt_offset: i32,
    message: Option<String>,
    empty_message: String,
}

impl PriceChart {
    pub fn new() -> Self {
        Self::new_internal(
            270,
            "Not enough price history for this range",
            "Loading price history",
        )
    }

    pub fn new_portfolio() -> Self {
        Self::new_internal(
            220,
            "Add activity to see portfolio history",
            "Add activity to see portfolio history",
        )
    }

    fn new_internal(height: i32, empty_message: &str, initial_message: &str) -> Self {
        let make_state = || {
            Rc::new(RefCell::new(ChartState {
                points: Vec::new(),
                currency: "CAD".into(),
                range: HistoryRange::OneMonth,
                inspect_index: None,
                trend_override: None,
                exchange_gmt_offset: 0,
                message: Some(initial_message.into()),
                empty_message: empty_message.into(),
            }))
        };
        let states = [make_state(), make_state()];
        let areas = [
            build_chart_area(height, states[0].clone()),
            build_chart_area(height, states[1].clone()),
        ];
        let stack = Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(170)
            .vhomogeneous(true)
            .hhomogeneous(true)
            .build();
        stack.add_named(&areas[0], Some("chart-0"));
        stack.add_named(&areas[1], Some("chart-1"));
        stack.set_visible_child_name("chart-0");

        Self {
            stack,
            areas,
            states,
            active: Rc::new(Cell::new(0)),
        }
    }

    pub fn widget(&self) -> &Stack {
        &self.stack
    }

    pub fn set_points_with_market_offset(
        &self,
        points: Vec<PricePoint>,
        currency: &str,
        range: HistoryRange,
        trend_override: Option<f64>,
        exchange_gmt_offset: i32,
    ) {
        self.set_points_internal(
            points,
            currency,
            range,
            trend_override,
            exchange_gmt_offset,
        );
    }

    pub fn set_points_with_trend(
        &self,
        points: Vec<PricePoint>,
        currency: &str,
        range: HistoryRange,
        trend_override: Option<f64>,
    ) {
        self.set_points_internal(points, currency, range, trend_override, 0);
    }

    fn set_points_internal(
        &self,
        points: Vec<PricePoint>,
        currency: &str,
        range: HistoryRange,
        trend_override: Option<f64>,
        exchange_gmt_offset: i32,
    ) {
        let active = self.active.get();
        let reduced = reduce_points_for_display(points, 1200);

        // Refreshing the same range updates in place. A range change renders into
        // the hidden chart first and then crossfades, so only explicit 1D/5D/…
        // changes animate instead of every background quote update flashing.
        if self.states[active].borrow().range == range {
            let mut state = self.states[active].borrow_mut();
            state.points = reduced;
            state.currency = currency.to_string();
            state.inspect_index = None;
            state.trend_override = trend_override;
            state.exchange_gmt_offset = exchange_gmt_offset;
            state.message = None;
            drop(state);
            self.areas[active].queue_draw();
            return;
        }

        let next = 1 - active;
        {
            let mut state = self.states[next].borrow_mut();
            state.points = reduced;
            state.currency = currency.to_string();
            state.range = range;
            state.inspect_index = None;
            state.trend_override = trend_override;
            state.exchange_gmt_offset = exchange_gmt_offset;
            state.message = None;
        }
        self.areas[next].queue_draw();
        self.stack
            .set_visible_child_name(if next == 0 { "chart-0" } else { "chart-1" });
        self.active.set(next);
    }

    pub fn set_message(&self, message: impl Into<String>) {
        let active = self.active.get();
        let mut state = self.states[active].borrow_mut();
        state.message = Some(message.into());
        state.inspect_index = None;
        state.trend_override = None;
        drop(state);
        self.areas[active].queue_draw();
    }
}

fn build_chart_area(height: i32, state: Rc<RefCell<ChartState>>) -> DrawingArea {
    let area = DrawingArea::builder()
        .height_request(height)
        .hexpand(true)
        .vexpand(false)
        .build();
    area.set_accessible_role(gtk::AccessibleRole::Img);

    {
        let state = state.clone();
        area.set_draw_func(move |area, context, width, height| {
            draw_chart(area, context, width, height, &state.borrow());
        });
    }

    let motion = EventControllerMotion::new();
    {
        let state = state.clone();
        let area = area.clone();
        motion.connect_motion(move |_, x, _| {
            let next = {
                let state = state.borrow();
                price_index_at_x(&state.points, f64::from(area.width().max(1)), x)
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
        let state = state.clone();
        let area = area.clone();
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
        let state = state.clone();
        let area = area.clone();
        click.connect_pressed(move |_, _, x, _| {
            let next = {
                let state = state.borrow();
                price_index_at_x(&state.points, f64::from(area.width().max(1)), x)
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

    area
}

fn price_index_at_x(points: &[PricePoint], width: f64, x: f64) -> Option<usize> {
    if points.len() < 2 {
        return None;
    }

    let plot_left = 10.0;
    let plot_right = (width - 10.0).max(plot_left + 1.0);
    let fraction = ((x - plot_left) / (plot_right - plot_left)).clamp(0.0, 1.0);
    let first_timestamp = points.first()?.timestamp;
    let last_timestamp = points.last()?.timestamp;
    let target = first_timestamp as f64
        + fraction * (last_timestamp.saturating_sub(first_timestamp) as f64);

    let upper = points.partition_point(|point| (point.timestamp as f64) <= target);
    if upper == 0 {
        return Some(0);
    }
    if upper >= points.len() {
        return Some(points.len() - 1);
    }

    let lower = upper - 1;
    let lower_distance = ((points[lower].timestamp as f64) - target).abs();
    let upper_distance = ((points[upper].timestamp as f64) - target).abs();
    Some(if upper_distance < lower_distance {
        upper
    } else {
        lower
    })
}

fn reduce_points_for_display(points: Vec<PricePoint>, maximum: usize) -> Vec<PricePoint> {
    if points.len() <= maximum || maximum < 4 {
        return points;
    }

    // Preserve local highs and lows instead of simply dropping every nth point.
    // This keeps long portfolio/stock histories visually faithful while bounding
    // Cairo path work on resize and redraw.
    let bucket_count = ((maximum - 2) / 2).max(1);
    let interior = &points[1..points.len() - 1];
    let bucket_size = interior.len().div_ceil(bucket_count);
    let mut reduced = Vec::with_capacity(maximum + 2);
    reduced.push(points[0].clone());

    for bucket in interior.chunks(bucket_size.max(1)) {
        let Some(minimum_point) = bucket
            .iter()
            .min_by(|left, right| left.close.total_cmp(&right.close))
        else {
            continue;
        };
        let Some(maximum_point) = bucket
            .iter()
            .max_by(|left, right| left.close.total_cmp(&right.close))
        else {
            continue;
        };
        if minimum_point.timestamp <= maximum_point.timestamp {
            reduced.push(minimum_point.clone());
            if maximum_point.timestamp != minimum_point.timestamp {
                reduced.push(maximum_point.clone());
            }
        } else {
            reduced.push(maximum_point.clone());
            if maximum_point.timestamp != minimum_point.timestamp {
                reduced.push(minimum_point.clone());
            }
        }
    }

    reduced.push(points[points.len() - 1].clone());
    reduced
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

    if state.points.len() < 2 {
        context.set_source_rgba(foreground, foreground, foreground, 0.72);
        context.select_font_face(
            "Sans",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Normal,
        );
        context.set_font_size(14.0);
        context.move_to(18.0, height / 2.0);
        let _ = context.show_text(&state.empty_message);
        return;
    }

    let plot_left = 10.0;
    let plot_right = (width - 10.0).max(plot_left + 1.0);
    let plot_top = 14.0;
    let plot_bottom = (height - 28.0).max(plot_top + 1.0);

    let min_timestamp = state.points.first().map(|point| point.timestamp).unwrap_or(0);
    let max_timestamp = state
        .points
        .last()
        .map(|point| point.timestamp)
        .unwrap_or(min_timestamp + 1);
    let time_span = (max_timestamp - min_timestamp).max(1) as f64;

    let mut min_price = f64::INFINITY;
    let mut max_price = f64::NEG_INFINITY;
    for point in &state.points {
        min_price = min_price.min(point.close);
        max_price = max_price.max(point.close);
    }
    let raw_span = (max_price - min_price).abs();
    let padding = if raw_span < f64::EPSILON {
        (max_price.abs() * 0.03).max(1.0)
    } else {
        raw_span * 0.08
    };
    min_price -= padding;
    max_price += padding;
    let price_span = (max_price - min_price).max(f64::EPSILON);

    let point_xy = |point: &PricePoint| {
        let x = plot_left
            + ((point.timestamp - min_timestamp) as f64 / time_span) * (plot_right - plot_left);
        let y = plot_bottom
            - ((point.close - min_price) / price_span) * (plot_bottom - plot_top);
        (x, y)
    };

    let coordinates = state.points.iter().map(point_xy).collect::<Vec<_>>();
    let rising = state.trend_override.map(|trend| trend >= 0.0).unwrap_or_else(|| {
        state
            .points
            .last()
            .zip(state.points.first())
            .map(|(last, first)| last.close >= first.close)
            .unwrap_or(true)
    });
    let (line_red, line_green, line_blue) = if rising {
        (46.0 / 255.0, 194.0 / 255.0, 126.0 / 255.0)
    } else {
        (224.0 / 255.0, 27.0 / 255.0, 36.0 / 255.0)
    };

    if let Some((first_x, first_y)) = coordinates.first().copied() {
        context.move_to(first_x, plot_bottom);
        context.line_to(first_x, first_y);
        for (x, y) in coordinates.iter().skip(1) {
            context.line_to(*x, *y);
        }
        if let Some((last_x, _)) = coordinates.last().copied() {
            context.line_to(last_x, plot_bottom);
        }
        context.close_path();
        context.set_source_rgba(line_red, line_green, line_blue, 0.11);
        let _ = context.fill();
    }

    context.set_line_width(2.0);
    context.set_source_rgba(line_red, line_green, line_blue, 1.0);
    if let Some((first_x, first_y)) = coordinates.first().copied() {
        context.move_to(first_x, first_y);
        for (x, y) in coordinates.iter().skip(1) {
            context.line_to(*x, *y);
        }
        let _ = context.stroke();
    }

    // Keep date labels intentionally sparse so the graph remains legible at
    // ~360 px phone widths.
    context.select_font_face(
        "Sans",
        gtk::cairo::FontSlant::Normal,
        gtk::cairo::FontWeight::Normal,
    );
    context.set_font_size(11.0);
    context.set_source_rgba(foreground, foreground, foreground, 0.58);
    if let Some(first) = state.points.first() {
        context.move_to(plot_left, height - 7.0);
        let _ = context.show_text(&format_axis_time(first.timestamp, state.range, state.exchange_gmt_offset));
    }
    if let Some(last) = state.points.last() {
        let label = format_axis_time(last.timestamp, state.range, state.exchange_gmt_offset);
        let estimated_width = label.chars().count() as f64 * 6.4;
        context.move_to(
            (plot_right - estimated_width).max(plot_left + 40.0),
            height - 7.0,
        );
        let _ = context.show_text(&label);
    }

    let Some(index) = state.inspect_index.filter(|index| *index < coordinates.len()) else {
        return;
    };
    let (x, y) = coordinates[index];

    context.arc(x, y, 4.0, 0.0, std::f64::consts::TAU);
    context.set_source_rgba(line_red, line_green, line_blue, 1.0);
    let _ = context.fill();

    let point = &state.points[index];
    let price = format_price(point.close, &state.currency);
    let time = format_inspect_time(point.timestamp, state.range, state.exchange_gmt_offset);
    let popup_width = 154.0;
    let popup_height = 44.0;
    let popup_x = if x + popup_width + 14.0 < width {
        x + 10.0
    } else {
        (x - popup_width - 10.0).max(4.0)
    };
    let popup_y = (y - popup_height - 10.0)
        .max(4.0)
        .min((height - popup_height - 4.0).max(4.0));

    if dark {
        context.set_source_rgba(32.0 / 255.0, 32.0 / 255.0, 32.0 / 255.0, 0.96);
    } else {
        context.set_source_rgba(1.0, 1.0, 1.0, 0.96);
    }
    rounded_rectangle(context, popup_x, popup_y, popup_width, popup_height, 8.0);
    let _ = context.fill();

    context.set_source_rgba(foreground, foreground, foreground, 0.95);
    context.set_font_size(12.0);
    context.move_to(popup_x + 10.0, popup_y + 17.0);
    let _ = context.show_text(&price);
    context.set_source_rgba(foreground, foreground, foreground, 0.58);
    context.set_font_size(10.5);
    context.move_to(popup_x + 10.0, popup_y + 34.0);
    let _ = context.show_text(&time);
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

fn format_price(value: f64, currency: &str) -> String {
    let number = format_money_number(value);
    match currency {
        "CAD" => format!("C${number}"),
        "USD" => format!("US${number}"),
        "EUR" => format!("€{number}"),
        "GBP" => format!("£{number}"),
        _ => format!("{number} {currency}"),
    }
}

fn format_axis_time(timestamp: i64, range: HistoryRange, exchange_gmt_offset: i32) -> String {
    let local_timestamp = timestamp.saturating_add(i64::from(exchange_gmt_offset));
    match range {
        HistoryRange::OneDay => {
            let (_, hour, minute) = split_utc(local_timestamp);
            format!("{hour:02}:{minute:02}")
        }
        HistoryRange::FiveDays | HistoryRange::OneMonth => {
            let (_, month, day, _, _) = utc_parts(local_timestamp);
            format!("{month:02}/{day:02}")
        }
        _ => {
            let (year, month, _, _, _) = utc_parts(local_timestamp);
            format!("{year}-{month:02}")
        }
    }
}

fn format_inspect_time(timestamp: i64, range: HistoryRange, exchange_gmt_offset: i32) -> String {
    let local_timestamp = timestamp.saturating_add(i64::from(exchange_gmt_offset));
    let (year, month, day, hour, minute) = utc_parts(local_timestamp);
    match range {
        HistoryRange::OneDay | HistoryRange::FiveDays => {
            format!("{year}-{month:02}-{day:02} {hour:02}:{minute:02}")
        }
        _ => format!("{year}-{month:02}-{day:02}"),
    }
}

fn split_utc(timestamp: i64) -> (i64, u32, u32) {
    let seconds = timestamp.rem_euclid(86_400);
    let hour = (seconds / 3_600) as u32;
    let minute = ((seconds % 3_600) / 60) as u32;
    (timestamp.div_euclid(86_400), hour, minute)
}

fn utc_parts(timestamp: i64) -> (i32, u32, u32, u32, u32) {
    let (days, hour, minute) = split_utc(timestamp);
    let (year, month, day) = civil_from_days(days);
    (year, month, day, hour, minute)
}

// Gregorian civil date conversion adapted from Howard Hinnant's public-domain
// civil_from_days algorithm. Keeping it local avoids a heavy date dependency
// just for compact chart labels.
fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year as i32, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, price_index_at_x};
    use crate::model::PricePoint;

    #[test]
    fn converts_unix_epoch_to_civil_date() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_308), (2025, 8, 8));
    }

    #[test]
    fn pointer_lookup_uses_nearest_price_point() {
        let points = vec![
            PricePoint { timestamp: 100, close: 10.0 },
            PricePoint { timestamp: 200, close: 20.0 },
            PricePoint { timestamp: 300, close: 30.0 },
        ];
        assert_eq!(price_index_at_x(&points, 110.0, 10.0), Some(0));
        assert_eq!(price_index_at_x(&points, 110.0, 55.0), Some(1));
        assert_eq!(price_index_at_x(&points, 110.0, 100.0), Some(2));
    }
}
