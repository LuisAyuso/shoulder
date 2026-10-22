use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, BorderType, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table,
};

use crate::app::{App, GpuHist, ProcRow, SortBy};
use crate::gpu::EngineKind;

const PALETTE: [Color; 8] = [
    Color::Cyan,
    Color::LightMagenta,
    Color::LightYellow,
    Color::LightGreen,
    Color::LightBlue,
    Color::LightRed,
    Color::Magenta,
    Color::Yellow,
];

struct Plot {
    name: String,
    color: Color,
    pts: Vec<(f64, f64)>,
}

pub fn draw(f: &mut Frame, app: &App) {
    let [body, footer] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
    let (charts, procs) = if app.show_procs {
        let [c, p] =
            Layout::vertical([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(body);
        (c, Some(p))
    } else {
        (body, None)
    };
    let [r1, r2, r3] = Layout::vertical([Constraint::Ratio(1, 3); 3]).areas(charts);
    let [cpu, mem] = Layout::horizontal([Constraint::Percentage(50); 2]).areas(r1);
    let [gpu_load, gpu_mem] = Layout::horizontal([Constraint::Percentage(50); 2]).areas(r2);
    let [video, temps] = Layout::horizontal([Constraint::Percentage(50); 2]).areas(r3);

    draw_cpu(f, cpu, app);
    draw_mem(f, mem, app);
    match app.selected_gpu() {
        Some(g) => {
            draw_gpu_load(f, gpu_load, app, g);
            draw_gpu_mem(f, gpu_mem, app, g);
            draw_video(f, video, app, g);
        }
        None => {
            for (area, t) in [
                (gpu_load, "GPU"),
                (gpu_mem, "GPU memory"),
                (video, "Video engines"),
            ] {
                let p = Paragraph::new("No GPU detected")
                    .alignment(Alignment::Center)
                    .block(panel(Line::from(t)));
                f.render_widget(p, area);
            }
        }
    }
    draw_temps(f, temps, app);
    if let Some(area) = procs {
        draw_procs(f, area, app);
    }
    draw_footer(f, footer, app);
}

fn panel(title: Line<'_>) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::DarkGray))
        .title(title)
}

fn title<'a>(name: &'a str, value: String, extra: String) -> Line<'a> {
    let mut spans = vec![Span::raw(" "), Span::styled(name, Style::new().bold())];
    if !value.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            value,
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        ));
    }
    if !extra.is_empty() {
        spans.push(Span::styled(
            format!(" · {extra}"),
            Style::new().fg(Color::Gray),
        ));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

#[allow(clippy::too_many_arguments)]
fn time_chart(
    f: &mut Frame,
    area: Rect,
    block: Block,
    plots: &[Plot],
    y_max: f64,
    fmt_y: impl Fn(f64) -> String,
    window: f64,
) {
    let datasets = plots
        .iter()
        .map(|p| {
            // Later datasets overwrite earlier ones cell by cell, so a flat 0%
            // line would hide any low-but-nonzero series in the bottom row.
            // Idle series keep their legend entry but draw nothing.
            let idle = p.pts.iter().all(|&(_, v)| v <= 0.0);
            Dataset::default()
                .name(p.name.clone())
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(p.color))
                .data(if idle { &[] } else { &p.pts })
        })
        .collect();
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [chart_area, legend_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);

    let axis_style = Style::new().fg(Color::DarkGray);
    let chart = Chart::new(datasets)
        .x_axis(Axis::default().bounds([-window, 0.0]))
        .y_axis(
            Axis::default()
                .style(axis_style)
                .bounds([0.0, y_max])
                .labels([fmt_y(0.0), fmt_y(y_max / 2.0), fmt_y(y_max)]),
        )
        .legend_position(None);
    f.render_widget(chart, chart_area);

    // One-line legend in place of the time axis; the window goes on the right.
    let span_label = format!("last {window:.0}s");
    let [items_area, span_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(span_label.len() as u16 + 1),
    ])
    .areas(legend_area);
    let mut items = Vec::new();
    for p in plots {
        items.push(Span::styled("━ ", Style::new().fg(p.color)));
        items.push(Span::styled(
            format!("{}  ", p.name),
            Style::new().fg(Color::Gray),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(items)), items_area);
    let span = Paragraph::new(span_label)
        .alignment(Alignment::Right)
        .style(axis_style);
    f.render_widget(span, span_area);
}

