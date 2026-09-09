use std::collections::BTreeMap;

use supra_types::{Mode, ToolClass};

use crate::command::{Command, builtins};

/// A command refusal.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum CommandError {
    /// The name is not registered.
    #[error("no command named /{name}")]
    Unknown {
        /// The name typed.
        name: String,
    },

    /// The command's authority class is above the invoker's.
    #[error("/{name} needs {class:?} authority")]
    Authority {
        /// The command.
        name: String,
        /// The class it demands.
        class: ToolClass,
    },

    /// A duplicate registration.
    #[error("command /{name} is already registered")]
    Duplicate {
        /// The duplicated name.
        name: String,
    },

    /// The name carries characters a palette cannot type.
    #[error("command name {name:?} must be lowercase letters, digits, and hyphens")]
    BadName {
        /// The refused name.
        name: String,
    },
}

/// The registry: the commands the palette lists and the slash parser
/// resolves against. Register-time enforces name shape and uniqueness;
/// dispatch-time enforces the authority class.
#[derive(Debug, Default)]
pub struct Registry {
    commands: BTreeMap<&'static str, Command>,
}

impl Registry {
    /// A registry with the built-ins loaded.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut registry = Self::default();
        for command in builtins() {
            let _ = registry.register(command);
        }
        registry
    }

    /// Register one command.
    ///
    /// # Errors
    ///
    /// [`CommandError::Duplicate`] when the name is taken;
    /// [`CommandError::BadName`] when the name is not lowercase
    /// letters, digits, and hyphens.
    pub fn register(&mut self, command: Command) -> Result<(), CommandError> {
        let name = command.name;
        let valid = !name.is_empty()
            && name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !valid {
            return Err(CommandError::BadName { name: name.to_owned() });
        }
        if self.commands.contains_key(name) {
            return Err(CommandError::Duplicate { name: name.to_owned() });
        }
        self.commands.insert(name, command);
        Ok(())
    }

    /// Resolve one slash input (with or without the leading slash) to a
    /// command, enforcing the authority class against the invoker.
    ///
    /// # Errors
    ///
    /// [`CommandError::Unknown`] when the name is not registered;
    /// [`CommandError::Authority`] when the class is above the
    /// invoker's.
    pub fn resolve(
        &self,
        input: &str,
        invoker: supra_types::Invoker,
        _mode: Mode,
    ) -> Result<Command, CommandError> {
        let name = input.strip_prefix('/').unwrap_or(input);
        let command =
            self.commands.get(name).ok_or_else(|| CommandError::Unknown { name: name.to_owned() })?;
        if !command.class.permits(invoker) {
            return Err(CommandError::Authority { name: command.name.to_owned(), class: command.class });
        }
        Ok(command.clone())
    }

    /// Every registered command, in name order - the palette's listing.
    #[must_use]
    pub fn all(&self) -> Vec<&Command> {
        self.commands.values().collect()
    }

    /// Fuzzy-rank commands against a query. A command matches when the
    /// query's characters appear in order in its name; ties keep name
    /// order, so the palette never reorders under a slow typist.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<&Command> {
        if query.is_empty() {
            return self.all();
        }
        let needle = query.to_lowercase();
        self.commands.values().filter(|command| subsequence(&needle, command.name)).collect()
    }
}

fn subsequence(needle: &str, haystack: &str) -> bool {
    let mut rest = needle.chars();
    let Some(mut wanted) = rest.next() else { return true };
    for character in haystack.chars() {
        if character == wanted {
            match rest.next() {
                Some(next) => wanted = next,
                None => return true,
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_types::Invoker;

    #[test]
    fn builtins_load_once_and_resolve() {
        let registry = Registry::with_builtins();
        assert!(registry.all().len() >= 12);
        let command = registry.resolve("help", Invoker::Host, Mode::Auto).expect("resolve");
        assert_eq!(command.name, "help");
        let command = registry.resolve("/exit", Invoker::Host, Mode::Auto).expect("slash optional");
        assert_eq!(command.name, "exit");
    }

    #[test]
    fn unknown_names_refuse_with_the_name() {
        let registry = Registry::with_builtins();
        let error = registry.resolve("definitely-not-here", Invoker::Host, Mode::Auto).expect_err("unknown");
        assert_eq!(error, CommandError::Unknown { name: "definitely-not-here".to_owned() });
    }

    #[test]
    fn authority_refuses_agent_invoking_host_class() {
        let registry = Registry::with_builtins();
        let error = registry.resolve("sandbox", Invoker::Agent, Mode::Yolo).expect_err("authority");
        assert_eq!(error, CommandError::Authority { name: "sandbox".to_owned(), class: ToolClass::Host });
        assert!(registry.resolve("sandbox", Invoker::Host, Mode::Auto).is_ok(), "the host invokes it freely");
    }

    #[test]
    fn yolo_does_not_lift_authority() {
        let registry = Registry::with_builtins();
        assert!(registry.resolve("sandbox", Invoker::Agent, Mode::Yolo).is_err());
    }

    #[test]
    fn duplicates_refuse() {
        let mut registry = Registry::with_builtins();
        let error =
            registry.register(Command::new("exit", "again", ToolClass::Agent)).expect_err("duplicate");
        assert!(matches!(error, CommandError::Duplicate { .. }));
    }

    #[test]
    fn bad_names_refuse() {
        let mut registry = Registry::default();
        for bad in ["Bad Name", "", "Cost", "with space", "semi;colon", "tab\there"] {
            let error = registry.register(Command::new(bad, "desc", ToolClass::Agent)).expect_err("bad name");
            assert!(matches!(error, CommandError::BadName { .. }), "{bad:?}: {error}");
        }
    }

    #[test]
    fn search_ranks_by_subsequence_in_name_order() {
        let registry = Registry::with_builtins();
        let hits = registry.search("mo");
        assert!(hits.iter().any(|command| command.name == "mode"), "{hits:?}");
        assert!(hits.iter().any(|command| command.name == "model"), "{hits:?}");

        let exact = registry.search("mode");
        assert!(exact.iter().all(|command| subsequence("mode", command.name)));

        assert!(registry.search("zzz").is_empty(), "no match, no entries");
        assert_eq!(registry.search("").len(), registry.all().len(), "empty query lists all");
    }

    #[test]
    fn search_is_case_insensitive() {
        let registry = Registry::with_builtins();
        let hits = registry.search("MO");
        assert!(hits.iter().any(|command| command.name == "mode"), "{hits:?}");
    }
}
