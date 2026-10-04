//! Terminal feedback for headless playback, without taking over the screen.
use anyhow::{Context, Result, ensure};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    style::{Color, Stylize},
    terminal,
};
use std::{
    fmt,
    future::Future,
    io::{self, IsTerminal, Write},
};
use tokio::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug)]
pub struct Cancelled;
impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Cancelled")
    }
}
impl std::error::Error for Cancelled {}

#[derive(Clone, Debug)]
pub struct Feedback {
    title: String,
    detail: String,
    hint: String,
    empty: bool,
}

impl Feedback {
    pub fn empty(
        title: impl Into<String>,
        detail: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
            hint: hint.into(),
            empty: true,
        }
    }

    fn from_error(error: &anyhow::Error) -> Self {
        if let Some(feedback) = error.downcast_ref::<Self>() {
            return feedback.clone();
        }
        let detail = format!("{error:#}");
        let lower = detail.to_lowercase();
        let network = error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
            .any(|cause| cause.is_connect());
        let timeout = error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
            .any(|cause| cause.is_timeout());
        let status = error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
            .find_map(reqwest::Error::status);
        let (title, hint) = if status == Some(reqwest::StatusCode::FORBIDDEN)
            || lower.contains("403 forbidden")
            || lower.contains("http error 403")
        {
            (
                "YouTube rejected the stream",
                "Update yt-dlp and retry. If the track still fails, try another track or refresh your sign-in with `dymus auth browser`.",
            )
        } else if lower.contains("sign in") && lower.contains("bot") {
            (
                "YouTube needs a browser session",
                "Sign in with `dymus auth browser`, then retry.",
            )
        } else if status == Some(reqwest::StatusCode::UNAUTHORIZED)
            || lower.contains("401 unauthorized")
            || lower.contains("requires sign-in")
            || lower.contains("credentials")
            || lower.contains("auth file")
            || lower.contains("saved youtube cookie")
        {
            (
                "YouTube sign-in needs attention",
                "Sign in again with `dymus auth browser` or `dymus auth paste`, then retry.",
            )
        } else if timeout || lower.contains("timed out") || lower.contains("did not start within") {
            (
                "The request took too long",
                "Check your connection and retry, or choose another track.",
            )
        } else if network
            || lower.contains("failed to resolve")
            || lower.contains("network is unreachable")
        {
            (
                "Could not reach the music service",
                "Check your internet connection and retry.",
            )
        } else if ["mpv", "yt-dlp", "ffmpeg", "ffprobe"]
            .iter()
            .any(|tool| lower.contains(&format!("cannot start {tool}")))
        {
            (
                "A required playback or download tool is unavailable",
                "Run `dymus doctor`, install or repair the missing tool, then retry.",
            )
        } else if lower.contains("local path does not exist")
            || lower.contains("local media is missing")
        {
            (
                "Local media is unavailable",
                "Check the file or directory path, or run `dymus local --json` to see available collections.",
            )
        } else if lower.contains("mpv exited") || lower.contains("mpv could not play") {
            (
                "Playback could not start",
                "Try another track. If playback keeps failing, run `dymus doctor` and check the player error above.",
            )
        } else {
            (
                "Could not complete the request",
                "Check the details above and retry.",
            )
        };
        Self {
            title: title.into(),
            detail,
            hint: hint.into(),
            empty: false,
        }
    }
}

impl fmt::Display for Feedback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} {}", self.title, self.detail, self.hint)
    }
}
impl std::error::Error for Feedback {}