fn pct(v: f64) -> String {
    format!("{v:.0}%")
}

fn draw_cpu(f: &mut Frame, area: Rect, app: &App) {
    let (now, w) = (app.now, app.window());
    let total = app.cpu_total.last().unwrap_or(0.0);
    let mut plots = Vec::new();
    if app.per_core {
        for (i, s) in app.cpu_cores.iter().enumerate() {
            plots.push(Plot {
                name: format!("{i}:{:.0}%", s.last().unwrap_or(0.0)),
                color: PALETTE[i % PALETTE.len()],
                pts: s.window(now, w),
            });
        }
    } else {
        plots.push(Plot {
            name: format!("total {}", pct(total)),
            color: Color::Cyan,
            pts: app.cpu_total.window(now, w),
        });
    }
    let extra = format!("{} cores · {}", app.cpu_cores.len(), app.cpu_name);
    let block = panel(title("CPU", pct(total), extra));
    let y_max = pct_scale(&plots);
    time_chart(f, area, block, &plots, y_max, pct, w);
}

fn fmt_bytes(b: f64) -> String {
    const GIB: f64 = (1u64 << 30) as f64;
    const MIB: f64 = (1u64 << 20) as f64;
    if b == 0.0 {
        "0".into()
    } else if b >= GIB {
        format!("{:.1}G", b / GIB)
    } else {
        format!("{:.0}M", b / MIB)
    }
}

fn draw_mem(f: &mut Frame, area: Rect, app: &App) {
    let (now, w) = (app.now, app.window());
    let used = app.mem_used.last().unwrap_or(0.0);
    let total = app.mem_total as f64;
    let mut plots = vec![Plot {
        name: format!("used {}", fmt_bytes(used)),
        color: Color::LightGreen,
        pts: app.mem_used.window(now, w),
    }];
    let mut extra = format!("{:.0}%", 100.0 * used / total.max(1.0));
    if app.swap_total > 0 {
        let swap = app.swap_used.last().unwrap_or(0.0);
        extra += &format!(
            " · swap {}/{}",
            fmt_bytes(swap),
            fmt_bytes(app.swap_total as f64)
        );
        plots.push(Plot {
            name: format!("swap {}", fmt_bytes(swap)),
            color: Color::LightMagenta,
            pts: app.swap_used.window(now, w),
        });
    }
    let block = panel(title(
        "Memory",
        format!("{}/{}", fmt_bytes(used), fmt_bytes(total)),
        extra,
    ));
    time_chart(f, area, block, &plots, total.max(1.0), fmt_bytes, w);
}

fn engine_plots(app: &App, g: &GpuHist, media: bool) -> Vec<Plot> {
    g.engines
        .iter()
        .filter(|e| e.kind.is_media() == media)
        .enumerate()
        .map(|(i, e)| Plot {
            name: format!("{} {:.0}%", e.label, e.series.last().unwrap_or(0.0)),
            color: engine_color(e.kind, i),
            pts: e.series.window(app.now, app.window()),
        })
        .collect()
}

fn engine_color(kind: EngineKind, i: usize) -> Color {
    match kind {
        EngineKind::Graphics => Color::LightRed,
        EngineKind::Compute => Color::LightBlue,
        EngineKind::Copy => Color::LightYellow,
        EngineKind::Video => Color::LightMagenta,
        EngineKind::Encode => Color::LightMagenta,
        EngineKind::Decode => Color::Cyan,
        EngineKind::VideoEnhance => Color::LightGreen,
        EngineKind::Other => PALETTE[i % PALETTE.len()],
    }
}

fn gpu_header(app: &App, g: &GpuHist) -> String {
    let l = &g.last;
    let mut parts = Vec::new();
    if app.gpus.len() > 1 {
        parts.push(format!("[{}/{}]", app.gpu_sel + 1, app.gpus.len()));
    }
    parts.push(l.name.clone());
    if let Some(mhz) = l.freq_mhz {
        match l.max_freq_mhz {
            Some(max) => parts.push(format!("{mhz}/{max} MHz")),
            None => parts.push(format!("{mhz} MHz")),
        }
    }
    if let Some(t) = l.temp_c {
        parts.push(format!("{t:.0}°C"));
    }
    if let Some(p) = l.power_w {
        parts.push(format!("{p:.1} W"));
    }
    parts.join(" · ")
}

