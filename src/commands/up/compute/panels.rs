use std::collections::VecDeque;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Sparkline},
    Frame,
};

use super::{
    age, cpu_temp_color, dim, gpu_name, gpu_temp_color, load_color, mem_pct, now, panel, tokens,
    val, Board, History, Reading, Unit, ACCENT, DIM,
};

/// The history row: sparkDash's area charts over everything kept, newest at the
/// right edge. A machine without a GPU charts its CPU instead.
pub(super) fn render_charts(f: &mut Frame, area: Rect, u: &Unit, h: &History) {
    let m = &u.metrics;
    let pct = |v: Option<f64>| v.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "—".into());
    let deg = |v: Option<f64>| v.map(|v| format!("{v:.0}°C")).unwrap_or_else(|| "—".into());
    let gpu = !u.spec.gpus.is_empty();
    let cells: [(String, &VecDeque<f64>, Option<f64>, Color); 4] = [
        if gpu {
            (
                format!(" GPU util  {} ", pct(Some(m.gpu_util * 100.0))),
                &h.gpu,
                Some(100.0),
                load_color(m.gpu_util * 100.0),
            )
        } else {
            (
                format!(" CPU util  {} ", pct(m.cpu_util.map(|c| c * 100.0))),
                &h.cpu,
                Some(100.0),
                ACCENT,
            )
        },
        if gpu {
            (
                format!(" GPU temp  {} ", deg(m.gpu_temp)),
                &h.gpu_temp,
                Some(100.0),
                m.gpu_temp.map(gpu_temp_color).unwrap_or(DIM),
            )
        } else {
            (
                format!(" CPU temp  {} ", deg(m.cpu_temp)),
                &h.cpu_temp,
                Some(100.0),
                m.cpu_temp.map(cpu_temp_color).unwrap_or(DIM),
            )
        },
        (
            format!(" memory  {} ", pct(mem_pct(&u.spec, m))),
            &h.mem,
            Some(100.0),
            mem_pct(&u.spec, m).map(load_color).unwrap_or(DIM),
        ),
        (
            format!(
                " decode  {} ",
                m.decode
                    .map(|d| format!("{} tok/s", tokens(d)))
                    .unwrap_or_else(|| "—".into())
            ),
            &h.decode,
            None,
            ACCENT,
        ),
    ];
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 4); 4])
        .split(area);
    for (i, (title, series, max, color)) in cells.into_iter().enumerate() {
        let block = panel(title);
        let inner = block.inner(cols[i]);
        f.render_widget(block, cols[i]);
        let width = inner.width as usize;
        let tail: Vec<f64> = series
            .iter()
            .skip(series.len().saturating_sub(width))
            .copied()
            .collect();
        let top = max
            .unwrap_or_else(|| tail.iter().copied().fold(0.0, f64::max))
            .max(1.0);
        let mut data = vec![0u64; width - tail.len()];
        data.extend(
            tail.iter()
                .map(|v| (v.clamp(0.0, top) * 100.0 / top).round() as u64),
        );
        f.render_widget(
            Sparkline::default()
                .data(&data)
                .max(100)
                .style(Style::default().fg(color)),
            inner,
        );
    }
}

