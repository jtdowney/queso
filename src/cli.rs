use std::time::Duration;

use console::{Style, Term, measure_text_width, style};
use eyre::Result;
use indicatif::{ProgressBar, ProgressStyle};

const STATUS_WIDTH: usize = 15;
const HEADER_WIDTH: usize = 44;

pub struct Printer {
    term: Term,
    status_style: Style,
}

impl Printer {
    #[must_use]
    pub fn stderr() -> Self {
        Self {
            term: Term::stderr(),
            status_style: Style::new().magenta().bright().bold(),
        }
    }

    pub fn status(&self, verb: &str, message: &str) -> Result<()> {
        self.term.write_line(&format_status(verb, message))?;
        Ok(())
    }

    pub fn detail(&self, label: &str, value: &str) -> Result<()> {
        self.term.write_line(&format_detail(label, value))?;
        Ok(())
    }

    pub fn header(&self, text: &str) -> Result<()> {
        self.term.write_line(&format_header(text))?;
        Ok(())
    }

    #[must_use]
    pub fn spinner(&self, verb: &str, message: &str) -> ProgressBar {
        let styled_verb = self.status_style.apply_to(format!("{verb:>STATUS_WIDTH$}"));
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template(&format!("{styled_verb} {{spinner}} {{msg}}"))
                .expect("valid spinner template"),
        );
        pb.set_message(message.to_string());
        pb.enable_steady_tick(Duration::from_millis(80));
        pb
    }
}

fn format_status(verb: &str, message: &str) -> String {
    let styled_verb = style(format!("{verb:>STATUS_WIDTH$}"))
        .magenta()
        .bright()
        .bold();
    format!("{styled_verb} {message}")
}

fn format_detail(label: &str, value: &str) -> String {
    let styled_label = style(format!("{label:>STATUS_WIDTH$}")).dim();
    format!("{styled_label} {value}")
}

fn format_header(text: &str) -> String {
    let prefix = format!("  ── {text} ");
    let pad_len = HEADER_WIDTH.saturating_sub(measure_text_width(&prefix));
    let line = format!("{prefix}{}", "─".repeat(pad_len));
    let styled_line = style(line).dim();
    format!("\n{styled_line}")
}

#[cfg(test)]
mod test {
    use console::strip_ansi_codes;

    use super::*;

    #[test]
    fn test_status_output() {
        let output = strip_ansi_codes(&format_status("Building", "my_app v1.0.0")).to_string();
        insta::assert_snapshot!(output, @"       Building my_app v1.0.0");
    }

    #[test]
    fn test_detail_output() {
        let output = strip_ansi_codes(&format_detail("OTP", "OTP-28.4.1")).to_string();
        insta::assert_snapshot!(output, @"            OTP OTP-28.4.1");
    }

    #[test]
    fn test_header_output() {
        let output = strip_ansi_codes(&format_header("x86_64-windows")).to_string();
        insta::assert_snapshot!(output, @"

        ── x86_64-windows ────────────────────────
        ");
    }
}
