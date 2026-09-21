use crate::{
    app::{App, Playback, VisualizerMode},
    model::Track,
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Block, Cell, Paragraph, Row, Table, TableState, Wrap,
        canvas::{Canvas, Circle, Line as CanvasLine, Points},
    },
};
use ratatui_image::{Image, Resize};
use std::collections::BTreeSet;

// Tokyo Night foregrounds. Backgrounds stay at the terminal's default, including
// selections and help, so terminal transparency is preserved throughout.
const TEXT: Color = Color::Rgb(0xc0, 0xca, 0xf5);
const SECONDARY: Color = Color::Rgb(0xa9, 0xb1, 0xd6);
const MUTED: Color = Color::Rgb(0x73, 0x7a, 0xa2);
const BLUE: Color = Color::Rgb(0x7a, 0xa2, 0xf7);
const GREEN: Color = Color::Rgb(0x9e, 0xce, 0x6a);
const YELLOW: Color = Color::Rgb(0xe0, 0xaf, 0x68);
const RED: Color = Color::Rgb(0xf7, 0x76, 0x8e);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().fg(TEXT)), area);
    if area.width < 40 || area.height < 14 {
        frame.render_widget(
            Paragraph::new("Resize to 40 × 14\nCtrl+C quit").style(Style::default().fg(MUTED)),
            area,
        );
        return;
    }
    let inner = area.inner(ratatui::layout::Margin::new(2, 1));
    if app.help {
        help(frame, app, inner);
        return;
    }
    if app.menu {
        actions(frame, app, inner);
        return;
    }
    if app.now_playing_view {
        playing_view(frame, app, inner);
        return;
    }
    let playing = app.queue.current.is_some();
    let show_search =
        (!app.queue_focused && !app.library_focused && !app.home_focused && !app.explore_focused)
            || app.editing;
    let rows = Layout::vertical([
        Constraint::Length(1), // Navigation: the active view and how to find help.
        Constraint::Length(1),
        Constraint::Length(u16::from(show_search)),
        Constraint::Length(u16::from(show_search)),
        Constraint::Min(1),
        Constraint::Length(if app.status.is_empty() { 0 } else { 2 }),
        Constraint::Length(u16::from(playing)), // Whitespace separates transport.
        Constraint::Length(if playing { 2 } else { 0 }),
    ])
    .split(inner);
    navigation(frame, app, rows[0]);
    let mut context = Vec::new();
    if !app.marks().is_empty() {
        context.push(format!("{} selected", app.marks().len()));
    }
    if app.radio_loading() {
        context.push("starting radio… · esc cancel".into());
    }
    frame.render_widget(
        Paragraph::new(context.join(" · ")).style(Style::default().fg(BLUE)),
        rows[1],
    );
    if show_search {
        search(frame, app, rows[2]);
    }
    if app.home_focused || app.explore_focused {
        discovery(frame, app, rows[4]);
    } else if app.library_focused {
        library(frame, app, rows[4]);
    } else if app.queue_focused {
        if app.queue.upcoming.is_empty() {
            frame.render_widget(
                Paragraph::new("Queue is empty").style(Style::default().fg(MUTED)),
                rows[4],
            );
        } else {
            tracks(
                frame,
                app.queue.upcoming.iter(),
                &mut app.queue_state,
                &app.queue_marks,
                !app.editing,
                rows[4],
            );
        }
    } else if app.results.is_empty() {
        let message = if app.searching {
            "Searching…"
        } else if app.query.is_empty() || !app.status.is_empty() {
            ""
        } else {
            "No songs found"
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(MUTED)),
            rows[4],
        );
    } else {
        tracks(
            frame,
            app.results.iter(),
            &mut app.results_state,
            &app.result_marks,
            !app.editing,
            rows[4],
        );
    }
    if !app.status.is_empty() {
        frame.render_widget(
            Paragraph::new(app.status.as_str())
                .style(Style::default().fg(RED))
                .wrap(Wrap { trim: true }),
            rows[5],
        );
    }
    if playing {
        now_playing(frame, app, rows[7]);
    }
}

fn navigation(frame: &mut Frame, app: &App, area: Rect) {
    let columns = Layout::horizontal([Constraint::Min(1), Constraint::Length(22)]).split(area);
    let active = Style::default().fg(BLUE);
    let inactive = Style::default().fg(MUTED);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "dymus",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled("home", if app.home_focused { active } else { inactive }),
            Span::raw("   "),
            Span::styled(
                "explore",
                if app.explore_focused {
                    active
                } else {
                    inactive
                },
            ),
            Span::raw("   "),
            Span::styled(
                "library",
                if app.library_focused {
                    active
                } else {
                    inactive
                },
            ),
            Span::raw("   "),
            Span::styled(
                "songs",
                if app.queue_focused
                    || app.library_focused
                    || app.home_focused
                    || app.explore_focused
                {
                    inactive
                } else {
                    active
                },
            ),
            Span::raw("   "),
            Span::styled(
                format!("queue {}", app.queue.upcoming.len()),
                if app.queue_focused { active } else { inactive },
            ),
        ])),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(if app.queue.current.is_some() {
            "t now playing · ?"
        } else {
            "h/e/l views · ?"
        })
        .alignment(Alignment::Right)
        .style(inactive),
        columns[1],
    );
}

