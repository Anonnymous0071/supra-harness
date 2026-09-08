/// The five configured language servers, over the digest's seven
/// languages. Two share: `clangd` covers C and C++, the TypeScript
/// server covers JavaScript as well.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Server {
    /// rust-analyzer, for Rust.
    RustAnalyzer,
    /// typescript-language-server, for TypeScript and JavaScript.
    TypeScript,
    /// pyright, for Python.
    Pyright,
    /// gopls, for Go.
    Gopls,
    /// clangd, for C and C++.
    Clangd,
}

impl Server {
    /// All five, in registry order.
    pub const ALL: [Self; 5] =
        [Self::RustAnalyzer, Self::TypeScript, Self::Pyright, Self::Gopls, Self::Clangd];

    /// The server command's program name.
    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Self::RustAnalyzer => "rust-analyzer",
            Self::TypeScript => "typescript-language-server",
            Self::Pyright => "pyright-langserver",
            Self::Gopls => "gopls",
            Self::Clangd => "clangd",
        }
    }

    /// Arguments the server starts with. LSP servers on stdio take none;
    /// the TypeScript server needs its stdio flag.
    #[must_use]
    pub const fn args(self) -> &'static [&'static str] {
        match self {
            Self::TypeScript => &["--stdio"],
            _ => &[],
        }
    }

    /// The server covering one digest language, or `None` when the
    /// language is outside coverage. Refusing rather than guessing is the
    /// digest's own rule: a server started against the wrong grammar
    /// reports references that do not exist.
    #[must_use]
    pub const fn for_language(language: supra_digest::Language) -> Option<Self> {
        use supra_digest::Language;
        match language {
            Language::Rust => Some(Self::RustAnalyzer),
            Language::TypeScript | Language::JavaScript => Some(Self::TypeScript),
            Language::Python => Some(Self::Pyright),
            Language::Go => Some(Self::Gopls),
            Language::C | Language::Cpp => Some(Self::Clangd),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_digest::Language;

    #[test]
    fn five_servers_cover_all_seven_languages() {
        for language in Language::ALL {
            let server = Server::for_language(language).expect("every language is covered");
            assert!(
                Language::ALL.iter().any(|l| Server::for_language(*l) == Some(server)),
                "{server:?} covers {language:?}"
            );
        }
        // Two servers cover two languages each: 5 servers, 7 languages.
        let covered: std::collections::BTreeSet<_> =
            Language::ALL.iter().map(|l| Server::for_language(*l)).collect();
        assert_eq!(covered.len(), 5);
    }

    #[test]
    fn server_programs_are_the_real_binary_names() {
        assert_eq!(Server::RustAnalyzer.program(), "rust-analyzer");
        assert_eq!(Server::TypeScript.program(), "typescript-language-server");
        assert_eq!(Server::TypeScript.args(), &["--stdio"]);
        assert_eq!(Server::Clangd.args().len(), 0);
        assert_eq!(Server::Pyright.args().len(), 0);
    }
}
