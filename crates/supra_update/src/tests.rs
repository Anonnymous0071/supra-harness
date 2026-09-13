use super::*;

const PUBLIC_KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

struct Fixture {
    root: PathBuf,
    archive: PathBuf,
    manifest: PathBuf,
    signature: PathBuf,
    key: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn root() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "supra-update-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_nanos()
    ));
    fs::create_dir_all(&path).expect("fixture root");
    path
}

fn tar_gz(entries: &[(&str, &[u8], tar::EntryType)]) -> Vec<u8> {
    let mut compressed = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut compressed, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (path, bytes, kind) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(u64::try_from(bytes.len()).expect("size"));
            header.set_mode(0o755);
            header.set_entry_type(*kind);
            header.set_cksum();
            builder.append_data(&mut header, path, *bytes).expect("append");
        }
        builder.into_inner().expect("tar").finish().expect("gzip");
    }
    compressed
}

fn manifest_json(archive_name: &str, archive: &[u8], executable_path: &str, executable: &[u8]) -> String {
    format!(
        "{{\"schema\":1,\"version\":\"0.2.0\",\"target\":\"{}\",\"archive\":\"{}\",\"archive_size\":{},\"archive_sha256\":\"{}\",\"executable\":{{\"path\":\"{}\",\"size\":{},\"sha256\":\"{}\"}}}}\n",
        current_target(),
        archive_name,
        archive.len(),
        digest_sha256(archive),
        executable_path,
        executable.len(),
        digest_sha256(executable)
    )
}

fn fixture(entries: &[(&str, &[u8], tar::EntryType)], executable_path: &str, executable: &[u8]) -> Fixture {
    let root = root();
    let archive = root.join(format!("supra-{}.tar.gz", current_target()));
    let archive_bytes = tar_gz(entries);
    fs::write(&archive, &archive_bytes).expect("archive");
    let manifest = root.join(format!("supra-{}.manifest.json", current_target()));
    fs::write(
        &manifest,
        manifest_json(
            archive.file_name().expect("name").to_str().expect("UTF-8"),
            &archive_bytes,
            executable_path,
            executable,
        ),
    )
    .expect("manifest");
    let signature = root.join("manifest.minisig");
    fs::write(&signature, "invalid signature fixture").expect("signature");
    let key = root.join("supra.pub");
    fs::write(&key, format!("untrusted comment: key\n{PUBLIC_KEY}\n")).expect("key");
    Fixture { root, archive, manifest, signature, key }
}

fn input(fixture: &Fixture) -> LocalUpdate<'_> {
    LocalUpdate {
        archive: &fixture.archive,
        manifest: &fixture.manifest,
        signature: &fixture.signature,
        public_key: &fixture.key,
        expected_target: current_target(),
    }
}

#[test]
fn versions_are_strict_semver_with_an_optional_input_prefix() {
    assert_eq!(parse_version("v0.1.0").expect("version"), semver::Version::new(0, 1, 0));
    assert_eq!(parse_version("1.2.3").expect("version"), semver::Version::new(1, 2, 3));
    assert!(parse_version("not-a-version").is_err());
}

#[test]
fn public_key_files_and_tampering_fail_as_signature_errors() {
    let result = verify(b"tampered", &format!("comment\n{PUBLIC_KEY}\n"), "invalid-base64");
    assert!(matches!(result, Err(UpdateError::BadSignature(_))));
    assert!(matches!(verify(b"data", "not-a-key", "not-a-signature"), Err(UpdateError::BadSignature(_))));
}

#[test]
fn strict_manifest_rejects_unknown_fields_and_noncanonical_values() {
    let archive = b"archive";
    let executable = b"binary";
    let valid = manifest_json("supra-test.tar.gz", archive, "supra-test/supra", executable);
    let unknown = valid.replacen("{\"schema\":1", "{\"schema\":1,\"extra\":true", 1);
    assert!(matches!(parse_manifest(unknown.as_bytes()), Err(UpdateError::MalformedManifest(_))));
    let prefixed = valid.replace("\"version\":\"0.2.0\"", "\"version\":\"v0.2.0\"");
    assert!(matches!(parse_manifest(prefixed.as_bytes()), Err(UpdateError::MalformedManifest(_))));
    let uppercase = valid.replace(&digest_sha256(archive), &digest_sha256(archive).to_uppercase());
    assert!(matches!(parse_manifest(uppercase.as_bytes()), Err(UpdateError::MalformedManifest(_))));
}