fn playing_view(frame: &mut Frame, app: &mut App, area: Rect) {
    let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    let panes = Layout::horizontal([
        Constraint::Percentage(44),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .split(layout[0]);
    let left = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(panes[0]);
    let right = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(panes[2]);
    let (play_icon, play_color) = match app.playback {
        Playback::Loading => ("…", YELLOW),
        Playback::Paused => ("Ⅱ", YELLOW),
        Playback::Failed => ("!", RED),
        Playback::Playing => ("▶", GREEN),
        Playback::Idle => ("·", MUTED),
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(play_icon, Style::default().fg(play_color)),
            Span::styled("  now playing", Style::default().fg(TEXT)),
        ])),
        left[0],
    );
    if let Some(track) = app.queue.current.as_ref() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    track.title.as_str(),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    track.artist.as_str(),
                    Style::default().fg(SECONDARY),
                )),
                Line::from(Span::styled(
                    track.album.as_str(),
                    Style::default().fg(MUTED),
                )),
            ]),
            left[1],
        );
        if let Some(cover) = &mut app.cover_art {
            if app.image_picker.protocol_type() == ratatui_image::picker::ProtocolType::Halfblocks {
                frame.render_widget(
                    Paragraph::new("terminal image protocol unavailable")
                        .style(Style::default().fg(MUTED)),
                    left[2],
                );
            } else {
                render_cover(frame, cover, &app.image_picker, left[2]);
            }
        } else {
            let message = if app.cover_loading {
                "cover · loading…"
            } else {
                "cover unavailable"
            };
            frame.render_widget(
                Paragraph::new(message).style(Style::default().fg(MUTED)),
                left[2],
            );
        }
        frame.render_widget(
            Paragraph::new(progress_bar(
                app.position,
                app.duration,
                left[3].width as usize,
            ))
            .style(Style::default().fg(BLUE)),
            left[3],
        );
        let duration = if app.duration > 0.0 {
            time(app.duration)
        } else if track.duration.is_empty() {
            "—".into()
        } else {
            track.duration.clone()
        };
        frame.render_widget(
            Paragraph::new(format!(
                "{} / {}     vol {}",
                time(app.position),
                duration,
                app.volume
            ))
            .style(Style::default().fg(MUTED)),
            left[4],
        );
    }
    let tabs = [
        ("q queue", app.now_panel == crate::app::NowPanel::Queue),
        (
            "v visualizer",
            app.now_panel == crate::app::NowPanel::Visualizer,
        ),
        ("y lyrics", app.now_panel == crate::app::NowPanel::Lyrics),
    ];
    let header = if right[0].width < 34 {
        let (key, label) = match app.now_panel {
            crate::app::NowPanel::Queue => ("q", "queue"),
            crate::app::NowPanel::Visualizer => ("v", "visualizer"),
            crate::app::NowPanel::Lyrics => ("y", "lyrics"),
        };
        Line::from(Span::styled(
            format!("{key}  {label}"),
            Style::default().fg(BLUE).add_modifier(Modifier::BOLD),
        ))
    } else {
        tabs.into_iter()
            .map(|(text, active)| {
                Span::styled(
                    format!("{text}  "),
                    if active {
                        Style::default().fg(BLUE).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(MUTED)
                    },
                )
            })
            .collect::<Line>()
    };
    frame.render_widget(Paragraph::new(header), right[0]);
    let subheading = match app.now_panel {
        crate::app::NowPanel::Queue if right[1].width >= 34 => format!(
            "up next · {} · enter play · . actions",
            app.queue.upcoming.len()
        ),
        crate::app::NowPanel::Queue => format!("up next · {}", app.queue.upcoming.len()),
        crate::app::NowPanel::Visualizer => match app.audio_available {
            true => format!("{} · signal live · m / [ ]", app.visualizer_mode.label()),
            false if app.audio_capture_error.is_some() => {
                format!("{} · no PipeWire signal", app.visualizer_mode.label())
            }
            false => format!("{} · waiting for signal…", app.visualizer_mode.label()),
        },
        crate::app::NowPanel::Lyrics => match &app.lyrics {
            Some(lyrics) if lyrics.has_synced() && lyrics.plain.is_some() => {
                if app.lyrics_plain {
                    "plain lyrics · p synced".into()
                } else {
                    "synced lyrics · p plain".into()
                }
            }
            Some(lyrics) if lyrics.has_synced() => "synced lyrics · follows playback".into(),
            Some(_) => "plain lyrics · j/k scroll".into(),
            None if app.lyrics_loading => "lyrics · searching LRCLIB…".into(),
            None if app.lyrics_error.is_some() => "lyrics · lookup failed".into(),
            None => "lyrics · not found".into(),
        },
    };
    let subheading = if right[1].width < 24 {
        format!("{subheading} · tab")
    } else {
        subheading
    };
    frame.render_widget(
        Paragraph::new(subheading).style(Style::default().fg(MUTED)),
        right[1],
    );
    match app.now_panel {
        crate::app::NowPanel::Queue => {
            if app.queue.upcoming.is_empty() {
                frame.render_widget(
                    Paragraph::new("Queue is empty").style(Style::default().fg(MUTED)),
                    right[2],
                );
            } else {
                tracks(
                    frame,
                    app.queue.upcoming.iter(),
                    &mut app.queue_state,
                    &app.queue_marks,
                    true,
                    right[2],
                );
            }
        }
        crate::app::NowPanel::Visualizer => render_visualizer(frame, app, right[2]),
        crate::app::NowPanel::Lyrics => render_lyrics(frame, app, right[2]),
    }
    let mut controls = vec![
        Span::styled("space", Style::default().fg(BLUE)),
        Span::styled(" pause   ", Style::default().fg(MUTED)),
        Span::styled("n", Style::default().fg(BLUE)),
        Span::styled(" next   ", Style::default().fg(MUTED)),
        Span::styled("←/→", Style::default().fg(BLUE)),
        Span::styled(" seek   ", Style::default().fg(MUTED)),
        Span::styled("−/+", Style::default().fg(BLUE)),
        Span::styled(" volume   ", Style::default().fg(MUTED)),
        Span::styled("tab", Style::default().fg(BLUE)),
        Span::styled(" switch panel   ", Style::default().fg(MUTED)),
        Span::styled("esc", Style::default().fg(BLUE)),
        Span::styled(" back", Style::default().fg(MUTED)),
    ];
    if app.playback == Playback::Failed {
        controls.extend([
            Span::styled("   r", Style::default().fg(BLUE)),
            Span::styled(" retry", Style::default().fg(MUTED)),
        ]);
    }
    frame.render_widget(Paragraph::new(Line::from(controls)), layout[1]);
}

