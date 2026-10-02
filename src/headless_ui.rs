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
use tokio::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug)]
pub struct Cancelled;
impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Cancelled")
    }
}
impl std::error::Error for Cancelled {}

pub struct Flow {
    step: usize,
}
impl Flow {
    pub fn new(title: &str, detail: &str) -> Self {
        println!();
        line(&format!("Dymus · {title}"), Color::Cyan);
        line(detail, Color::DarkGrey);
        Self { step: 0 }
    }

    fn stage(&mut self, title: &str) {
        self.step += 1;
        println!();
        line(&format!("{} · {title}", self.step), Color::Cyan);
    }

    pub async fn load<T>(
        &mut self,
        title: &str,
        detail: &str,
        future: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        self.stage(title);
        line(detail, Color::DarkGrey);
        let started = Instant::now();
        let mut spinner = Loading::new();
        let mut tick = tokio::time::interval(Duration::from_millis(120));
        tokio::pin!(future);
        let result = loop {
            tokio::select! {
                result = &mut future => break result,
                signal = tokio::signal::ctrl_c() => {
                    signal.context("Cannot listen for cancellation")?;
                    break Err(Cancelled.into());
                }
                _ = tick.tick() => spinner.draw(started.elapsed().as_secs())?,
            }
        };
        drop(spinner);
        match &result {
            Ok(_) => line(
                &format!("✓ Ready · {:.1}s", started.elapsed().as_secs_f32()),
                Color::Green,
            ),
            Err(error) if error.is::<Cancelled>() => {}
            Err(_) => line("× This step failed", Color::Red),
        }
        result
    }

    pub fn choose<T: Clone>(
        &mut self,
        title: &str,
        choices: &[T],
        describe: impl Fn(&T) -> String,
    ) -> Result<T> {
        ensure!(
            io::stdin().is_terminal() && io::stdout().is_terminal(),
            "{title} requires an interactive terminal"
        );
        ensure!(!choices.is_empty(), "No choices available");
        self.stage(title);
        let digits = choices.len().to_string().len();
        let descriptions: Vec<_> = choices.iter().map(&describe).collect();
        let rows = descriptions
            .iter()
            .map(|text| text.lines().count())
            .max()
            .unwrap_or(1)
            .max(1);
        let height = terminal::size()
            .map(|(_, height)| usize::from(height))
            .unwrap_or(24);
        let page_size = (height.saturating_sub(8) / rows).max(1);
        let pages = choices.len().div_ceil(page_size);
        let mut page = 0;
        loop {
            if pages > 1 {
                line(
                    &format!("Page {} of {pages} · {} matches", page + 1, choices.len()),
                    Color::DarkGrey,
                );
            }
            for (index, description) in descriptions
                .iter()
                .enumerate()
                .skip(page * page_size)
                .take(page_size)
            {
                let mut rows = description.lines();
                let prefix = format!("{:>digits$}. ", index + 1);
                line(
                    &format!("{prefix}{}", rows.next().unwrap_or_default()),
                    Color::White,
                );
                for detail in rows {
                    line(
                        &format!("{}{}", " ".repeat(digits + 2), detail),
                        Color::DarkGrey,
                    );
                }
            }
            line("Enter number · q/Esc to cancel", Color::DarkGrey);
            if pages > 1 {
                line("n/p + Enter: next/previous page", Color::DarkGrey);
            }
            loop {
                let prompt = fit(
                    &format!("Choose [1–{}]: ", choices.len()),
                    width().saturating_sub(8),
                );
                let input = read_input(&prompt, digits)?;
                match input.trim().to_ascii_lowercase().as_str() {
                    "n" if pages > 1 => {
                        page = (page + 1).min(pages - 1);
                        break;
                    }
                    "p" if pages > 1 => {
                        page = page.saturating_sub(1);
                        break;
                    }
                    _ => {}
                }
                if let Ok(index) = input.trim().parse::<usize>()
                    && let Some(choice) = index.checked_sub(1).and_then(|index| choices.get(index))
                {
                    line(
                        &format!(
                            "✓ {}",
                            descriptions[index - 1].lines().next().unwrap_or_default()
                        ),
                        Color::Green,
                    );
                    return Ok(choice.clone());
                }
                line(
                    &format!("Use a number from 1 to {}, or q to cancel.", choices.len()),
                    Color::Yellow,
                );
            }
        }
    }

    pub fn playing(&mut self, title: &str, artist: &str, detail: &str, detached: bool) {
        self.stage(if detached {
            "Playback detached"
        } else {
            "Now playing"
        });
        line(title, Color::White);
        line(artist, Color::DarkGrey);
        line(detail, Color::DarkGrey);
        if detached {
            line("Playback continues after this command exits.", Color::Green);
        } else {
            line("Ctrl+C to stop", Color::DarkGrey);
            line("Control from another terminal.", Color::DarkGrey);
        }
        line("Controls: dymus control <action>", Color::DarkGrey);
        line("status · toggle · next · previous", Color::DarkGrey);
        line("volume <0–100> · stop", Color::DarkGrey);
    }
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
        .min(120)
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

struct Loading {
    active: bool,
    frame: usize,
}
impl Loading {
    fn new() -> Self {
        let active = io::stdout().is_terminal();
        if !active {
            println!("Loading…");
        }
        Self { active, frame: 0 }
    }
    fn draw(&mut self, seconds: u64) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let text = fit(
            &format!(
                "{} Loading · {seconds}s · Ctrl+C to cancel",
                frames[self.frame % frames.len()]
            ),
            width().saturating_sub(1),
        );
        print!("\r\x1b[2K{text}");
        io::stdout()
            .flush()
            .context("Cannot update loading indicator")?;
        self.frame += 1;
        Ok(())
    }
}
impl Drop for Loading {
    fn drop(&mut self) {
        if self.active {
            print!("\r\x1b[2K");
            let _ = io::stdout().flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