pub(super) fn render_machines(f: &mut Frame, area: Rect, board: &Board, units: &[Unit]) {
    let title = match &board.account {
        Some(a) => format!(" machines · {a} "),
        None => " machines ".into(),
    };
    let block = panel(title);
    // The model column goes first when the panel is narrow, then the GPU name shrinks.
    let inner = area.width.saturating_sub(2) as usize;
    let wide = inner >= 93;
    let gw = if inner >= 71 || wide { 18 } else { 12 };
    let row = |name: &str,
               gpu: &str,
               util: &str,
               temp: &str,
               mem: &str,
               tps: &str,
               model: &str,
               when: &str| {
        let mut s = format!("{name:<14}{gpu:<gw$}{util:>6}{temp:>7}{mem:>6}{tps:>9}");
        if wide {
            s.push_str(&format!("  {model:<22}"));
        }
        s.push_str(&format!("{when:>5}"));
        s
    };
    let head = Line::from(Span::styled(
        format!(
            "  {}",
            row("MACHINE", "GPU", "UTIL", "TEMP", "MEM", "TOK/S", "MODEL", "SEEN")
        ),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    ));
    let mut lines = vec![head];
    for (i, u) in units.iter().enumerate() {
        let m = &u.metrics;
        let fresh = u.local || u.online;
        let dot = if i == board.selected {
            Span::styled(
                "▶ ",
                Style::default().fg(if fresh { Color::Cyan } else { Color::DarkGray }),
            )
        } else if fresh {
            Span::styled("● ", Style::default().fg(Color::Green))
        } else {
            dim("○ ")
        };
        let util = if u.spec.gpus.is_empty() || !fresh {
            "—".into()
        } else {
            format!("{:.0}%", m.gpu_util * 100.0)
        };
        let temp = m
            .gpu_temp
            .filter(|_| fresh)
            .map(|t| format!("{t:.0}°C"))
            .unwrap_or_else(|| "—".into());
        let mem = mem_pct(&u.spec, m)
            .filter(|_| fresh)
            .map(|p| format!("{p:.0}%"))
            .unwrap_or_else(|| "—".into());
        let tps = m
            .decode
            .filter(|_| fresh)
            .map(tokens)
            .unwrap_or_else(|| "—".into());
        let model: String = m
            .model
            .clone()
            .unwrap_or_else(|| "—".into())
            .chars()
            .take(21)
            .collect();
        let when = if u.local {
            "live".into()
        } else {
            u.at.map(|a| age(now() - a)).unwrap_or_else(|| "—".into())
        };
        let name: String = u.label.chars().take(13).collect();
        let gpu: String = gpu_name(&u.spec).chars().take(gw - 1).collect();
        let text = row(&name, &gpu, &util, &temp, &mem, &tps, &model, &when);
        let mut style = Style::default().fg(if fresh { Color::White } else { Color::DarkGray });
        if i == board.selected {
            style = style
                .bg(Color::Rgb(40, 44, 58))
                .add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(vec![dot, Span::styled(text, style)]));
    }
    match &board.fleet {
        Reading::Pending => lines.push(Line::from(dim("  reading the org's machines…"))),
        Reading::Failed(e) => lines.push(Line::from(vec![
            Span::styled("  cloud: ", Style::default().fg(Color::Yellow)),
            dim(e.clone()),
        ])),
        Reading::Ok if board.targets.is_empty() => lines.push(Line::from(dim(
            "  no other machines linked — `hanzo link` on one",
        ))),
        Reading::Ok => {}
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

pub(super) fn render_side(f: &mut Frame, area: Rect, board: &Board) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(board.accounts.len().max(1) as u16 + 2),
        ])
        .split(area);

    let mut nets = Vec::new();
    match &board.net {
        Reading::Pending => nets.push(Line::from(dim(" reading…"))),
        Reading::Failed(e) => nets.push(Line::from(dim(format!(" {e}")))),
        Reading::Ok if board.networks.is_empty() => {
            nets.push(Line::from(dim(" none — `hanzo link`")))
        }
        Reading::Ok => {
            for n in &board.networks {
                let up = matches!(n.status.as_str(), "active" | "online" | "up" | "ready");
                nets.push(Line::from(vec![
                    if up {
                        Span::styled(" ● ", Style::default().fg(Color::Green))
                    } else {
                        dim(" ○ ")
                    },
                    val(
                        if n.name.is_empty() {
                            n.id.clone()
                        } else {
                            n.name.clone()
                        },
                        Color::White,
                    ),
                    dim(format!("  {} · {} nodes", n.status, n.nodes)),
                ]));
            }
        }
    }
    f.render_widget(
        Paragraph::new(nets).block(panel(" networks · zero trust ".into())),
        rows[0],
    );

    let mut accts = Vec::new();
    if board.accounts.is_empty() {
        accts.push(Line::from(dim(" not signed in — `hanzo auth login`")));
    }
    for a in &board.accounts {
        let active = board.account.as_deref() == Some(a.as_str());
        accts.push(Line::from(vec![
            if active {
                Span::styled(" * ", Style::default().fg(ACCENT))
            } else {
                dim("   ")
            },
            val(a.clone(), if active { Color::White } else { DIM }),
        ]));
    }
    f.render_widget(
        Paragraph::new(accts).block(panel(" accounts ".into())),
        rows[1],
    );
}