fn render_visualizer(frame: &mut Frame, app: &App, area: Rect) {
    let seed = app
        .queue
        .current
        .as_ref()
        .map(|track| {
            track.id.bytes().fold(0u32, |hash, byte| {
                hash.wrapping_mul(31).wrapping_add(byte as u32)
            })
        })
        .unwrap_or(0) as f64
        * 0.001;
    let phase = app.position + app.visualizer_phase;
    let bands_data = app.audio_bands.clone();
    let waveform = app.audio_waveform.clone();
    let scope = app.audio_scope.clone();
    let history: Vec<Vec<f32>> = app.spectrogram_history.iter().cloned().collect();
    let rms = app.audio_rms;
    let has_audio = app.audio_available;
    let color = BLUE;
    let accent = Color::Rgb(0xbb, 0x9a, 0xf7);
    let mode = app.visualizer_mode;
    let marker = if mode == VisualizerMode::Bars {
        Marker::HalfBlock
    } else {
        Marker::Braille
    };
    frame.render_widget(
        Canvas::default()
            .marker(marker)
            .x_bounds([0.0, 100.0])
            .y_bounds([-1.0, 1.0])
            .paint(move |ctx| match mode {
                VisualizerMode::Spectrum => {
                    let count = if has_audio && !bands_data.is_empty() {
                        bands_data.len()
                    } else {
                        52
                    };
                    for band in 0..count {
                        let x = (band as f64 + 0.5) * 100.0 / count as f64;
                        let frequency = band as f64 / count as f64;
                        let height = if has_audio && !bands_data.is_empty() {
                            0.04 + bands_data[band] as f64 * 0.92
                        } else {
                            let envelope = 0.16 + 0.72 * (1.0 - frequency * 0.72);
                            let wobble =
                                (phase * (1.3 + frequency * 2.8) + seed + band as f64 * 0.73)
                                    .sin()
                                    .abs();
                            let overtone = (phase * 0.47 + band as f64 * 1.61 + seed).cos().abs();
                            0.12 + envelope * (0.58 * wobble + 0.42 * overtone)
                        };
                        ctx.draw(&CanvasLine::new(x, -height, x, height, color));
                    }
                    ctx.draw(&CanvasLine::new(0.0, 0.0, 100.0, 0.0, MUTED));
                }
                VisualizerMode::Waveform => {
                    let mut previous = None;
                    let count = if has_audio && !waveform.is_empty() {
                        waveform.len()
                    } else {
                        520
                    };
                    for index in 0..count {
                        let denominator = (count - 1).max(1) as f64;
                        let x = index as f64 * 100.0 / denominator;
                        let t = index as f64 / denominator;
                        let y = if has_audio && !waveform.is_empty() {
                            waveform[index] as f64 * 0.9
                        } else {
                            0.55 * (t * 18.0 - phase * 4.0 + seed).sin()
                                + 0.22 * (t * 39.0 + phase * 2.1 + seed * 1.7).sin()
                        };
                        if let Some((px, py)) = previous {
                            ctx.draw(&CanvasLine::new(px, py, x, y, color));
                        }
                        previous = Some((x, y));
                    }
                    ctx.draw(&CanvasLine::new(0.0, 0.0, 100.0, 0.0, MUTED));
                }
                VisualizerMode::Orbit => {
                    let points: Vec<(f64, f64)> = (0..1200)
                        .map(|i| {
                            let t = i as f64 / 1200.0 * std::f64::consts::TAU * 5.0;
                            (
                                50.0 + 38.0 * (t + phase * 0.8 + seed).sin(),
                                0.78 * if has_audio {
                                    0.25 + rms as f64 * 0.75
                                } else {
                                    1.0
                                } * (t * 1.37 + phase * 0.53).sin(),
                            )
                        })
                        .collect();
                    ctx.draw(&Points::new(&points, accent));
                }
                VisualizerMode::Pulse => {
                    let pulse = if has_audio {
                        rms.clamp(0.0, 1.0) as f64
                    } else {
                        (phase * 2.1 + seed).sin() * 0.5 + 0.5
                    };
                    for ring in 0..4 {
                        let radius = 8.0 + ring as f64 * 9.0 + pulse * 8.0;
                        let color = if ring == 0 { accent } else { color };
                        ctx.draw(&Circle::new(50.0, 0.0, radius, color));
                    }
                }
                VisualizerMode::Spectrogram => {
                    let palette = [
                        Color::Rgb(0x24, 0x3b, 0x6b),
                        Color::Rgb(0x3d, 0x59, 0xa1),
                        Color::Rgb(0x7a, 0xa2, 0xf7),
                        Color::Rgb(0xbb, 0x9a, 0xf7),
                        Color::Rgb(0xf7, 0x76, 0x8e),
                        Color::Rgb(0xff, 0xc7, 0x77),
                    ];
                    let mut buckets: [Vec<(f64, f64)>; 6] =
                        std::array::from_fn(|_| Vec::with_capacity(400));
                    for row in 0usize..24 {
                        let history_row = if history.len() < 24 {
                            row.checked_sub(24 - history.len())
                        } else {
                            Some(history.len() - 24 + row)
                        };
                        for band in 0..64 {
                            let frequency = band as f64 / 64.0;
                            let x = (band as f64 + 0.5) * 100.0 / 64.0;
                            let y = 0.92 - row as f64 * 1.84 / 23.0;
                            let intensity = if has_audio {
                                history_row
                                    .and_then(|history_row| history.get(history_row))
                                    .map(|row| {
                                        let source_band = band * row.len() / 64;
                                        row.get(source_band).copied().unwrap_or_default() as f64
                                    })
                                    .unwrap_or_default()
                            } else {
                                let age = row as f64 * 0.19;
                                let carrier = (phase * (0.8 + frequency * 2.5) - age
                                    + seed
                                    + band as f64 * 0.31)
                                    .sin()
                                    .abs();
                                let overtone =
                                    (phase * 0.37 + age * 1.8 + band as f64 * 0.11).cos().abs();
                                (carrier * 0.66 + overtone * 0.34) * (1.0 - frequency * 0.25)
                            };
                            let bucket = (intensity * palette.len() as f64)
                                .floor()
                                .min((palette.len() - 1) as f64)
                                as usize;
                            buckets[bucket].push((x, y));
                        }
                    }
                    for (points, color) in buckets.iter().zip(palette) {
                        ctx.draw(&Points::new(points, color));
                    }
                }
                VisualizerMode::Vectorscope => {
                    let traces = [color, accent, GREEN];
                    if has_audio && !scope.is_empty() {
                        let points: Vec<(f64, f64)> = scope
                            .iter()
                            .map(|(left, right)| (50.0 + *left as f64 * 44.0, *right as f64 * 0.88))
                            .collect();
                        ctx.draw(&Points::new(&points, accent));
                    } else {
                        for (trace, color) in traces.into_iter().enumerate() {
                            let points: Vec<(f64, f64)> = (0..1400)
                                .map(|sample| {
                                    let t = sample as f64 / 1400.0 * std::f64::consts::TAU;
                                    let offset = trace as f64 * 0.17 + seed;
                                    let x =
                                        (t * (2.0 + trace as f64 * 0.5) + phase * 0.37 + offset)
                                            .sin()
                                            * (0.72 + 0.21 * (t * 7.0 - phase).cos());
                                    let y = (t * (3.0 + trace as f64 * 0.5) - phase * 0.29
                                        + offset)
                                        .sin()
                                        * (0.72 + 0.2 * (t * 5.0 + phase).cos());
                                    (50.0 + x * 44.0, y * 0.88)
                                })
                                .collect();
                            ctx.draw(&Points::new(&points, color));
                        }
                    }
                }
                VisualizerMode::Constellation => {
                    let nodes: Vec<(f64, f64)> = (0..96)
                        .map(|index| {
                            let i = index as f64;
                            let angle = i * 2.399_963_229_728_653 + seed;
                            let radius = 8.0 + (i * 17.13 + seed).sin().abs() * 38.0;
                            (
                                50.0 + radius * (angle + phase * 0.08).cos(),
                                (radius / 52.0) * (angle * 1.7 - phase * 0.11).sin() * 0.9,
                            )
                        })
                        .collect();
                    for a in 0..nodes.len() {
                        for b in a + 1..nodes.len() {
                            let dx = nodes[a].0 - nodes[b].0;
                            let dy = nodes[a].1 - nodes[b].1;
                            let distance = (dx * dx + dy * dy).sqrt();
                            if distance < 15.0 {
                                let edge_color = if distance < 8.0 { accent } else { color };
                                ctx.draw(&CanvasLine::new(
                                    nodes[a].0, nodes[a].1, nodes[b].0, nodes[b].1, edge_color,
                                ));
                            }
                        }
                    }
                    ctx.draw(&Points::new(&nodes, Color::Rgb(0xc0, 0xca, 0xf5)));
                }
                VisualizerMode::Bars | VisualizerMode::BrailleBars => {
                    let bands = if mode == VisualizerMode::Bars { 30 } else { 72 };
                    for band in 0..bands {
                        let frequency = band as f64 / bands as f64;
                        let x = (band as f64 + 0.5) * 100.0 / bands as f64;
                        let height = if has_audio && !bands_data.is_empty() {
                            0.04 + bands_data[band * bands_data.len() / bands] as f64 * 0.92
                        } else {
                            let swell =
                                (phase * (0.9 + frequency * 1.8) + seed + band as f64 * 0.37)
                                    .sin()
                                    .abs();
                            let transient = (phase * 0.31 - band as f64 * 0.13 + seed).cos().abs();
                            0.08 + (0.25 + 0.65 * (1.0 - frequency * 0.5))
                                * (0.72 * swell + 0.28 * transient)
                        };
                        let band_color = if frequency > 0.72 {
                            accent
                        } else if frequency < 0.28 {
                            GREEN
                        } else {
                            color
                        };
                        ctx.draw(&CanvasLine::new(x, -height, x, height, band_color));
                    }
                    ctx.draw(&CanvasLine::new(0.0, 0.0, 100.0, 0.0, MUTED));
                }
            }),
        area,
    );
}

