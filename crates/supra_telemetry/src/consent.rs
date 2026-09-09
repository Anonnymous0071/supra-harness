use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The consent state. Default is off: telemetry that ships enabled is
/// telemetry that was never asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Consent {
    /// Not enabled. The default.
    #[default]
    Off,
    /// The user said yes.
    On,
}

impl Consent {
    /// Read consent from the marker file, `None` when the file does not
    /// exist - absent is off, the same refusal the session store makes.
    ///
    /// # Errors
    ///
    /// [`std::io::Error`] when the read fails for a reason other than
    /// absence.
    pub fn read(directory: &Path) -> std::io::Result<Option<Self>> {
        let path = path(directory);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        match text.trim() {
            "on" => Ok(Some(Self::On)),
            "off" => Ok(Some(Self::Off)),
            _ => Ok(None),
        }
    }

    /// Write consent. One line, one word, created atomically through a
    /// temp file and a rename.
    ///
    /// # Errors
    ///
    /// [`std::io::Error`] when the write or rename fails.
    pub fn write(self, directory: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(directory)?;
        let path = path(directory);
        let temp: PathBuf = directory.join(".consent.tmp");
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(match self {
            Self::On => b"on\n",
            Self::Off => b"off\n",
        })?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
    }
}

fn path(directory: &Path) -> PathBuf {
    directory.join("telemetry-consent")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "supra-telemetry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn absent_consent_reads_as_none_and_defaults_off() {
        let dir = scratch();
        assert_eq!(Consent::read(&dir).expect("read"), None);
        assert_eq!(Consent::default(), Consent::Off);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn consent_round_trips() {
        let dir = scratch();
        Consent::On.write(&dir).expect("write");
        assert_eq!(Consent::read(&dir).expect("read"), Some(Consent::On));
        Consent::Off.write(&dir).expect("write");
        assert_eq!(Consent::read(&dir).expect("read"), Some(Consent::Off));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_consent_reads_as_none_not_on() {
        let dir = scratch();
        std::fs::write(dir.join("telemetry-consent"), b"yes please").expect("write");
        assert_eq!(Consent::read(&dir).expect("read"), None, "garbage is not consent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_marker_file_is_one_word() {
        let dir = scratch();
        Consent::On.write(&dir).expect("write");
        let text = std::fs::read_to_string(dir.join("telemetry-consent")).expect("read");
        assert_eq!(text, "on\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