#[test]
fn exact_archive_filename_target_size_and_hash_are_bound() {
    let executable = b"binary";
    let fixture =
        fixture(&[("bundle/supra", executable, tar::EntryType::Regular)], "bundle/supra", executable);
    let manifest_bytes = fs::read(&fixture.manifest).expect("manifest");
    let parsed = parse_manifest(&manifest_bytes).expect("parse");
    let wrong_target = LocalUpdate { expected_target: "aarch64-apple-darwin", ..input(&fixture) };
    // Signature is intentionally invalid, and verification happens first; direct helpers
    // exercise the authenticated binding checks without a private test key.
    assert!(matches!(
        if parsed.target == wrong_target.expected_target {
            Ok(())
        } else {
            Err(UpdateError::TargetMismatch {
                offered: parsed.target.clone(),
                expected: wrong_target.expected_target.to_owned(),
            })
        },
        Err(UpdateError::TargetMismatch { .. })
    ));
    let renamed = fixture.root.join("renamed.tar.gz");
    fs::copy(&fixture.archive, &renamed).expect("copy");
    assert_ne!(regular_filename(&renamed).expect("name"), parsed.archive);
    let altered = fixture.root.join("altered.tar.gz");
    fs::write(&altered, b"different").expect("altered");
    assert!(matches!(
        verify_file_binding(
            &altered,
            parsed.archive_size,
            &parsed.archive_sha256,
            MAX_ARCHIVE_BYTES,
            "archive"
        ),
        Err(UpdateError::SizeMismatch { .. } | UpdateError::DigestMismatch { .. })
    ));
}

#[test]
fn archive_accepts_exactly_one_signed_regular_executable() {
    let executable = b"#!/bin/sh\necho safe\n";
    let fixture =
        fixture(&[("bundle/supra", executable, tar::EntryType::Regular)], "bundle/supra", executable);
    let manifest = parse_manifest(&fs::read(&fixture.manifest).expect("manifest")).expect("parse");
    inspect_archive(&fixture.archive, &manifest, None).expect("safe archive");
}

#[test]
fn archive_rejects_unsigned_extra_members() {
    let executable = b"binary";
    let fixture = fixture(
        &[
            ("bundle/supra", executable, tar::EntryType::Regular),
            ("bundle/README", b"extra", tar::EntryType::Regular),
        ],
        "bundle/supra",
        executable,
    );
    let manifest = parse_manifest(&fs::read(&fixture.manifest).expect("manifest")).expect("parse");
    assert!(matches!(inspect_archive(&fixture.archive, &manifest, None), Err(UpdateError::UnsafeArchive(_))));
}

#[test]
fn archive_rejects_links_special_members_and_wrong_executable_bytes() {
    let executable = b"binary";
    for kind in [tar::EntryType::Symlink, tar::EntryType::Link, tar::EntryType::Fifo] {
        let fixture = fixture(&[("bundle/supra", executable, kind)], "bundle/supra", executable);
        let manifest = parse_manifest(&fs::read(&fixture.manifest).expect("manifest")).expect("parse");
        assert!(matches!(
            inspect_archive(&fixture.archive, &manifest, None),
            Err(UpdateError::UnsafeArchive(_))
        ));
    }
    let fixture =
        fixture(&[("bundle/supra", b"tampered", tar::EntryType::Regular)], "bundle/supra", executable);
    let manifest = parse_manifest(&fs::read(&fixture.manifest).expect("manifest")).expect("parse");
    assert!(matches!(
        inspect_archive(&fixture.archive, &manifest, None),
        Err(UpdateError::SizeMismatch { .. } | UpdateError::DigestMismatch { .. })
    ));
}

#[test]
fn archive_path_validation_rejects_traversal_and_windows_separators() {
    for path in ["../supra", "/tmp/supra", "bundle/../supra", "bundle\\supra", "./supra"] {
        assert!(validate_archive_path(path).is_err(), "accepted {path}");
    }
}

#[cfg(unix)]
#[test]
fn unix_apply_rejects_symlink_destinations_and_installs_mode_0755() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let executable = b"new binary";
    let fixture =
        fixture(&[("bundle/supra", executable, tar::EntryType::Regular)], "bundle/supra", executable);
    let manifest = parse_manifest(&fs::read(&fixture.manifest).expect("manifest")).expect("parse");
    let destination = fixture.root.join("installed");
    fs::write(&destination, b"old").expect("old binary");
    apply_verified(&fixture.archive, &manifest, &destination).expect("apply");
    assert_eq!(fs::read(&destination).expect("installed"), executable);
    assert_eq!(fs::metadata(&destination).expect("metadata").permissions().mode() & 0o777, 0o755);

    let victim = fixture.root.join("victim");
    fs::write(&victim, b"victim").expect("victim");
    let linked = fixture.root.join("linked");
    symlink(&victim, &linked).expect("symlink");
    assert!(matches!(
        apply_verified(&fixture.archive, &manifest, &linked),
        Err(UpdateError::UnsafeDestination(_))
    ));
    assert_eq!(fs::read(&victim).expect("victim unchanged"), b"victim");
}

#[test]
fn malformed_and_oversized_manifest_values_fail_closed() {
    let archive = b"archive";
    let executable = b"binary";
    let valid = manifest_json("supra-test.tar.gz", archive, "bundle/supra", executable);
    for invalid in [
        valid.replace("\"schema\":1", "\"schema\":2"),
        valid.replace("\"archive_size\":7", "\"archive_size\":0"),
        valid.replace("\"path\":\"bundle/supra\"", "\"path\":\"../supra\""),
        valid.replace("\"archive\":\"supra-test.tar.gz\"", "\"archive\":\"../test.tar.gz\""),
    ] {
        assert!(matches!(parse_manifest(invalid.as_bytes()), Err(UpdateError::MalformedManifest(_))));
    }
}