/// Y-axis ceiling for a percentage chart: the smallest of 10/20/25/50/100%
/// that leaves some headroom above the peak.
fn pct_scale(plots: &[Plot]) -> f64 {
    let peak = plots
        .iter()
        .flat_map(|p| p.pts.iter().map(|&(_, v)| v))
        .fold(0.0, f64::max);
    [10.0, 20.0, 25.0, 50.0]
        .into_iter()
        .find(|&c| peak * 1.1 <= c)
        .unwrap_or(100.0)
}

fn draw_gpu_load(f: &mut Frame, area: Rect, app: &App, g: &GpuHist) {
    let plots = engine_plots(app, g, false);
    let busiest = g
        .engines
        .iter()
        .filter(|e| !e.kind.is_media())
        .filter_map(|e| e.series.last())
        .fold(0.0, f64::max);
    let block = panel(title("GPU", pct(busiest), gpu_header(app, g)));
    let y_max = pct_scale(&plots);
    time_chart(f, area, block, &plots, y_max, pct, app.window());
}

fn draw_video(f: &mut Frame, area: Rect, app: &App, g: &GpuHist) {
    let plots = engine_plots(app, g, true);
    let block = panel(title(
        "Video engines",
        String::new(),
        "encode / decode / processing".into(),
    ));
    if plots.is_empty() {
        let p = Paragraph::new("No media engines reported")
            .alignment(Alignment::Center)
            .block(block);
        f.render_widget(p, area);
        return;
    }
    let y_max = pct_scale(&plots);
    time_chart(f, area, block, &plots, y_max, pct, app.window());
}

fn draw_gpu_mem(f: &mut Frame, area: Rect, app: &App, g: &GpuHist) {
    let (now, w) = (app.now, app.window());
    let l = &g.last;
    let mut plots = Vec::new();
    let mut parts = Vec::new();
    let mb = |b: u64| fmt_bytes(b as f64);

    let (name, value, y_max) = match (l.vram_total, l.vram_used) {
        // Discrete GPU: fixed capacity vs what is in use.
        (Some(total), Some(used)) => {
            let used_pts = g.vram.window(now, w);
            plots.push(Plot {
                name: format!("total {}", mb(total)),
                color: Color::LightYellow,
                pts: used_pts.iter().map(|&(x, _)| (x, total as f64)).collect(),
            });
            plots.push(Plot {
                name: format!("used {}", mb(used)),
                color: Color::LightGreen,
                pts: used_pts,
            });
            if let Some(gtt) = l.shared_used.filter(|&b| b > 0) {
                plots.push(Plot {
                    name: format!("GTT {}", mb(gtt)),
                    color: Color::LightBlue,
                    pts: g.shared.window(now, w),
                });
            }
            parts.push(format!("{:.0}%", 100.0 * used as f64 / total.max(1) as f64));
            let peak = g.shared.max_in(now, w).max(total as f64);
            ("VRAM", format!("{}/{}", mb(used), mb(total)), peak * 1.1)
        }
        // No known capacity (integrated GPUs): what clients allocated vs what
        // is actually resident in memory.
        _ => {
            let resident = l.vram_used.unwrap_or(0) + l.shared_used.unwrap_or(0);
            if let Some(alloc) = l.allocated {
                plots.push(Plot {
                    name: format!("allocated {}", mb(alloc)),
                    color: Color::LightYellow,
                    pts: g.allocated.window(now, w),
                });
            }
            plots.push(Plot {
                name: format!("resident {}", mb(resident)),
                color: Color::LightBlue,
                pts: g.resident.window(now, w),
            });
            let peak = g.allocated.max_in(now, w).max(g.resident.max_in(now, w));
            let y_max = (peak * 1.25).max((64u64 << 20) as f64);
            ("GPU memory", mb(resident), y_max)
        }
    };
    if let Some(n) = &l.note {
        parts.push(n.clone());
    }
    let block = panel(title(name, value, parts.join(" · ")));
    time_chart(f, area, block, &plots, y_max, fmt_bytes, w);
}