fn render_lyrics(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(lyrics) = &app.lyrics else {
        let message = if app.lyrics_loading {
            "Searching LRCLIB…"
        } else if let Some(error) = &app.lyrics_error {
            error.as_str()
        } else {
            "No lyrics found"
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(MUTED)),
            area,
        );
        return;
    };
    let lines = lyrics.synced_lines();
    if !app.lyrics_plain && !lines.is_empty() {
        let active = crate::lyrics::active_line(&lines, app.position);
        let height = area.height as usize;
        let start = active
            .unwrap_or(0)
            .saturating_sub(height / 2)
            .min(lines.len().saturating_sub(height));
        let content: Vec<Line> = lines
            .iter()
            .enumerate()
            .skip(start)
            .take(height)
            .map(|(i, line)| {
                let spans = if Some(i) == active {
                    vec![
                        Span::styled("▌ ", Style::default().fg(BLUE)),
                        Span::styled(
                            line.text.as_str(),
                            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                        ),
                    ]
                } else if active.is_some_and(|current| i < current) {
                    vec![Span::styled(
                        line.text.as_str(),
                        Style::default().fg(Color::Rgb(0x58, 0x61, 0x7e)),
                    )]
                } else {
                    let distance = active.map_or(i, |current| i.saturating_sub(current));
                    let color = if distance <= 2 { TEXT } else { SECONDARY };
                    vec![Span::styled(line.text.as_str(), Style::default().fg(color))]
                };
                let mut spans = spans;
                if line.text.is_empty() {
                    spans = vec![Span::styled("♪", Style::default().fg(MUTED))];
                }
                Line::from(spans)
            })
            .collect();
        frame.render_widget(Paragraph::new(content).wrap(Wrap { trim: true }), area);
    } else if let Some(plain) = &lyrics.plain {
        frame.render_widget(
            Paragraph::new(plain.as_str())
                .style(Style::default().fg(TEXT))
                .wrap(Wrap { trim: false })
                .scroll((app.lyrics_scroll, 0)),
            area,
        );
    } else {
        frame.render_widget(
            Paragraph::new("Plain lyrics unavailable").style(Style::default().fg(MUTED)),
            area,
        );
    }
}

