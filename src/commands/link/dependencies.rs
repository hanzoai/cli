//! Optional browser-terminal prerequisites. Keep detection free of global PATH
//! mutations so tests can model minimal Linux installations safely.

pub(super) fn missing(
    multiplexed: bool,
    mut available: impl FnMut(&str) -> bool,
) -> Vec<&'static str> {
    let mut names = vec!["ttyd"];
    if multiplexed {
        names.extend(["tmux", "sh", "tr", "cut"]);
    }
    names.into_iter().filter(|name| !available(name)).collect()
}

pub(super) fn install_help() -> String {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    help_for(std::env::consts::OS, &release).to_string()
}

fn help_for(os: &str, release: &str) -> &'static str {
    match os {
        "macos" => "Install terminal dependencies with `brew install ttyd tmux`.",
        "linux" => {
            let debian = release.lines().filter_map(|line| {
                let (key, value) = line.split_once('=')?;
                matches!(key, "ID" | "ID_LIKE").then_some(value.trim_matches(['\"', '\'']))
            }).flat_map(str::split_whitespace).any(|id| matches!(id, "debian" | "ubuntu"));
            if debian {
                "Install terminal dependencies with `sudo apt update && sudo apt install ttyd tmux` (the default shell also needs sh, tr and cut). If ttyd is unavailable in your repositories, use a matching Linux binary from https://github.com/tsl0922/ttyd/releases and put it on PATH."
            } else {
                "Install ttyd and tmux using your distribution's package manager (the default shell also needs sh, tr and cut), or get a matching Linux ttyd binary from https://github.com/tsl0922/ttyd/releases and put it on PATH."
            }
        }
        _ => "Install ttyd from https://github.com/tsl0922/ttyd/releases and put it on PATH; the default shell also requires tmux, sh, tr and cut, or select an installed command with `--shell`.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_linux_reports_missing_terminal_tools() {
        assert_eq!(missing(true, |name| name != "ttyd"), ["ttyd"]);
        assert_eq!(missing(true, |name| name != "tmux"), ["tmux"]);
        assert_eq!(
            missing(true, |_| false),
            ["ttyd", "tmux", "sh", "tr", "cut"]
        );
        assert!(missing(true, |_| true).is_empty());
    }

    #[test]
    fn named_shell_does_not_require_multiplexer_tools() {
        assert!(missing(false, |name| name == "ttyd").is_empty());
        assert_eq!(missing(false, |_| false), ["ttyd"]);
    }

    #[test]
    fn install_advice_matches_platform_and_distribution() {
        for release in [
            "ID=ubuntu",
            "ID=debian",
            "ID=pop\nID_LIKE=\"ubuntu debian\"",
            "ID_LIKE='debian'",
        ] {
            let help = help_for("linux", release);
            assert!(help.contains("sudo apt install ttyd tmux"));
            assert!(!help.contains("brew"));
        }
        for release in ["", "ID=fedora", "NAME=ubuntu\nID=unknown"] {
            let help = help_for("linux", release);
            assert!(help.contains("distribution's package manager"));
            assert!(!help.contains("brew") && !help.contains("sudo apt"));
        }
        assert!(help_for("macos", "").contains("brew install ttyd tmux"));
        assert!(!help_for("windows", "").contains("brew"));
    }
}