fn temp_color(frac: f32) -> Color {
    if frac < 0.6 {
        Color::LightGreen
    } else if frac < 0.8 {
        Color::LightYellow
    } else {
        Color::LightRed
    }
}

fn draw_temps(f: &mut Frame, area: Rect, app: &App) {
    let hottest = app.temps.iter().map(|t| t.temp).fold(f32::MIN, f32::max);
    let value = if app.temps.is_empty() {
        "n/a".into()
    } else {
        format!("{hottest:.0}°C")
    };
    let block = panel(title("Temperatures", value, "hottest".into()));

    let inner_w = area.width.saturating_sub(2);
    let label_w = app
        .temps
        .iter()
        .map(|t| t.label.chars().count() as u16)
        .max()
        .unwrap_or(0)
        .min(inner_w / 2);
    let bar_w = inner_w.saturating_sub(label_w + 6 + 9 + 3) as usize;

    let rows = app.temps.iter().map(|t| {
        let crit = t.critical.filter(|c| *c > 0.0).unwrap_or(100.0);
        let frac = (t.temp / crit).clamp(0.0, 1.0);
        let color = temp_color(frac);
        let filled = (frac * bar_w as f32).round() as usize;
        let bar = Line::from(vec![
            Span::styled("━".repeat(filled), Style::new().fg(color)),
            Span::styled("─".repeat(bar_w - filled), Style::new().fg(Color::DarkGray)),
        ]);
        Row::new(vec![
            Cell::from(t.label.clone()),
            Cell::from(Line::from(format!("{:.0}°C", t.temp)).alignment(Alignment::Right))
                .style(Style::new().fg(color).bold()),
            Cell::from(bar),
            Cell::from(Line::from(format!("max {:.0}", t.max_seen)).alignment(Alignment::Right))
                .style(Style::new().fg(Color::Gray)),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(label_w),
            Constraint::Length(6),
            Constraint::Fill(1),
            Constraint::Length(9),
        ],
    )
    .block(block);
    f.render_widget(table, area);
}

fn heat(pct: f64) -> Style {
    let color = if pct < 0.05 {
        Color::DarkGray
    } else if pct < 25.0 {
        Color::Gray
    } else if pct < 60.0 {
        Color::LightYellow
    } else {
        Color::LightRed
    };
    Style::new().fg(color)
}

fn num_cell(text: String, style: Style) -> Cell<'static> {
    Cell::from(Line::from(text).alignment(Alignment::Right)).style(style)
}

fn pct_cell(v: f64) -> Cell<'static> {
    num_cell(format!("{v:.1}"), heat(v))
}

fn bytes_cell(b: u64, pct: f64) -> Cell<'static> {
    let text = if b == 0 {
        "-".into()
    } else {
        fmt_bytes(b as f64)
    };
    num_cell(text, heat(pct))
}

