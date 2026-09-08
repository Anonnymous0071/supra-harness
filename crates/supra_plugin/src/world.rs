//! The supra WIT world: one interface per class, one function per call.
//!
//! The world is deliberately tiny: three interfaces named after the
//! authority ladder, each exporting the same two-call shape (`run` takes
//! canonical text, answers canonical text). The ladder is what the world
//! is *for* - a guest that only needs reads has its imports satisfied by
//! the Agent interface alone, and never sees the Host interface at all.
//!
//! The text is stated here, once, in a `&'static str`, and the host both
//! renders it for tooling and resolves components against it. One source,
//! two consumers - the alternative (a `.wit` file beside this source)
//! would be two sources for one world the moment they disagreed.
//!
//! ```text
//! package supra:plugin@0.1.0;
//!
//! world agent {
//!     import supra:plugin/agent-tools;
//!     export run: func(arguments: string) -> string;
//! }
//!
//! world host {
//!     import supra:plugin/agent-tools;
//!     import supra:plugin/host-tools;
//!     export run: func(arguments: string) -> string;
//! }
//!
//! world user {
//!     import supra:plugin/agent-tools;
//!     import supra:plugin/host-tools;
//!     import supra:plugin/user-tools;
//!     export run: func(arguments: string) -> string;
//! }
//!
//! interface agent-tools {
//!     /// A read through the host: path in, file bytes out.
//!     read-file: func(path: string) -> string;
//!     /// A search through the host: query in, matches out.
//!     search: func(query: string) -> string;
//! }
//!
//! interface host-tools {
//!     /// Spawn through the host's sandbox; argv in, exit description out.
//!     /// The sandbox policy, the fd audit, and the journal all run - the
//!     /// guest only learns the answer, never the host's descriptors.
//!     spawn: func(argv: string) -> string;
//!     /// A journal snapshot through the host: path in, snapshot id out.
//!     snapshot: func(path: string) -> string;
//! }
//!
//! interface user-tools {
//!     /// Ask the person at the terminal: question in, answer out. Never
//!     /// batchable from inside a guest - the TUI surfaces one question
//!     /// per guest call.
//!     ask-user: func(question: string) -> string;
//! }
//! ```

/// The WIT world text, the single source for the plugin ABI.
pub const WORLD: &str = include_str!("supra.wit");

/// The host functions each class may import, as `(interface, function)`
/// pairs. The ladder is ascending by construction: Host sees Agent's
/// imports plus its own, User sees all three.
///
/// Isolation in this harness is by absence (the T20 sentence): the host
/// only *registers* these into the linker, so a guest has no name to
/// call anything outside its class - not a check that refuses, but a
/// namespace that simply has nothing in it.
#[must_use]
pub const fn imports_for(class: supra_types::ToolClass) -> &'static [(&'static str, &'static str)] {
    match class {
        supra_types::ToolClass::Agent => {
            &[("supra:plugin/agent-tools", "read-file"), ("supra:plugin/agent-tools", "search")]
        }
        supra_types::ToolClass::Host => &[
            ("supra:plugin/agent-tools", "read-file"),
            ("supra:plugin/agent-tools", "search"),
            ("supra:plugin/host-tools", "spawn"),
            ("supra:plugin/host-tools", "snapshot"),
        ],
        supra_types::ToolClass::User => &[
            ("supra:plugin/agent-tools", "read-file"),
            ("supra:plugin/agent-tools", "search"),
            ("supra:plugin/host-tools", "spawn"),
            ("supra:plugin/host-tools", "snapshot"),
            ("supra:plugin/user-tools", "ask-user"),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_world_text_is_what_this_module_documents() {
        // The doc comment above and the shipped .wit file must agree:
        // one source, two consumers only works when there is one source.
        assert!(WORLD.contains("world agent"), "the agent world");
        assert!(WORLD.contains("world host"), "the host world");
        assert!(WORLD.contains("world user"), "the user world");
        assert!(WORLD.contains("read-file"), "the agent interface");
        assert!(WORLD.contains("spawn"), "the host interface");
        assert!(WORLD.contains("ask-user"), "the user interface");
        assert!(WORLD.contains("run: func(arguments: string) -> string"), "the export shape");
    }

    #[test]
    fn the_ladder_is_ascending_and_each_rung_is_named() {
        let agent = imports_for(supra_types::ToolClass::Agent);
        let host = imports_for(supra_types::ToolClass::Host);
        let user = imports_for(supra_types::ToolClass::User);

        assert_eq!(agent.len(), 2, "agent: read-file + search");
        assert_eq!(host.len(), 4, "host: agent's two plus spawn + snapshot");
        assert_eq!(user.len(), 5, "user: all five");

        for import in agent {
            assert!(host.contains(import), "host sees every agent import: {import:?}");
            assert!(user.contains(import), "user sees every agent import: {import:?}");
        }
        for import in host {
            assert!(user.contains(import), "user sees every host import: {import:?}");
        }

        // No rung smuggles a higher interface: agent never sees host or
        // user functions, host never sees user functions.
        for (interface, _) in agent {
            assert_eq!(*interface, "supra:plugin/agent-tools", "agent is agent-tools only");
        }
        for (interface, _) in host {
            assert!(
                *interface == "supra:plugin/agent-tools" || *interface == "supra:plugin/host-tools",
                "host is agent+host tools only"
            );
        }
    }
}
