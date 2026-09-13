#![deny(missing_docs)]

/// One parsed interactive line: either a slash command or a task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Line {
    /// A `/name args...` dispatch, with the slash stripped.
    Slash {
        /// Command name as typed.
        name: String,
        /// Remainder of the line after the name.
        args: String,
    },
    /// A provider task for the cohort.
    Task(String),
}

/// Split one interactive line at its first word.
///
/// A leading `/` marks a command; anything else is a task. The name is
/// the first whitespace-delimited word, lowercased for resolution; the
/// args are the trimmed remainder. An empty line is an empty task, which
/// the turn refuses - the parser does not invent errors the turn owns.
#[must_use]
pub(crate) fn parse_line(line: &str) -> Line {
    let trimmed = line.trim();
    let Some(body) = trimmed.strip_prefix('/') else {
        return Line::Task(trimmed.to_owned());
    };
    let body = body.trim_start();
    let (name, args) = match body.find(char::is_whitespace) {
        Some(index) => (&body[..index], body[index..].trim().to_owned()),
        None => (body, String::new()),
    };
    Line::Slash { name: name.to_ascii_lowercase(), args }
}

/// What the loop does with one parsed line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Print the palette: every registered command in name order.
    Help,
    /// End the session, persisting what the loop already saved per turn.
    Exit,
    /// Clear the transcript view; the session file is untouched.
    Clear,
    /// Show the permission mode.
    Mode,
    /// Show the sandbox tier.
    Sandbox,
    /// Resume a saved session by id.
    Resume(String),
    /// Branch the live session into a fresh id.
    Branch,
    /// Export the live transcript as Markdown.
    Export,
    /// Show or switch the provider model.
    Model(Option<String>),
    /// Start a fresh session, abandoning the live id.
    New,
    /// Show telemetry consent state.
    Telemetry,
    /// Show permission mode detail.
    Permissions,
    /// Send the text to the provider cohort.
    Task(String),
}

/// Map one parsed line onto a loop action.
///
/// Unknown names are an error naming the name, not a task: silently
/// sending `/resum sess-1` to the provider would bill a typo. Resolution
/// goes through the registry so authority is checked on the gate's axis;
/// an `Agent`-class command resolves for the terminal user.
pub(crate) fn dispatch(
    registry: &supra_command::Registry,
    line: Line,
) -> Result<Action, supra_command::CommandError> {
    let (name, args) = match line {
        Line::Slash { name, args } => (name, args),
        Line::Task(task) => return Ok(Action::Task(task)),
    };
    let command = registry.resolve(&name, supra_types::Invoker::User, supra_types::Mode::Yolo)?;
    Ok(match command.name {
        "help" => Action::Help,
        "exit" => Action::Exit,
        "clear" => Action::Clear,
        "mode" => Action::Mode,
        "sandbox" => Action::Sandbox,
        "resume" => Action::Resume(args),
        "branch" => Action::Branch,
        "export" => Action::Export,
        "model" => Action::Model(if args.is_empty() { None } else { Some(args) }),
        "new" => Action::New,
        "telemetry" => Action::Telemetry,
        "permissions" => Action::Permissions,
        other => Action::Task(format!("/{other} {args}").trim_end().to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> supra_command::Registry {
        supra_command::Registry::with_builtins()
    }

    #[test]
    fn plain_text_is_a_task_and_slash_is_a_command() {
        assert_eq!(parse_line("fix the build"), Line::Task("fix the build".to_owned()));
        assert_eq!(
            parse_line("/resume 01J00000000000000000000000"),
            Line::Slash { name: "resume".to_owned(), args: "01J00000000000000000000000".to_owned() }
        );
        assert_eq!(parse_line("  /HELP  "), Line::Slash { name: "help".to_owned(), args: String::new() });
        assert_eq!(parse_line(""), Line::Task(String::new()));
        assert_eq!(parse_line("/"), Line::Slash { name: String::new(), args: String::new() });
    }

    #[test]
    fn every_builtin_dispatches_to_its_action() {
        let registry = registry();
        let cases: &[(&str, Action)] = &[
            ("/help", Action::Help),
            ("/exit", Action::Exit),
            ("/clear", Action::Clear),
            ("/mode", Action::Mode),
            ("/sandbox", Action::Sandbox),
            ("/branch", Action::Branch),
            ("/export", Action::Export),
            ("/new", Action::New),
            ("/telemetry", Action::Telemetry),
            ("/permissions", Action::Permissions),
            ("/model", Action::Model(None)),
            ("/model opus", Action::Model(Some("opus".to_owned()))),
            ("/resume 01J0", Action::Resume("01J0".to_owned())),
        ];
        for (input, expected) in cases {
            let line = parse_line(input);
            let action = dispatch(&registry, line).expect("builtin resolves");
            assert_eq!(&action, expected, "{input}");
        }
    }

    #[test]
    fn an_unknown_slash_name_is_a_named_error_not_a_task() {
        let registry = registry();
        let error = dispatch(&registry, parse_line("/resum 01J0")).expect_err("typo must not bill");
        assert!(error.to_string().contains("resum"), "{error}");
    }

    #[test]
    fn tasks_pass_through_dispatch_untouched() {
        let registry = registry();
        let action = dispatch(&registry, parse_line("edit src/main.rs")).expect("task");
        assert_eq!(action, Action::Task("edit src/main.rs".to_owned()));
    }
}
