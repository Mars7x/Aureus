use std::path::Path;

use cairo::{Context, FontSlant, FontWeight, PdfSurface};

const PAGE_WIDTH: f64 = 595.0;
const PAGE_HEIGHT: f64 = 842.0;
const MARGIN: f64 = 44.0;
const CONTENT_RIGHT: f64 = PAGE_WIDTH - MARGIN;
const CONTENT_WIDTH: f64 = CONTENT_RIGHT - MARGIN;
const FOOTER_Y: f64 = PAGE_HEIGHT - 27.0;

#[derive(Clone, Debug)]
pub struct ReportMetric {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct PortfolioHoldingRow {
    pub code: String,
    pub name: String,
    pub shares: String,
    pub price: String,
    pub market_value: String,
    pub cost_basis: String,
    pub gain: String,
}

#[derive(Clone, Debug)]
pub struct PortfolioActivityRow {
    pub activity: String,
    pub count: usize,
    pub amount: String,
}

#[derive(Clone, Debug)]
pub struct PortfolioReport {
    pub generated_on: String,
    pub account_name: String,
    pub account_currency: String,
    pub period_label: String,
    pub period_dates: String,
    pub metrics: Vec<ReportMetric>,
    pub activity: Vec<PortfolioActivityRow>,
    pub holdings: Vec<PortfolioHoldingRow>,
}

#[derive(Clone, Debug)]
pub struct DividendSummaryRow {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct DividendPaymentRow {
    pub ex_date: String,
    pub source: String,
    pub shares: String,
    pub rate: String,
    pub gross: String,
}

#[derive(Clone, Debug)]
pub struct DividendReport {
    pub generated_on: String,
    pub account_name: String,
    pub account_currency: String,
    pub period_label: String,
    pub period_dates: String,
    pub total_gross: String,
    pub distribution_count: usize,
    pub payer_count: usize,
    pub month_count: usize,
    pub months: Vec<DividendSummaryRow>,
    pub stocks: Vec<DividendSummaryRow>,
    pub distributions: Vec<DividendPaymentRow>,
}

const PORTFOLIO_ACTIVITY_COLUMNS: [(&str, f64); 3] = [
    ("Activity", 0.50),
    ("Entries", 0.18),
    ("Amount", 0.32),
];
const PORTFOLIO_HOLDING_WIDTHS: [f64; 6] = [0.25, 0.08, 0.14, 0.19, 0.17, 0.17];
const DIVIDEND_COLUMNS: [(&str, f64); 5] = [
    ("Ex-dividend", 0.18),
    ("Security", 0.18),
    ("Shares", 0.16),
    ("Rate / Share", 0.23),
    ("Gross", 0.25),
];
const SUMMARY_COLUMNS: [(&str, f64); 2] = [("Period / Security", 0.68), ("Gross Income", 0.32)];

pub fn write_portfolio_pdf(path: &Path, report: &PortfolioReport) -> Result<(), String> {
    let mut pdf = PdfWriter::new(
        path,
        "Portfolio Performance Report",
        &report.account_name,
        &report.account_currency,
        &report.period_label,
        &report.period_dates,
        &report.generated_on,
    )?;

    pdf.section("Account Summary")?;
    pdf.metric_grid(&report.metrics)?;

    pdf.section("Period Activity")?;
    if report.activity.is_empty() {
        pdf.note("No account activity was recorded during this reporting period.")?;
    } else {
        pdf.table_header(&PORTFOLIO_ACTIVITY_COLUMNS)?;
        for row in &report.activity {
            pdf.portfolio_activity_row(row)?;
        }
    }

    pdf.section("Holdings at Period End")?;
    if report.holdings.is_empty() {
        pdf.note("No securities were held in this account at the end of the reporting period.")?;
    } else {
        pdf.portfolio_holding_header()?;
        for row in &report.holdings {
            pdf.portfolio_holding_row(row)?;
        }
    }

    pdf.section("Important Information")?;
    pdf.note(&format!(
        "Statement values are presented in {} unless a security price is explicitly shown in its native currency.",
        report.account_currency
    ))?;
    pdf.note("This report is generated from activity recorded in Aureus and historical market data available for the selected period. It is intended as a portfolio record, not a brokerage statement or tax document.")?;
    pdf.finish()
}

pub fn write_dividend_pdf(path: &Path, report: &DividendReport) -> Result<(), String> {
    let mut pdf = PdfWriter::new(
        path,
        "Dividend Income Report",
        &report.account_name,
        &report.account_currency,
        &report.period_label,
        &report.period_dates,
        &report.generated_on,
    )?;

    pdf.section("Income Summary")?;
    pdf.metric_grid(&[
        ReportMetric {
            label: "Gross dividend income".into(),
            value: report.total_gross.clone(),
        },
        ReportMetric {
            label: "Distributions".into(),
            value: report.distribution_count.to_string(),
        },
        ReportMetric {
            label: "Dividend-paying securities".into(),
            value: report.payer_count.to_string(),
        },
        ReportMetric {
            label: "Months with distributions".into(),
            value: report.month_count.to_string(),
        },
    ])?;

    pdf.summary_table("Income by Ex-dividend Month", &report.months)?;
    pdf.summary_table("Income by Security", &report.stocks)?;

    pdf.section("Distribution Detail")?;
    if report.distributions.is_empty() {
        pdf.note("No dividend distributions were identified for this account and reporting period.")?;
    } else {
        pdf.table_header(&DIVIDEND_COLUMNS)?;
        for row in &report.distributions {
            pdf.dividend_row(row)?;
        }
    }

    pdf.section("Important Information")?;
    pdf.note("Historical dividend income is reconstructed from known distributions and the shares recorded as held on each ex-dividend date. This keeps prior-year reports useful even when Aureus never created a historical dividend cash entry.")?;
    pdf.note("Gross reconstructed income can differ from broker cash because of withholding tax, dividend reinvestment, foreign-exchange treatment, or corporate adjustments. This report is not a tax document.")?;
    pdf.finish()
}

struct PdfWriter {
    surface: PdfSurface,
    context: Context,
    title: String,
    account_name: String,
    account_currency: String,
    period_label: String,
    period_dates: String,
    generated_on: String,
    y: f64,
    page: u32,
}

impl PdfWriter {
    fn new(
        path: &Path,
        title: &str,
        account_name: &str,
        account_currency: &str,
        period_label: &str,
        period_dates: &str,
        generated_on: &str,
    ) -> Result<Self, String> {
        let surface = PdfSurface::new(PAGE_WIDTH, PAGE_HEIGHT, path).map_err(pdf_error)?;
        let context = Context::new(&surface).map_err(pdf_error)?;
        let mut writer = Self {
            surface,
            context,
            title: title.to_string(),
            account_name: account_name.to_string(),
            account_currency: account_currency.to_string(),
            period_label: period_label.to_string(),
            period_dates: period_dates.to_string(),
            generated_on: generated_on.to_string(),
            y: MARGIN,
            page: 1,
        };
        writer.page_header(false)?;
        Ok(writer)
    }

