/// The three configured debug adapters. DAP coverage follows the
/// languages with native debuggers worth a harness: compiled ones plus
/// Python.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Adapter {
    /// `CodeLLDB`, for Rust, C, and C++ - the LLVM debugger behind
    /// one adapter.
    CodeLldb,
    /// debugpy, for Python.
    DebugPy,
    /// Delve, for Go.
    Delve,
}

impl Adapter {
    /// All three, in registry order.
    pub const ALL: [Self; 3] = [Self::CodeLldb, Self::DebugPy, Self::Delve];

    /// The adapter command's program name.
    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Self::CodeLldb => "codelldb",
            Self::DebugPy => "debugpy-adapter",
            Self::Delve => "dlv",
        }
    }

    /// Arguments the adapter starts with. DAP adapters speak stdio.
    #[must_use]
    pub const fn args(self) -> &'static [&'static str] {
        match self {
            Self::CodeLldb | Self::Delve | Self::DebugPy => &[],
        }
    }

    /// The adapter covering one digest language, or `None` for
    /// TypeScript and JavaScript - interpreted, with no debugger the
    /// harness wires. Refusing rather than guessing is the T24 rule.
    #[must_use]
    pub const fn for_language(language: supra_digest::Language) -> Option<Self> {
        use supra_digest::Language;
        match language {
            Language::Rust | Language::C | Language::Cpp => Some(Self::CodeLldb),
            Language::Python => Some(Self::DebugPy),
            Language::Go => Some(Self::Delve),
            Language::TypeScript | Language::JavaScript => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_digest::Language;

    #[test]
    fn three_adapters_cover_five_of_seven_languages() {
        let covered =
            Language::ALL.iter().filter(|language| Adapter::for_language(**language).is_some()).count();
        assert_eq!(covered, 5, "Rust, C, C++, Python, Go");
    }

    #[test]
    fn typescript_and_javascript_refuse_rather_than_guess() {
        assert!(Adapter::for_language(Language::TypeScript).is_none());
        assert!(Adapter::for_language(Language::JavaScript).is_none());
    }

    #[test]
    fn adapter_programs_are_the_real_binary_names() {
        assert_eq!(Adapter::CodeLldb.program(), "codelldb");
        assert_eq!(Adapter::DebugPy.program(), "debugpy-adapter");
        assert_eq!(Adapter::Delve.program(), "dlv");
        assert_eq!(Adapter::Delve.args().len(), 0);
        assert_eq!(Adapter::CodeLldb.args().len(), 0);
        assert_eq!(Adapter::DebugPy.args().len(), 0);
    }
}
