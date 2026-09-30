//! What a Claude Code pane is showing, read from `session capture` text.
//!
//! The patterns come from screens recorded from Claude Code 2.1.285 at a usage
//! limit (`tests/fixtures/screens/`). They are English-only. Anything that does
//! not match one of them is `Unknown`, and nothing is ever typed into an
//! `Unknown` screen.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The composer is empty and nothing is running or counting down.
    EmptyPrompt,
    /// The composer holds exactly our message.
    OurText,
    /// The composer holds something else: the user is typing.
    OtherText,
    /// The `/rate-limit-options` menu is open.
    LimitMenu,
    /// Claude's own resume is counting down (`Continuing automatically at …`).
    ArmedWait,
    /// Anything else: a dialog, a running turn, another program.
    Unknown,
}

impl Screen {
    pub fn name(self) -> &'static str {
        match self {
            Screen::EmptyPrompt => "empty-prompt",
            Screen::OurText => "our-text",
            Screen::OtherText => "other-text",
            Screen::LimitMenu => "limit-menu",
            Screen::ArmedWait => "armed-wait",
            Screen::Unknown => "unknown",
        }
    }
}

const PROMPT: char = '❯';

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    t.chars().count() >= 10 && t.chars().all(|c| c == '─')
}

pub fn classify(text: &str, message: &str) -> Screen {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();

    let has = |needle: &str| lines.iter().any(|l| l.contains(needle));
    if has("What do you want to do?") && has("Stop and wait for limit to reset") {
        return Screen::LimitMenu;
    }

    // The composer is the last `❯` line sitting directly between two rules.
    let Some(at) = (1..lines.len().saturating_sub(1)).rev().find(|&i| {
        lines[i].trim_start().starts_with(PROMPT) && is_rule(lines[i - 1]) && is_rule(lines[i + 1])
    }) else {
        return Screen::Unknown;
    };

    // Below the composer: Claude's own countdown, when it is armed. The same
    // words stay in the scrollback above after a cancel, so only here counts.
    let footer = &lines[at + 2..];
    if footer.iter().any(|l| l.contains("esc to cancel") && l.contains("Continuing")) {
        return Screen::ArmedWait;
    }
    // Just above it: a turn in progress.
    let above = &lines[at.saturating_sub(4)..at];
    if above.iter().chain(footer).any(|l| l.contains("esc to interrupt")) {
        return Screen::Unknown;
    }

    let typed = lines[at]
        .trim_start()
        .trim_start_matches(PROMPT)
        .trim_matches(|c: char| c.is_whitespace() || c == '\u{a0}');
    if typed.is_empty() {
        Screen::EmptyPrompt
    } else if typed == message.trim() {
        Screen::OurText
    } else {
        Screen::OtherText
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/screens/claude-2.1.285");
        std::fs::read_to_string(dir.join(name)).unwrap()
    }

    #[test]
    fn recorded_screens_classify() {
        let cases = [
            ("empty-prompt.txt", Screen::EmptyPrompt),
            ("empty-after-cancel.txt", Screen::EmptyPrompt),
            ("our-text.txt", Screen::OurText),
            ("limit-menu.txt", Screen::LimitMenu),
            ("armed-wait.txt", Screen::ArmedWait),
            ("armed-wait-shortly.txt", Screen::ArmedWait),
        ];
        for (name, want) in cases {
            assert_eq!(classify(&fixture(name), "continue"), want, "{name}");
        }
    }

    #[test]
    fn someone_elses_text_is_not_ours() {
        assert_eq!(classify(&fixture("our-text.txt"), "carry on"), Screen::OtherText);
    }

    #[test]
    fn a_shell_or_a_blank_pane_is_unknown() {
        assert_eq!(classify("user@host:~$ \n", "continue"), Screen::Unknown);
        assert_eq!(classify("", "continue"), Screen::Unknown);
    }

    #[test]
    fn a_running_turn_is_unknown() {
        let rule = "─".repeat(40);
        let screen = format!("✻ Cooking… (esc to interrupt)\n{rule}\n❯ \n{rule}\n  ⏵⏵ auto mode on\n");
        assert_eq!(classify(&screen, "continue"), Screen::Unknown);
    }
}