    fn finish(mut self) -> Result<(), String> {
        self.page_footer()?;
        self.context.show_page().map_err(pdf_error)?;
        drop(self.context);
        self.surface.finish();
        self.surface.status().map_err(pdf_error)
    }

    fn page_header(&mut self, continued: bool) -> Result<(), String> {
        // Branding is deliberately restrained: one wordmark and one accent rule.
        self.set_font(8.4, true);
        self.set_accent();
        self.draw_text(MARGIN, self.y, "AUREUS")?;
        self.set_font(7.4, false);
        self.set_muted();
        let generated = format!("Generated {}", self.generated_on);
        let generated_width = self.text_width(&generated)?;
        self.draw_text(CONTENT_RIGHT - generated_width, self.y, &generated)?;
        self.y += 17.0;

        self.set_accent();
        self.context.set_line_width(1.4);
        self.context.move_to(MARGIN, self.y);
        self.context.line_to(CONTENT_RIGHT, self.y);
        self.context.stroke().map_err(pdf_error)?;
        self.y += if continued { 18.0 } else { 22.0 };

        self.set_text_color();
        self.set_font(if continued { 14.0 } else { 20.0 }, true);
        let heading = if continued {
            format!("{} - continued", self.title)
        } else {
            self.title.clone()
        };
        self.draw_fit(MARGIN, self.y, &heading, CONTENT_WIDTH)?;
        self.y += if continued { 20.0 } else { 27.0 };

        let account_name = self.account_name.clone();
        let period_label = self.period_label.clone();
        let account_currency = self.account_currency.clone();
        let period_dates = self.period_dates.clone();
        self.metadata_line("Account", &account_name, "Reporting period", &period_label)?;
        self.metadata_line("Currency", &account_currency, "Period dates", &period_dates)?;
        self.y += 8.0;
        self.hairline()?;
        self.y += 10.0;
        Ok(())
    }