fn draw_procs(f: &mut Frame, area: Rect, app: &App) {
    let sort_name = match app.sort_by {
        SortBy::Cpu => "CPU",
        SortBy::Mem => "memory",
        SortBy::Gpu => "GPU",
        SortBy::Video => "video",
        SortBy::GpuMem => "GPU memory",
    };
    let mut extra = format!("sorted by {sort_name}");
    if !app.is_root {
        extra += " · GPU columns: own processes only (run as root for all)";
    }
    let block = panel(title("Processes", app.procs.len().to_string(), extra));

    let columns: [(&str, Option<SortBy>, Constraint); 10] = [
        ("PID", None, Constraint::Length(7)),
        ("NAME", None, Constraint::Length(15)),
        ("CPU%", Some(SortBy::Cpu), Constraint::Length(6)),
        ("MEM%", Some(SortBy::Mem), Constraint::Length(6)),
        ("MEM", None, Constraint::Length(7)),
        ("GPU%", Some(SortBy::Gpu), Constraint::Length(6)),
        ("VID%", Some(SortBy::Video), Constraint::Length(6)),
        ("GMEM%", Some(SortBy::GpuMem), Constraint::Length(6)),
        ("GMEM", None, Constraint::Length(7)),
        ("COMMAND", None, Constraint::Fill(1)),
    ];
    let header = Row::new(columns.iter().enumerate().map(|(i, (name, sort, _))| {
        let mut style = Style::new().fg(Color::Gray).bold();
        if *sort == Some(app.sort_by) {
            style = style.fg(Color::Cyan).add_modifier(Modifier::UNDERLINED);
        }
        let line = Line::from(*name);
        let left = i <= 1 || i == columns.len() - 1;
        let line = if left {
            line
        } else {
            line.alignment(Alignment::Right)
        };
        Cell::from(line).style(style)
    }));

    // Rows inside the border, minus the header.
    let visible = area.height.saturating_sub(3).max(1) as usize;
    let mut offset = app.proc_offset.get();
    if app.proc_sel < offset {
        offset = app.proc_sel;
    } else if app.proc_sel >= offset + visible {
        offset = app.proc_sel + 1 - visible;
    }
    offset = offset.min(app.procs.len().saturating_sub(visible));
    app.proc_offset.set(offset);

    let rows =
        app.procs
            .iter()
            .enumerate()
            .skip(offset)
            .take(visible)
            .map(|(i, r): (usize, &ProcRow)| {
                let cells = vec![
                    Cell::from(r.pid.to_string()).style(Style::new().fg(Color::DarkGray)),
                    Cell::from(r.name.clone()),
                    pct_cell(r.cpu),
                    pct_cell(r.mem_pct),
                    bytes_cell(r.mem, r.mem_pct),
                    pct_cell(r.gpu),
                    pct_cell(r.video),
                    pct_cell(r.gpu_mem_pct),
                    bytes_cell(r.gpu_mem, r.gpu_mem_pct),
                    Cell::from(r.command.clone()).style(Style::new().fg(Color::DarkGray)),
                ];
                if i == app.proc_sel {
                    // Cell colors would override the row's, and the dim ones
                    // vanish on the DarkGray highlight.
                    let cells = cells.into_iter().map(|c| c.style(Style::new()));
                    Row::new(cells).style(Style::new().bg(Color::DarkGray).fg(Color::White))
                } else {
                    Row::new(cells)
                }
            });
    let table = Table::new(rows, columns.map(|(_, _, c)| c))
        .header(header)
        .column_spacing(1)
        .block(block);
    f.render_widget(table, area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let key = |k: &'static str| Span::styled(k, Style::new().fg(Color::Black).bg(Color::Cyan));
    let desc = |d: String| Span::styled(format!(" {d}  "), Style::new().fg(Color::Gray));
    let mut spans = vec![
        key(" q "),
        desc("quit".into()),
        key(" c "),
        desc(format!(
            "per-core {}",
            if app.per_core { "on" } else { "off" }
        )),
        key(" w "),
        desc(format!("window {:.0}s", app.window())),
        key(" +/- "),
        desc(format!("interval {:.2}s", app.interval().as_secs_f64())),
        key(" p "),
        desc(if app.paused {
            "resume".into()
        } else {
            "pause".into()
        }),
    ];
    spans.extend([
        key(" t "),
        desc("processes".into()),
        key(" s "),
        desc("sort".into()),
        key(" ↑↓ "),
        desc("select".into()),
    ]);
    if app.gpus.len() > 1 {
        spans.push(key(" g "));
        spans.push(desc("next GPU".into()));
    }
    if app.paused {
        spans.push(Span::styled(
            " PAUSED ",
            Style::new().fg(Color::Black).bg(Color::Yellow),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn low_value_line_visible() {
        let flat = |v: f64| (0..60).map(|i| (i as f64 - 60.0, v)).collect::<Vec<_>>();
        let plots = vec![
            Plot {
                name: "busy".into(),
                color: Color::LightRed,
                pts: flat(12.0),
            },
            Plot {
                name: "idle".into(),
                color: Color::LightYellow,
                pts: flat(0.0),
            },
        ];
        let mut term = Terminal::new(TestBackend::new(50, 9)).unwrap();
        term.draw(|f| time_chart(f, f.area(), Block::bordered(), &plots, 100.0, pct, 60.0))
            .unwrap();
        let buf = term.backend().buffer();
        let red = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .filter(|&p| buf[p].fg == Color::LightRed)
            .count();
        for y in 0..buf.area.height {
            let line: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            println!("{line}");
        }
        println!("red cells: {red}");
        assert_eq!(pct_scale(&plots), 20.0);
        assert!(red > 20, "12% line hidden by the 0% line");
    }
}