fn feedback_text(feedback: &Feedback) -> String {
    // Preserve the complete explanation in pipes and narrow terminals. Strip
    // terminal controls and signed URLs before displaying external diagnostics.
    let safe = |text: &str| {
        clean(text)
            .split_whitespace()
            .map(|word| {
                if word.contains("https://") || word.contains("http://") {
                    "[stream URL]"
                } else {
                    word
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    format!(
        "\n  {}: {}\n  {}\n\n  {}\n\n",
        if feedback.empty {
            "Nothing to show"
        } else {
            "Error"
        },
        safe(&feedback.title),
        safe(&feedback.detail),
        safe(&feedback.hint)
    )
}

pub fn report_error(error: &anyhow::Error) -> io::Result<()> {
    io::stderr().write_all(feedback_text(&Feedback::from_error(error)).as_bytes())
}

pub fn print_empty(title: &str, detail: &str, hint: &str) -> Result<()> {
    io::stdout().write_all(feedback_text(&Feedback::empty(title, detail, hint)).as_bytes())?;
    Ok(())
}

/// A small inline region: no alternate screen, no accumulated progress log.
#[derive(Default)]
pub struct Block {
    rows: usize,
}

impl Block {
    pub fn clear(&mut self) -> Result<()> {
        if self.rows == 0 {
            return Ok(());
        }
        let mut output = format!("\x1b[{}A", self.rows);
        for _ in 0..self.rows {
            output.push_str("\r\x1b[2K\n");
        }
        output.push_str(&format!("\x1b[{}A\r", self.rows));
        io::stdout().write_all(output.as_bytes())?;
        self.rows = 0;
        Ok(())
    }

    pub fn draw(&mut self, lines: &[Row]) -> Result<()> {
        let interactive = io::stdout().is_terminal();
        let height = terminal::size().map(|(_, h)| usize::from(h)).unwrap_or(24);
        let count = lines.len().min(height.saturating_sub(4).max(1));
        let mut output = if interactive && self.rows > 0 {
            format!("\x1b[{}A", self.rows)
        } else {
            String::new()
        };
        let extent = if interactive {
            count.max(self.rows)
        } else {
            count
        };
        for index in 0..extent {
            if interactive {
                output.push_str("\r\x1b[2K");
            }
            if let Some(row) = lines.get(index).filter(|_| index < count) {
                let text = fit(&format!("  {}", row.text), width().saturating_sub(1));
                if interactive && std::env::var_os("NO_COLOR").is_none() {
                    let styled = text.with(row.color);
                    output.push_str(&if row.bold {
                        styled.bold().to_string()
                    } else {
                        styled.to_string()
                    });
                } else {
                    output.push_str(&text);
                }
            }
            output.push('\n');
        }
        if interactive && extent > count {
            output.push_str(&format!("\x1b[{}A\r", extent - count));
        }
        io::stdout().write_all(output.as_bytes())?;
        io::stdout().flush()?;
        self.rows = if interactive { count } else { 0 };
        Ok(())
    }
}

pub struct Row {
    pub text: String,
    color: Color,
    bold: bool,
}
impl Row {
    pub(crate) fn title(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: Color::Rgb {
                r: 226,
                g: 232,
                b: 240,
            },
            bold: true,
        }
    }
    pub(crate) fn muted(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: Color::Rgb {
                r: 139,
                g: 149,
                b: 167,
            },
            bold: false,
        }
    }
    pub(crate) fn accent(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: Color::Rgb {
                r: 125,
                g: 211,
                b: 218,
            },
            bold: false,
        }
    }
    pub(crate) fn blank() -> Self {
        Self::muted("")
    }
}

pub struct Flow {
    block: Block,
}
impl Flow {
    pub fn new(title: &str, detail: &str) -> Self {
        println!();
        line(
            &format!(
                "  dymus  /  {}",
                title
                    .strip_prefix("Headless ")
                    .unwrap_or(title)
                    .to_lowercase()
            ),
            Color::Rgb {
                r: 125,
                g: 211,
                b: 218,
            },
        );
        println!();
        let mut flow = Self {
            block: Block::default(),
        };
        let _ = flow.block.draw(&[Row::muted(detail)]);
        flow
    }

    pub async fn load<T>(
        &mut self,
        title: &str,
        detail: &str,
        future: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let mut frame = 0;
        let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let mut tick = tokio::time::interval(Duration::from_millis(120));
        tokio::pin!(future);
        let interactive = io::stdout().is_terminal();
        if !interactive {
            self.block.draw(&[Row::muted(title)])?;
        }
        let result = loop {
            tokio::select! {
                result = &mut future => break result,
                signal = tokio::signal::ctrl_c() => {
                    signal.context("Cannot listen for cancellation")?;
                    break Err(Cancelled.into());
                }
                _ = tick.tick(), if interactive => {
                    self.block.draw(&[
                        Row::accent(format!("{}  {title}", frames[frame % frames.len()])),
                        Row::muted(detail),
                    ])?;
                    frame += 1;
                }
            }
        };
        if interactive {
            self.block.clear()?;
        }
        result.with_context(|| format!("{title} failed"))
    }

    pub fn empty<T>(&mut self, title: &str, detail: &str, hint: &str) -> Result<T> {
        self.block.clear()?;
        Err(Feedback::empty(title, detail, hint).into())
    }

    pub fn choose<T: Clone>(
        &mut self,
        title: &str,
        choices: &[T],
        describe: impl Fn(&T) -> String,
    ) -> Result<T> {
        if choices.is_empty() {
            return self.empty(
                "No choices available",
                title,
                "Try another search or collection.",
            );
        }
        ensure!(
            io::stdin().is_terminal() && io::stdout().is_terminal(),
            "{title} requires an interactive terminal; run this command directly in a terminal without piping its input or output"
        );
        let digits = choices.len().to_string().len();
        let descriptions: Vec<_> = choices.iter().map(&describe).collect();
        let rows = descriptions
            .iter()
            .map(|text| text.lines().count())
            .max()
            .unwrap_or(1)
            .max(1)
            + 1;
        let height = terminal::size().map(|(_, h)| usize::from(h)).unwrap_or(24);
        let page_size = (height.saturating_sub(10) / rows).max(1);
        let pages = choices.len().div_ceil(page_size);
        let mut page = 0;
        let mut invalid = false;
        loop {
            let mut view = vec![
                Row::title(title),
                Row::muted(if pages > 1 {
                    format!("{} results  ·  page {} / {pages}", choices.len(), page + 1)
                } else {
                    format!("{} results", choices.len())
                }),
                Row::blank(),
            ];
            for (index, description) in descriptions
                .iter()
                .enumerate()
                .skip(page * page_size)
                .take(page_size)
            {
                let mut rows = description.lines();
                view.push(Row::title(format!(
                    "{:>digits$}  {}",
                    index + 1,
                    rows.next().unwrap_or_default()
                )));
                for detail in rows {
                    view.push(Row::muted(format!("{}{}", " ".repeat(digits + 2), detail)));
                }
                view.push(Row::blank());
            }
            view.push(Row::muted(if invalid {
                format!("Choose 1–{}  ·  q cancel", choices.len())
            } else if pages > 1 {
                "number select  ·  n/p page  ·  q cancel".into()
            } else {
                "number select  ·  q cancel".into()
            }));
            self.block.draw(&view)?;
            let input = read_input("  › ", digits)?;
            self.block.rows += 1;
            match input.trim().to_ascii_lowercase().as_str() {
                "n" if pages > 1 => {
                    page = (page + 1).min(pages - 1);
                    invalid = false;
                    continue;
                }
                "p" if pages > 1 => {
                    page = page.saturating_sub(1);
                    invalid = false;
                    continue;
                }
                _ => {}
            }
            if let Ok(index) = input.trim().parse::<usize>()
                && let Some(choice) = index.checked_sub(1).and_then(|index| choices.get(index))
            {
                self.block.clear()?;
                return Ok(choice.clone());
            }
            invalid = true;
        }
    }

    pub fn snapshot(&mut self, view: &PlaybackView<'_>) -> Result<()> {
        self.block.draw(&playback_rows(view, width()))
    }

    pub fn playing(&mut self, title: &str, artist: &str, detail: &str, detached: bool) {
        let _ = self.block.clear();
        if detached || !io::stdout().is_terminal() {
            let _ = self.block.draw(&[
                Row::title(title),
                Row::muted(artist),
                Row::blank(),
                Row::accent(if detached {
                    "playing in background"
                } else {
                    "playing"
                }),
                Row::muted(detail),
                Row::blank(),
                Row::muted("dymus control status / toggle / next / stop"),
            ]);
        }
    }
}

pub struct PlaybackView<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub position: Option<f64>,
    pub duration: Option<f64>,
    pub paused: bool,
    pub video: bool,
    pub index: usize,
    pub total: usize,
    pub volume: u8,
}