    fn metadata_line(
        &mut self,
        left_label: &str,
        left_value: &str,
        right_label: &str,
        right_value: &str,
    ) -> Result<(), String> {
        let left_value_x = MARGIN + 72.0;
        let right_x = MARGIN + CONTENT_WIDTH * 0.55;
        let right_value_x = right_x + 82.0;
        self.set_font(7.5, false);
        self.set_muted();
        self.draw_text(MARGIN, self.y, left_label)?;
        self.draw_text(right_x, self.y, right_label)?;
        self.set_font(8.7, true);
        self.set_text_color();
        self.draw_fit(
            left_value_x,
            self.y,
            left_value,
            right_x - left_value_x - 12.0,
        )?;
        self.draw_fit(
            right_value_x,
            self.y,
            right_value,
            CONTENT_RIGHT - right_value_x,
        )?;
        self.y += 15.0;
        Ok(())
    }

    fn page_footer(&mut self) -> Result<(), String> {
        self.context.set_line_width(0.5);
        self.context.set_source_rgb(0.86, 0.86, 0.86);
        self.context.move_to(MARGIN, FOOTER_Y - 10.0);
        self.context.line_to(CONTENT_RIGHT, FOOTER_Y - 10.0);
        self.context.stroke().map_err(pdf_error)?;
        self.set_font(7.3, false);
        self.set_muted();
        self.draw_text(MARGIN, FOOTER_Y, "Aureus portfolio record")?;
        let page = format!("Page {}", self.page);
        let width = self.text_width(&page)?;
        self.draw_text(CONTENT_RIGHT - width, FOOTER_Y, &page)?;
        self.set_text_color();
        Ok(())
    }

    fn new_page(&mut self) -> Result<(), String> {
        self.page_footer()?;
        self.context.show_page().map_err(pdf_error)?;
        self.page += 1;
        self.y = MARGIN;
        self.set_text_color();
        self.page_header(true)
    }