fn render_cover(
    frame: &mut Frame,
    cover: &mut crate::app::CoverArt,
    picker: &ratatui_image::picker::Picker,
    area: Rect,
) {
    let size = area.as_size();
    if cover.protocol_area != Some(size) {
        cover.protocol = picker
            .new_protocol(cover.image.clone(), size, Resize::Fit(None))
            .ok();
        cover.protocol_area = Some(size);
    }
    if let Some(protocol) = &cover.protocol {
        frame.render_widget(Image::new(protocol), area);
    }
}

fn progress_bar(position: f64, duration: f64, width: usize) -> String {
    let width = width.max(1);
    let ratio = if duration > 0.0 {
        (position / duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (ratio * width as f64).round() as usize;
    format!(
        "{}{}",
        "━".repeat(filled),
        "─".repeat(width.saturating_sub(filled))
    )
}

fn discovery(frame: &mut Frame, app: &mut App, area: Rect) {
    let detail = app.home_focused || app.explore_focused;
    let tabs = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
    let title = if app.home_focused {
        "home · recommendations"
    } else {
        "explore · new music"
    };
    frame.render_widget(
        Paragraph::new(title).style(Style::default().fg(MUTED)),
        tabs[0],
    );
    if app.discovery_loading || app.library_loading {
        frame.render_widget(
            Paragraph::new("Loading…").style(Style::default().fg(MUTED)),
            tabs[1],
        );
        return;
    }
    if detail && app.content_detail {
        if app.results.is_empty() {
            frame.render_widget(
                Paragraph::new("No tracks").style(Style::default().fg(MUTED)),
                tabs[1],
            );
        } else {
            tracks(
                frame,
                app.results.iter(),
                &mut app.results_state,
                &app.result_marks,
                true,
                tabs[1],
            );
        }
        return;
    }
    if app.discovery_items.is_empty() {
        let message = if app.status.is_empty() {
            "No suggestions available"
        } else {
            ""
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(MUTED)),
            tabs[1],
        );
        return;
    }
    let rows = app.discovery_items.iter().map(|item| {
        Row::new(vec![
            Cell::from(item.title.as_str()),
            Cell::from(item.detail.as_str()).style(Style::default().fg(SECONDARY)),
        ])
    });
    let table = Table::new(rows, [Constraint::Fill(2), Constraint::Fill(1)])
        .column_spacing(2)
        .highlight_symbol("› ")
        .row_highlight_style(Style::default().fg(BLUE).add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(table, tabs[1], &mut app.discovery_state);
}

fn library(frame: &mut Frame, app: &mut App, area: Rect) {
    let (label, selected) = match app.library_kind {
        crate::innertube::LibraryKind::Playlists => ("playlists", 0),
        crate::innertube::LibraryKind::Liked => ("liked songs", 1),
        crate::innertube::LibraryKind::Albums => ("albums", 2),
        crate::innertube::LibraryKind::Artists => ("artists", 3),
    };
    let tabs = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
    let names = ["1 playlists", "2 liked", "3 albums", "4 artists"];
    frame.render_widget(
        Paragraph::new(
            names
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    Span::styled(
                        format!("{}{}  ", n, if i == selected { " ·" } else { "" }),
                        if i == selected {
                            Style::default().fg(BLUE)
                        } else {
                            Style::default().fg(MUTED)
                        },
                    )
                })
                .collect::<Line>(),
        )
        .style(Style::default()),
        tabs[0],
    );
    if app.library_loading {
        frame.render_widget(
            Paragraph::new("Loading…").style(Style::default().fg(MUTED)),
            tabs[1],
        );
        return;
    }
    if app.library_detail {
        if app.results.is_empty() {
            frame.render_widget(
                Paragraph::new("No tracks").style(Style::default().fg(MUTED)),
                tabs[1],
            );
        } else {
            tracks(
                frame,
                app.results.iter(),
                &mut app.results_state,
                &app.result_marks,
                true,
                tabs[1],
            );
        }
        return;
    }
    if app.library_items.is_empty() {
        let message = if app.status.is_empty() {
            format!("No {label}")
        } else {
            "".into()
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(MUTED)),
            tabs[1],
        );
        return;
    }
    let table = Table::new(
        app.library_items.iter().map(|item| {
            Row::new(vec![
                Cell::from(item.title.as_str()),
                Cell::from(item.detail.as_str()).style(Style::default().fg(SECONDARY)),
            ])
        }),
        [Constraint::Fill(2), Constraint::Fill(1)],
    )
    .column_spacing(2)
    .highlight_symbol("› ")
    .row_highlight_style(Style::default().fg(BLUE).add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(table, tabs[1], &mut app.library_state);
}