pub fn playback_rows(view: &PlaybackView<'_>, available: usize) -> Vec<Row> {
    let available = available.saturating_sub(3);
    let duration = view
        .duration
        .filter(|value| value.is_finite() && *value > 0.0);
    let position = view
        .position
        .filter(|value| value.is_finite())
        .unwrap_or(0.0)
        .max(0.0);
    let clock = |seconds: f64| {
        let s = seconds.max(0.0) as u64;
        format!("{}:{:02}", s / 60, s % 60)
    };
    let time = format!(
        "{} / {}",
        clock(position),
        duration.map(clock).unwrap_or_else(|| "--:--".into())
    );
    let meter_width = available
        .saturating_sub(UnicodeWidthStr::width(time.as_str()) + 3)
        .min(36);
    let progress = if meter_width >= 4 {
        let fraction = duration
            .map(|seconds| (position / seconds).clamp(0.0, 1.0))
            .unwrap_or(0.0);
        let filled = (fraction * meter_width as f64).floor() as usize;
        format!(
            "{}{}  {time}",
            "━".repeat(filled),
            "─".repeat(meter_width - filled)
        )
    } else {
        time
    };
    let status = if view.paused {
        "paused"
    } else if view.position.is_none() {
        "buffering"
    } else {
        "playing"
    };
    let mut metadata = vec![
        status.to_owned(),
        if view.video {
            "video".into()
        } else {
            "audio".into()
        },
    ];
    if view.total > 1 {
        metadata.push(format!("{} / {}", view.index + 1, view.total));
    }
    metadata.push(format!("vol {}%", view.volume));
    vec![
        Row::title(view.title),
        Row::muted(view.artist),
        Row::blank(),
        Row::accent(progress),
        Row::muted(metadata.join("  ·  ")),
        Row::blank(),
        Row::muted("ctrl+c stop  ·  dymus control toggle / next"),
    ]
    .into_iter()
    .map(|mut row| {
        row.text = fit(&row.text, available);
        row
    })
    .collect()
}

