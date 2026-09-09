use supra_types::ToolClass;

/// What a command does when picked. Commands are descriptions plus a
/// selector; the runtime owns the action, so the registry never holds a
/// closure over harness state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// The name typed after the slash.
    pub name: &'static str,
    /// One line, shown in the palette.
    pub description: &'static str,
    /// The authority the command needs to run - the same axis the
    /// permission gate uses, so a command is subject to the gate, not
    /// exempt from it.
    pub class: ToolClass,
}

impl Command {
    /// Build a command.
    #[must_use]
    pub const fn new(name: &'static str, description: &'static str, class: ToolClass) -> Self {
        Self { name, description, class }
    }
}

/// The built-in command set. Names with a slash command in the
/// architecture document, and no more: `cost`, `cache`, and `context`
/// have no commands by design (section 7 - transparency that must be
/// requested is never consulted when it matters), and there is no
/// `/think` (the thinking budget is frozen per session).
#[must_use]
pub fn builtins() -> Vec<Command> {
    vec![
        Command::new("help", "List commands and keys.", ToolClass::Agent),
        Command::new("exit", "End the session.", ToolClass::Agent),
        Command::new("clear", "Clear the transcript view.", ToolClass::Agent),
        Command::new("model", "Show or switch the provider model.", ToolClass::Agent),
        Command::new("new", "Start a fresh session.", ToolClass::Agent),
        Command::new("resume", "Resume a saved session.", ToolClass::Agent),
        Command::new("branch", "Branch the session from here.", ToolClass::Agent),
        Command::new("export", "Export the transcript as Markdown.", ToolClass::Agent),
        Command::new("permissions", "Show the permission mode.", ToolClass::Agent),
        Command::new("telemetry", "Show telemetry consent state.", ToolClass::Agent),
        Command::new("sandbox", "Show the sandbox tier.", ToolClass::Host),
        Command::new("mode", "Show the permission mode.", ToolClass::Agent),
    ]
}

/// The commands the architecture forbids, asserted at test time: a
/// regression that adds one of these is a design regression, not a
/// feature.
pub const FORBIDDEN: [&str; 4] = ["cost", "cache", "context", "think"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_names_are_unique() {
        let commands = builtins();
        let mut names: Vec<&str> = commands.iter().map(|command| command.name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "every name is unique");
    }

    #[test]
    fn none_of_the_forbidden_names_appear() {
        let commands = builtins();
        for forbidden in FORBIDDEN {
            assert!(
                commands.iter().all(|command| command.name != forbidden),
                "/{forbidden} must not exist: section 7 forbids cost/cache/context, the thinking budget is frozen"
            );
        }
    }

    #[test]
    fn sandbox_demands_host_class() {
        let commands = builtins();
        let sandbox = commands.iter().find(|command| command.name == "sandbox").expect("present");
        assert_eq!(sandbox.class, ToolClass::Host);
    }
}