fn search(frame: &mut Frame, app: &App, area: Rect) {
    let query = if app.editing {
        app.input.as_str()
    } else {
        app.query.as_str()
    };
    // Leave space for the prompt and cursor. Trim only at UTF-8 boundaries.
    let width = area.width.saturating_sub(3) as usize;
    let mut visible = query;
    while Line::from(visible).width() > width {
        visible = &visible[visible.chars().next().map(char::len_utf8).unwrap_or(0)..];
    }
    let placeholder = query.is_empty();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "/ ",
                Style::default().fg(if app.editing { BLUE } else { MUTED }),
            ),
            Span::styled(
                if placeholder {
                    "Search songs or artists"
                } else {
                    visible
                },
                Style::default().fg(if placeholder || !app.editing {
                    MUTED
                } else {
                    TEXT
                }),
            ),
        ])),
        area,
    );
    if app.editing {
        frame.set_cursor_position((area.x + 2 + Line::from(visible).width() as u16, area.y));
    }
}

fn tracks<'a>(
    frame: &mut Frame,
    tracks: impl Iterator<Item = &'a Track>,
    state: &mut TableState,
    marks: &BTreeSet<usize>,
    focused: bool,
    area: Rect,
) {
    let rows = tracks.enumerate().map(|(index, track)| {
        Row::new(vec![
            Cell::from(Line::from(vec![
                Span::styled(
                    if marks.is_empty() {
                        ""
                    } else if marks.contains(&index) {
                        "● "
                    } else {
                        "  "
                    },
                    Style::default().fg(GREEN),
                ),
                Span::raw(track.title.as_str()),
            ])),
            Cell::from(track.artist.as_str()).style(Style::default().fg(SECONDARY)),
            Cell::from(Line::from(track.duration.as_str()).alignment(Alignment::Right))
                .style(Style::default().fg(MUTED)),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Fill(3),
            Constraint::Fill(2),
            Constraint::Length(5),
        ],
    )
    .column_spacing(2)
    .row_highlight_style(if focused {
        Style::default().fg(BLUE).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    })
    .highlight_symbol(if focused { "› " } else { "  " });
    frame.render_stateful_widget(table, area, state);
}

fn now_playing(frame: &mut Frame, app: &App, area: Rect) {
    let Some(track) = &app.queue.current else {
        return;
    };
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
    let (state, color) = match app.playback {
        Playback::Loading => ("…", YELLOW),
        Playback::Paused => ("Ⅱ", YELLOW),
        Playback::Failed => ("!", RED),
        _ => ("▶", GREEN),
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{state} "), Style::default().fg(color)),
            Span::raw(track.title.as_str()),
            Span::styled(
                format!(" — {}", track.artist),
                Style::default().fg(SECONDARY),
            ),
        ])),
        rows[0],
    );
    let columns = Layout::horizontal([Constraint::Min(1), Constraint::Length(8)]).split(rows[1]);
    let duration = if app.duration > 0.0 {
        time(app.duration)
    } else if !track.duration.is_empty() {
        track.duration.clone()
    } else {
        "—".into()
    };
    let timing = match app.playback {
        Playback::Loading => "  loading…".into(),
        Playback::Failed => "  r retry · n skip".into(),
        _ => format!("  {} / {duration}", time(app.position)),
    };
    frame.render_widget(
        Paragraph::new(timing).style(Style::default().fg(MUTED)),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(format!("vol {}", app.volume))
            .alignment(Alignment::Right)
            .style(Style::default().fg(MUTED)),
        columns[1],
    );
}