struct RawInput;
impl Drop for RawInput {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

fn read_input(prompt: &str, digits: usize) -> Result<String> {
    terminal::enable_raw_mode().context("Cannot enable terminal input")?;
    let raw = RawInput;
    let mut input = String::new();
    loop {
        let text = fit(&format!("{prompt}{input}"), width().saturating_sub(1));
        print!("\r\x1b[2K{text}");
        io::stdout().flush().context("Cannot write choice prompt")?;
        match event::read().context("Cannot read choice")? {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => {
                    drop(raw);
                    println!();
                    return Err(Cancelled.into());
                }
                KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    drop(raw);
                    println!();
                    return Err(Cancelled.into());
                }
                KeyCode::Char('q' | 'Q') => {
                    drop(raw);
                    println!();
                    return Err(Cancelled.into());
                }
                KeyCode::Enter => {
                    drop(raw);
                    println!();
                    return Ok(input);
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(ch)
                    if input.len() < digits + 1
                        && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    input.push(ch);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

pub fn width() -> usize {
    terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(80)
        .min(88)
}
pub fn clean(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}
pub fn fit(text: &str, available: usize) -> String {
    let text = clean(text);
    if UnicodeWidthStr::width(text.as_str()) <= available {
        return text;
    }
    if available == 0 {
        return String::new();
    }
    let mut used = 0;
    let mut result = String::new();
    for ch in text.chars() {
        let next = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + next > available - 1 {
            break;
        }
        result.push(ch);
        used += next;
    }
    result.push('…');
    result
}
fn line(text: &str, color: Color) {
    let text = fit(text, width().saturating_sub(1));
    if io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        println!("{}", text.with(color));
    } else {
        println!("{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_feedback_survives_loading_context() {
        let error = anyhow::Error::new(Feedback::empty(
            "No playlists found",
            "Nothing matched your query.",
            "Try another title.",
        ))
        .context("Search playlists failed");
        let text = feedback_text(&Feedback::from_error(&error));
        assert!(text.contains("Nothing to show: No playlists found"));
        assert!(text.contains("Nothing matched your query."));
        assert!(text.contains("Try another title."));
        assert!(!text.contains("Error:"));
    }

    #[test]
    fn failures_keep_the_cause_and_offer_specific_recovery() {
        for (cause, title, hint) in [
            (
                "HTTP error 403 Forbidden",
                "YouTube rejected the stream",
                "Update yt-dlp",
            ),
            (
                "Library requires sign-in",
                "YouTube sign-in needs attention",
                "dymus auth browser",
            ),
            (
                "Failed to resolve www.youtube.com",
                "Could not reach the music service",
                "internet connection",
            ),
            (
                "Stream lookup timed out",
                "The request took too long",
                "retry",
            ),
            (
                "Cannot start mpv",
                "A required playback or download tool is unavailable",
                "dymus doctor",
            ),
            (
                "Local path does not exist: /music",
                "Local media is unavailable",
                "Check the file or directory path",
            ),
        ] {
            let error = anyhow::anyhow!(cause).context("Prepare playback failed");
            let text = feedback_text(&Feedback::from_error(&error));
            assert!(text.contains(title));
            assert!(text.contains(cause));
            assert!(text.contains(hint));
            assert!(text.contains("Prepare playback failed"));
        }
    }

    #[test]
    fn feedback_preserves_long_details_and_removes_urls_and_controls() {
        let cause = format!(
            "HTTP error 403 Forbidden\nhttps://example.com/audio?token=secret\x1b[2J {}",
            "details ".repeat(100)
        );
        let text = feedback_text(&Feedback::from_error(&anyhow::anyhow!(cause)));
        assert!(text.contains("403 Forbidden"));
        assert!(text.contains("[stream URL]"));
        assert!(!text.contains("token=secret"));
        assert!(!text.contains('\x1b'));
        assert_eq!(text.matches("details").count(), 100);
    }

    #[tokio::test]
    async fn loading_context_preserves_cancellation() {
        let mut flow = Flow::new("Test", "");
        let error = flow
            .load::<()>("Load tracks", "", async { Err(Cancelled.into()) })
            .await
            .unwrap_err();
        assert!(error.is::<Cancelled>());
    }
    #[test]
    fn playback_card_is_compact_safe_and_adapts_to_terminal_width() {
        let view = PlaybackView {
            title: "Feather\n日本語\x1b[2J",
            artist: "Nujabes",
            position: Some(52.0),
            duration: Some(204.0),
            paused: false,
            video: false,
            index: 1,
            total: 12,
            volume: 65,
        };
        for width in [0, 1, 12, 30, 60, 88] {
            let rows = playback_rows(&view, width);
            assert_eq!(rows.len(), 7);
            for row in &rows {
                assert!(UnicodeWidthStr::width(row.text.as_str()) <= width.saturating_sub(3));
                assert!(!row.text.chars().any(char::is_control));
            }
            if width >= 60 {
                assert!(rows[3].text.ends_with("0:52 / 3:24"));
                assert_eq!(rows[4].text, "playing  ·  audio  ·  2 / 12  ·  vol 65%");
            }
        }
        let paused = PlaybackView {
            paused: true,
            duration: None,
            ..view
        };
        let rows = playback_rows(&paused, 88);
        assert!(rows[3].text.ends_with("0:52 / --:--"));
        assert!(rows[4].text.starts_with("paused"));
    }

    #[test]
    fn feedback_fits_narrow_terminals_without_control_characters() {
        let text = "Long title 🎵 日本語\nwith\rterminal\x1bcontrols".repeat(4);
        for width in [0, 1, 2, 12, 40, 80] {
            let result = fit(&text, width);
            assert!(UnicodeWidthStr::width(result.as_str()) <= width);
            assert!(!result.chars().any(char::is_control));
        }
    }
}