    fn ensure_space(&mut self, needed: f64) -> Result<bool, String> {
        if self.y + needed >= FOOTER_Y - 16.0 {
            self.new_page()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn section(&mut self, title: &str) -> Result<(), String> {
        self.ensure_space(36.0)?;
        self.y += 11.0;
        self.set_font(10.6, true);
        self.set_text_color();
        self.draw_text(MARGIN, self.y, title)?;
        self.y += 8.0;
        self.context.set_line_width(0.7);
        self.context.set_source_rgb(0.72, 0.72, 0.72);
        self.context.move_to(MARGIN, self.y);
        self.context.line_to(CONTENT_RIGHT, self.y);
        self.context.stroke().map_err(pdf_error)?;
        self.set_text_color();
        self.y += 13.0;
        Ok(())
    }

    fn metric_grid(&mut self, metrics: &[ReportMetric]) -> Result<(), String> {
        if metrics.is_empty() {
            return self.note("No summary values are available for this reporting period.");
        }
        let gap = 20.0;
        let column_width = (CONTENT_WIDTH - gap) / 2.0;
        for pair in metrics.chunks(2) {
            self.ensure_space(38.0)?;
            for (index, metric) in pair.iter().enumerate() {
                let x = MARGIN + index as f64 * (column_width + gap);
                self.set_font(7.5, false);
                self.set_muted();
                self.draw_fit(x, self.y, &metric.label, column_width)?;
                self.set_font(11.2, true);
                self.set_text_color();
                self.draw_fit(x, self.y + 16.0, &metric.value, column_width)?;
                self.context.set_line_width(0.45);
                self.context.set_source_rgb(0.88, 0.88, 0.88);
                self.context.move_to(x, self.y + 24.0);
                self.context.line_to(x + column_width, self.y + 24.0);
                self.context.stroke().map_err(pdf_error)?;
            }
            self.set_text_color();
            self.y += 36.0;
        }
        Ok(())
    }

    fn summary_table(&mut self, title: &str, rows: &[DividendSummaryRow]) -> Result<(), String> {
        self.section(title)?;
        if rows.is_empty() {
            return self.note("No data for this section.");
        }
        self.table_header(&SUMMARY_COLUMNS)?;
        for row in rows {
            let changed_page = self.ensure_space(18.0)?;
            if changed_page {
                self.table_header(&SUMMARY_COLUMNS)?;
            }
            self.set_font(8.4, false);
            self.set_text_color();
            self.draw_fit(MARGIN + 5.0, self.y, &row.label, CONTENT_WIDTH * 0.66)?;
            self.draw_right_fit(CONTENT_RIGHT - 5.0, self.y, &row.value, CONTENT_WIDTH * 0.30)?;
            self.row_rule(self.y + 7.0)?;
            self.y += 18.0;
        }
        Ok(())
    }

    fn table_header(&mut self, columns: &[(&str, f64)]) -> Result<(), String> {
        self.ensure_space(26.0)?;
        self.context.set_source_rgb(0.945, 0.945, 0.945);
        self.context.rectangle(MARGIN, self.y - 11.0, CONTENT_WIDTH, 20.0);
        self.context.fill().map_err(pdf_error)?;
        self.set_text_color();
        self.set_font(7.4, true);
        let usable = CONTENT_WIDTH - 10.0;
        let mut x = MARGIN + 5.0;
        for (label, fraction) in columns {
            self.draw_fit(x, self.y + 1.0, label, usable * fraction - 4.0)?;
            x += usable * fraction;
        }
        self.y += 23.0;
        Ok(())
    }

    fn portfolio_activity_row(&mut self, row: &PortfolioActivityRow) -> Result<(), String> {
        let changed_page = self.ensure_space(20.0)?;
        if changed_page {
            self.table_header(&PORTFOLIO_ACTIVITY_COLUMNS)?;
        }
        let usable = CONTENT_WIDTH - 10.0;
        self.set_font(8.4, false);
        self.set_text_color();
        self.draw_fit(MARGIN + 5.0, self.y, &row.activity, usable * 0.48)?;
        let entries_right = MARGIN + 5.0 + usable * (0.50 + 0.18) - 5.0;
        self.draw_right(entries_right, self.y, &row.count.to_string())?;
        self.draw_right_fit(CONTENT_RIGHT - 5.0, self.y, &row.amount, usable * 0.30)?;
        self.row_rule(self.y + 7.0)?;
        self.y += 19.0;
        Ok(())
    }

    fn portfolio_holding_header(&mut self) -> Result<(), String> {
        let widths = PORTFOLIO_HOLDING_WIDTHS;
        let usable = CONTENT_WIDTH - 10.0;
        let x0 = MARGIN + 5.0;

        self.ensure_space(26.0)?;
        self.context.set_source_rgb(0.945, 0.945, 0.945);
        self.context.rectangle(MARGIN, self.y - 11.0, CONTENT_WIDTH, 20.0);
        self.context.fill().map_err(pdf_error)?;
        self.set_text_color();
        self.set_font(7.4, true);

        // Security remains left-aligned. Every numeric heading uses the exact same
        // right edge as the values in portfolio_holding_row so columns read as
        // true financial-statement columns rather than independently placed text.
        self.draw_fit(x0, self.y + 1.0, "Security", usable * widths[0] - 5.0)?;
        let labels = ["Qty", "Price", "Market Value", "Cost Basis", "Gain/Loss"];
        let mut right = x0 + usable * (widths[0] + widths[1]);
        for (index, label) in labels.iter().enumerate() {
            self.draw_right_fit(
                right - 4.0,
                self.y + 1.0,
                label,
                usable * widths[index + 1] - 6.0,
            )?;
            if index + 2 < widths.len() {
                right += usable * widths[index + 2];
            }
        }

        self.y += 23.0;
        Ok(())
    }

    fn portfolio_holding_row(&mut self, row: &PortfolioHoldingRow) -> Result<(), String> {
        let widths = PORTFOLIO_HOLDING_WIDTHS;
        let usable = CONTENT_WIDTH - 10.0;
        let x0 = MARGIN + 5.0;

        self.set_font(7.1, false);
        let name_lines = self.wrap_text(&row.name, usable * widths[0] - 5.0)?;
        let name_line_count = name_lines.len().max(1) as f64;
        let needed = 26.0 + (name_line_count - 1.0) * 9.0;
        let changed_page = self.ensure_space(needed)?;
        if changed_page {
            self.portfolio_holding_header()?;
        }

        self.set_font(8.1, true);
        self.set_text_color();
        self.draw_fit(x0, self.y, &row.code, usable * widths[0] - 5.0)?;

        self.set_font(7.25, false);
        let values = [
            row.shares.as_str(),
            row.price.as_str(),
            row.market_value.as_str(),
            row.cost_basis.as_str(),
            row.gain.as_str(),
        ];
        let mut right = x0 + usable * (widths[0] + widths[1]);
        for (index, value) in values.iter().enumerate() {
            self.draw_right_fit(right - 4.0, self.y, value, usable * widths[index + 1] - 6.0)?;
            if index + 2 < widths.len() {
                right += usable * widths[index + 2];
            }
        }

        self.y += 11.5;
        self.set_font(7.1, false);
        self.set_muted();
        for line in name_lines {
            self.draw_text(x0, self.y, &line)?;
            self.y += 9.0;
        }
        self.row_rule(self.y + 2.0)?;
        self.set_text_color();
        self.y += 11.0;
        Ok(())
    }

    fn dividend_row(&mut self, row: &DividendPaymentRow) -> Result<(), String> {
        let changed_page = self.ensure_space(20.0)?;
        if changed_page {
            self.table_header(&DIVIDEND_COLUMNS)?;
        }
        let widths = [0.18, 0.18, 0.16, 0.23, 0.25];
        let usable = CONTENT_WIDTH - 10.0;
        let x0 = MARGIN + 5.0;
        self.set_font(8.0, false);
        self.set_text_color();
        self.draw_fit(x0, self.y, &row.ex_date, usable * widths[0] - 4.0)?;
        self.draw_fit(
            x0 + usable * widths[0],
            self.y,
            &row.source,
            usable * widths[1] - 4.0,
        )?;
        let shares_right = x0 + usable * (widths[0] + widths[1] + widths[2]);
        let rate_right = shares_right + usable * widths[3];
        self.draw_right_fit(shares_right - 4.0, self.y, &row.shares, usable * widths[2] - 5.0)?;
        self.draw_right_fit(rate_right - 4.0, self.y, &row.rate, usable * widths[3] - 5.0)?;
        self.draw_right_fit(CONTENT_RIGHT - 5.0, self.y, &row.gross, usable * widths[4] - 5.0)?;
        self.row_rule(self.y + 7.0)?;
        self.y += 19.0;
        Ok(())
    }

    fn note(&mut self, text: &str) -> Result<(), String> {
        self.ensure_space(23.0)?;
        self.set_font(8.0, false);
        self.set_muted();
        for line in self.wrap_text(text, CONTENT_WIDTH - 8.0)? {
            self.ensure_space(14.0)?;
            self.draw_text(MARGIN + 4.0, self.y, &line)?;
            self.y += 13.0;
        }
        self.set_text_color();
        self.y += 3.0;
        Ok(())
    }

    fn wrap_text(&self, text: &str, max_width: f64) -> Result<Vec<String>, String> {
        let mut lines = Vec::new();
        let mut current = String::new();
        for word in sanitize_text(text).split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if self.text_width(&candidate)? <= max_width {
                current = candidate;
            } else {
                if !current.is_empty() {
                    lines.push(current);
                }
                current = word.to_string();
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
        Ok(lines)
    }

    fn hairline(&self) -> Result<(), String> {
        self.context.set_line_width(0.5);
        self.context.set_source_rgb(0.82, 0.82, 0.82);
        self.context.move_to(MARGIN, self.y);
        self.context.line_to(CONTENT_RIGHT, self.y);
        self.context.stroke().map_err(pdf_error)?;
        self.set_text_color();
        Ok(())
    }

    fn row_rule(&self, y: f64) -> Result<(), String> {
        self.context.set_line_width(0.35);
        self.context.set_source_rgb(0.91, 0.91, 0.91);
        self.context.move_to(MARGIN, y);
        self.context.line_to(CONTENT_RIGHT, y);
        self.context.stroke().map_err(pdf_error)?;
        self.set_text_color();
        Ok(())
    }

    fn set_font(&self, size: f64, bold: bool) {
        self.context.select_font_face(
            "Sans",
            FontSlant::Normal,
            if bold { FontWeight::Bold } else { FontWeight::Normal },
        );
        self.context.set_font_size(size);
    }

    fn set_text_color(&self) {
        self.context.set_source_rgb(0.11, 0.11, 0.11);
    }

    fn set_muted(&self) {
        self.context.set_source_rgb(0.39, 0.39, 0.39);
    }

    fn set_accent(&self) {
        // Reports are intentionally monochrome for a conventional statement look.
        self.context.set_source_rgb(0.11, 0.11, 0.11);
    }

    fn draw_text(&self, x: f64, y: f64, text: &str) -> Result<(), String> {
        self.context.move_to(x, y);
        self.context.show_text(&sanitize_text(text)).map_err(pdf_error)
    }

    fn draw_right(&self, right: f64, y: f64, text: &str) -> Result<(), String> {
        let width = self.text_width(text)?;
        self.draw_text(right - width, y, text)
    }

    fn text_width(&self, text: &str) -> Result<f64, String> {
        self.context
            .text_extents(&sanitize_text(text))
            .map(|extents| extents.width())
            .map_err(pdf_error)
    }

    fn draw_fit(&self, x: f64, y: f64, text: &str, max_width: f64) -> Result<(), String> {
        let clean = sanitize_text(text);
        let width = self.text_width(&clean)?;
        if width <= max_width || width <= f64::EPSILON {
            return self.draw_text(x, y, &clean);
        }

        // Financial values should never be replaced with ellipses. Scale only
        // the overflowing cell's text so the complete value remains readable.
        let scale = (max_width / width).clamp(0.10, 1.0);
        self.context.save().map_err(pdf_error)?;
        self.context.translate(x, y);
        self.context.scale(scale, scale);
        self.context.move_to(0.0, 0.0);
        let result = self.context.show_text(&clean).map_err(pdf_error);
        self.context.restore().map_err(pdf_error)?;
        result
    }

    fn draw_right_fit(&self, right: f64, y: f64, text: &str, max_width: f64) -> Result<(), String> {
        let clean = sanitize_text(text);
        let width = self.text_width(&clean)?;
        if width <= max_width || width <= f64::EPSILON {
            return self.draw_right(right, y, &clean);
        }

        let scale = (max_width / width).clamp(0.10, 1.0);
        let rendered_width = width * scale;
        self.context.save().map_err(pdf_error)?;
        self.context.translate(right - rendered_width, y);
        self.context.scale(scale, scale);
        self.context.move_to(0.0, 0.0);
        let result = self.context.show_text(&clean).map_err(pdf_error);
        self.context.restore().map_err(pdf_error)?;
        result
    }
}

fn sanitize_text(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\u{2013}' | '\u{2014}' => '-',
            '\u{00b7}' => '|',
            '\u{2026}' => '.',
            _ if ch.is_control() => ' ',
            _ => ch,
        })
        .collect()
}

fn pdf_error(error: cairo::Error) -> String {
    format!("PDF export failed: {error}")
}
