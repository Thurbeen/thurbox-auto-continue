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

    // The composer is the last `❯` line sitting directly between two rules.
    let composer = (1..lines.len().saturating_sub(1))
        .rev()
        .find(|&i| lines[i].trim_start().starts_with(PROMPT) && is_rule(lines[i - 1]) && is_rule(lines[i + 1]));

    // The menu replaces the composer while it is open, so it counts only when
    // no composer is drawn below it; a closed one can linger in the scrollback.
    let last = |needle: &str| lines.iter().rposition(|l| l.contains(needle));
    if let (Some(menu), Some(_)) = (last("What do you want to do?"), last("Stop and wait for limit to reset"))
        && composer.is_none_or(|c| c < menu)
    {
        return Screen::LimitMenu;
    }

    let Some(at) = composer else {
        return Screen::Unknown;
    };

    // Below the composer: Claude's own countdown, when it is armed. The same
    // words stay in the scrollback above after a cancel, so only here counts.
    // Any one of its words counts, because a narrow pane wraps the line.
    let footer = &lines[at + 2..];
    if footer.iter().any(|l| {
        l.contains("esc to cancel") || l.contains("Continuing automatically") || l.contains("Continuing shortly")
    }) {
        return Screen::ArmedWait;
    }
    // Just above it: a turn in progress.
    let above = &lines[at.saturating_sub(4)..at];
    if above.iter().chain(footer).any(|l| l.contains("esc to interrupt")) {
        return Screen::Unknown;
    }

    let typed =
        lines[at].trim_start().trim_start_matches(PROMPT).trim_matches(|c: char| c.is_whitespace() || c == '\u{a0}');
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

    /// A narrow pane wraps Claude's countdown line in two.
    #[test]
    fn a_wrapped_countdown_is_still_armed() {
        let rule = "─".repeat(30);
        let screen = format!(
            "{rule}\n❯ \n{rule}\n  ⚠ Usage limit reached\n    Continuing automatically at\n    12:54pm · esc to cancel\n"
        );
        assert_eq!(classify(&screen, "continue"), Screen::ArmedWait);
    }

    /// A menu that was closed stays in the scrollback above the composer.
    #[test]
    fn a_closed_menu_in_the_scrollback_is_not_open() {
        let screen = format!("{}\n{}", fixture("limit-menu.txt"), fixture("empty-after-cancel.txt"));
        assert_eq!(classify(&screen, "continue"), Screen::EmptyPrompt);
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
