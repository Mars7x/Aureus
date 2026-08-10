use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::DrawingArea;

use crate::model::PricePoint;

#[derive(Clone)]
pub struct Sparkline {
    area: DrawingArea,
    points: Rc<RefCell<Vec<PricePoint>>>,
}

impl Sparkline {
    pub fn new() -> Self {
        let area = DrawingArea::builder()
            .height_request(58)
            .hexpand(true)
            .vexpand(false)
            .build();
        area.set_accessible_role(gtk::AccessibleRole::Img);

        let points = Rc::new(RefCell::new(Vec::new()));
        {
            let points = points.clone();
            area.set_draw_func(move |area, context, width, height| {
                draw_sparkline(area, context, width, height, &points.borrow());
            });
        }

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

        Self { area, points }
    }

    pub fn widget(&self) -> &DrawingArea {
        &self.area
    }

    pub fn set_points(&self, points: Vec<PricePoint>) {
        self.area.set_visible(points.len() >= 2);
        *self.points.borrow_mut() = points;
        self.area.queue_draw();
    }
}

fn draw_sparkline(
    area: &DrawingArea,
    context: &gtk::cairo::Context,
    width: i32,
    height: i32,
    points: &[PricePoint],
) {
    if points.len() < 2 {
        return;
    }

    let width = f64::from(width.max(1));
    let height = f64::from(height.max(1));
    let left = 2.0;
    let right = (width - 2.0).max(left + 1.0);
    let top = 5.0;
    let bottom = (height - 5.0).max(top + 1.0);

    let first_timestamp = points.first().map(|point| point.timestamp).unwrap_or(0);
    let last_timestamp = points
        .last()
        .map(|point| point.timestamp)
        .unwrap_or(first_timestamp + 1);
    let time_span = (last_timestamp - first_timestamp).max(1) as f64;

    let mut low = f64::INFINITY;
    let mut high = f64::NEG_INFINITY;
    for point in points {
        low = low.min(point.close);
        high = high.max(point.close);
    }
    let raw_span = (high - low).abs();
    let padding = if raw_span < f64::EPSILON {
        (high.abs() * 0.02).max(0.5)
    } else {
        raw_span * 0.08
    };
    low -= padding;
    high += padding;
    let price_span = (high - low).max(f64::EPSILON);

    let manager = adw::StyleManager::for_display(&area.display());
    let accent = manager.accent_color_rgba();
    context.set_line_width(1.8);
    context.set_line_cap(gtk::cairo::LineCap::Round);
    context.set_line_join(gtk::cairo::LineJoin::Round);
    context.set_source_rgba(
        f64::from(accent.red()),
        f64::from(accent.green()),
        f64::from(accent.blue()),
        0.95,
    );

    for (index, point) in points.iter().enumerate() {
        let x = left
            + ((point.timestamp - first_timestamp) as f64 / time_span) * (right - left);
        let y = bottom - ((point.close - low) / price_span) * (bottom - top);
        if index == 0 {
            context.move_to(x, y);
        } else {
            context.line_to(x, y);
        }
    }
    let _ = context.stroke();
}
