#[derive(Clone, Copy)]
pub struct Command {
    pub name: &'static str,
    pub about: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "new",
        about: "Start a fresh session",
    },
    Command {
        name: "clear",
        about: "Alias for /new",
    },
    Command {
        name: "resume",
        about: "Open the session picker",
    },
    Command {
        name: "home",
        about: "Return to the welcome screen",
    },
    Command {
        name: "plan",
        about: "Enter plan mode",
    },
    Command {
        name: "view-plan",
        about: "Show the current plan file",
    },
    Command {
        name: "always-approve",
        about: "Toggle always-approve mode",
    },
    Command {
        name: "status",
        about: "Show host, model, and turn counts",
    },
    Command {
        name: "model",
        about: "List or set the model",
    },
    Command {
        name: "copy",
        about: "Copy the last assistant reply",
    },
    Command {
        name: "help",
        about: "Keyboard shortcuts",
    },
    Command {
        name: "quit",
        about: "Quit",
    },
    Command {
        name: "exit",
        about: "Alias for /quit",
    },
];

pub fn match_indices(name: &str, q: &str) -> Option<Vec<usize>> {
    if q.is_empty() {
        return Some(Vec::new());
    }
    let lower: Vec<char> = name.chars().map(|c| c.to_ascii_lowercase()).collect();
    let needle: Vec<char> = q.chars().map(|c| c.to_ascii_lowercase()).collect();
    let mut hits = Vec::new();
    let mut k = 0;
    for (i, &c) in lower.iter().enumerate() {
        if k < needle.len() && c == needle[k] {
            hits.push(i);
            k += 1;
        }
    }
    if k == needle.len() {
        Some(hits)
    } else {
        None
    }
}

pub fn matches(query: &str) -> Vec<&'static Command> {
    let q = query.split_whitespace().next().unwrap_or("");
    COMMANDS
        .iter()
        .filter(|c| match_indices(c.name, q).is_some())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_new() {
        let m = matches("ne");
        assert!(m.iter().any(|c| c.name == "new"));
    }

    #[test]
    fn subsequence_highlights_as() {
        assert_eq!(
            match_indices("always-approve", "as").as_deref(),
            Some(&[0, 5][..])
        );
        assert_eq!(match_indices("new", "as"), None);
        assert!(matches("as").iter().any(|c| c.name == "always-approve"));
        assert!(matches("as").iter().all(|c| c.name != "new"));
    }
}