fn time(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn actions(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(area);
    let header = Layout::horizontal([Constraint::Min(1), Constraint::Length(9)]).split(rows[0]);
    let title = if app.marks().is_empty() {
        "actions".to_owned()
    } else {
        format!("actions · {} selected", app.marks().len())
    };
    frame.render_widget(
        Paragraph::new(title).style(Style::default().fg(TEXT)),
        header[0],
    );
    frame.render_widget(
        Paragraph::new("esc close")
            .alignment(Alignment::Right)
            .style(Style::default().fg(MUTED)),
        header[1],
    );
    let items = app.menu_items();
    let table = Table::new(
        items.iter().map(|item| {
            Row::new(vec![
                Cell::from(item.key).style(Style::default().fg(MUTED)),
                Cell::from(item.label),
            ])
        }),
        [Constraint::Length(5), Constraint::Min(1)],
    )
    .column_spacing(1)
    .highlight_symbol("› ")
    .row_highlight_style(Style::default().fg(BLUE).add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(table, rows[2], &mut app.menu_state);
}

pub const SHORTCUTS: &[(&str, &str)] = &[
    ("/ · Enter", "Search / play selection"),
    ("j/k · ↑/↓", "Move selection"),
    ("g/G", "First / last song"),
    ("Tab · .", "Views / actions"),
    ("x/v", "Mark song / select all"),
    ("Esc", "Clear / cancel radio"),
    ("a/A", "Queue selection / next"),
    ("P/Q", "Play all / queue all"),
    ("R", "Radio from focused song"),
    ("d · J/K", "Remove / move selection"),
    ("C", "Clear upcoming queue"),
    ("Space · n/r", "Pause · next / retry"),
    ("h/e/l · 1–4", "Home / Explore / Library sections"),
    ("Enter · Esc", "Open an item / return from its detail"),
    ("t · q/v/y · Tab", "Now-playing view · switch right pane"),
    ("Shift+Tab", "Previous Now-playing panel"),
    ("m · [/]", "Cycle now-playing visualizer styles"),
    ("p · j/k", "Switch / scroll lyrics formats"),
    ("←/→ · -/+", "Seek 5s / volume"),
    ("Ctrl+U", "Clear search"),
    ("q · Ctrl+C", "Quit"),
];

fn help(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(area);
    let header = Layout::horizontal([Constraint::Min(1), Constraint::Length(9)]).split(rows[0]);
    let title = if (rows[2].height as usize) < SHORTCUTS.len() {
        "shortcuts · j/k scroll"
    } else {
        "shortcuts"
    };
    frame.render_widget(
        Paragraph::new(title).style(Style::default().fg(TEXT)),
        header[0],
    );
    frame.render_widget(
        Paragraph::new("esc close")
            .alignment(Alignment::Right)
            .style(Style::default().fg(MUTED)),
        header[1],
    );
    app.help_scroll = app
        .help_scroll
        .min(SHORTCUTS.len().saturating_sub(rows[2].height as usize));
    frame.render_widget(
        Table::new(
            SHORTCUTS.iter().skip(app.help_scroll).map(|(key, action)| {
                Row::new(vec![
                    Cell::from(*key).style(Style::default().fg(BLUE)),
                    Cell::from(*action).style(Style::default().fg(SECONDARY)),
                ])
            }),
            [Constraint::Length(12), Constraint::Min(1)],
        )
        .column_spacing(1),
        rows[2],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::track;
    use ratatui::{Terminal, backend::TestBackend};

    #[tokio::test]
    async fn renders_wide_narrow_and_tiny_terminals() {
        let mut app = App::new().await.unwrap();
        app.results.push(track("Song title"));
        app.results_state.select(Some(0));
        app.queue.upcoming.push_back(track("Queued song"));
        for (width, height) in [(120, 32), (60, 24), (40, 14), (20, 5)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            app.queue_focused = true;
            app.help = true;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            app.help = false;
        }
    }

    #[tokio::test]
    async fn marks_actions_and_scrolled_help_fit_without_solid_backgrounds() {
        let mut app = App::new().await.unwrap();
        app.editing = false;
        app.results = vec![track("One"), track("Two")];
        app.results_state.select(Some(0));
        app.result_marks.insert(1);
        let mut terminal = Terminal::new(TestBackend::new(40, 14)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("1 selected"));
        assert!(text.contains("● Two"));
        app.menu = true;
        app.menu_state.select(Some(0));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Radio · replace queue"));
        assert!(text.contains("Play all · replace queue"));
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .all(|cell| cell.bg == Color::Reset)
        );
        app.menu = false;
        app.help = true;
        app.help_scroll = SHORTCUTS.len();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains("Quit"));
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[tokio::test]
    async fn long_unicode_search_stays_within_terminal() {
        let mut app = App::new().await.unwrap();
        app.input = "音楽é🎵".repeat(100);
        let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    }

    #[tokio::test]
    async fn now_playing_layout_renders_cover_tabs_and_queue() {
        let mut app = App::new().await.unwrap();
        app.now_playing_view = true;
        app.queue.current = Some(track("Current track"));
        app.queue.upcoming.push_back(track("Next track"));
        app.cover_art = Some(crate::app::CoverArt {
            image: image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(20, 20, |x, y| {
                image::Rgb([x as u8, y as u8, 120])
            })),
            protocol: None,
            protocol_area: None,
        });
        app.image_picker
            .set_protocol_type(ratatui_image::picker::ProtocolType::Kitty);
        let mut terminal = Terminal::new(TestBackend::new(88, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Current track"));
        assert!(text.contains("Next track"));
        assert!(text.contains("visualizer"));
        assert!(text.contains("lyrics"));
        app.now_panel = crate::app::NowPanel::Visualizer;
        app.audio_available = true;
        app.audio_bands = vec![0.4; 48];
        app.audio_waveform = vec![0.15; 256];
        app.audio_scope = vec![(0.2, -0.2); 192];
        app.audio_rms = 0.3;
        app.spectrogram_history
            .extend([vec![0.2; 48], vec![0.6; 48], vec![0.3; 48]]);
        for mode in [
            VisualizerMode::Spectrum,
            VisualizerMode::Waveform,
            VisualizerMode::Orbit,
            VisualizerMode::Pulse,
            VisualizerMode::Spectrogram,
            VisualizerMode::Vectorscope,
            VisualizerMode::Constellation,
            VisualizerMode::Bars,
            VisualizerMode::BrailleBars,
        ] {
            app.visualizer_mode = mode;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(buffer_text(terminal.backend().buffer()).contains(mode.label()));
        }
        app.image_picker
            .set_protocol_type(ratatui_image::picker::ProtocolType::Halfblocks);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(
            buffer_text(terminal.backend().buffer())
                .contains("terminal image protocol unavailable")
        );
    }

    #[tokio::test]
    async fn synced_lyrics_distinguish_elapsed_current_and_upcoming_lines() {
        let mut app = App::new().await.unwrap();
        app.now_playing_view = true;
        app.now_panel = crate::app::NowPanel::Lyrics;
        app.position = 3.0;
        app.queue.current = Some(track("Now playing"));
        app.lyrics = Some(
            serde_json::from_str(
                r#"{"plainLyrics":"Past line\nCurrent line\nFuture line","syncedLyrics":"[00:00.00]Past line\n[00:02.00]Current line\n[00:04.00]Future line"}"#,
            )
            .unwrap(),
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let cell = |letter: &str| {
            buffer
                .content
                .iter()
                .find(|cell| cell.symbol() == letter)
                .unwrap()
        };
        assert_eq!(cell("P").fg, Color::Rgb(0x58, 0x61, 0x7e));
        assert_eq!(cell("C").fg, TEXT);
        assert!(cell("C").modifier.contains(Modifier::BOLD));
        assert_eq!(cell("F").fg, TEXT);
    }
}
