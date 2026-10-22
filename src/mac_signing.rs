use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const DEFAULT_IDENTIFIER: &str = "io.github.choam-co.discodex";

#[derive(Serialize, Deserialize)]
struct Identity {
    version: u32,
    certificate_sha1: String,
    keychain: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identifier: Option<String>,
}

impl Identity {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1,
            "unsupported local signing identity version"
        );
        ensure!(
            self.keychain.is_absolute(),
            "local signing keychain path must be absolute"
        );
        ensure!(
            self.certificate_sha1.len() == 40
                && self.certificate_sha1.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid local signing certificate fingerprint"
        );
        if let Some(identifier) = &self.identifier {
            validate_identifier(identifier)?;
        }
        Ok(())
    }

    fn identifier(&self) -> Result<&str> {
        self.identifier.as_deref().context(
            "local signing identity has no recorded code-sign identifier; adopt it from the existing owned signed binary before signing",
        )
    }

    fn requirement(&self) -> Result<String> {
        self.validate()?;
        Ok(format!(
            "identifier \"{}\" and certificate leaf = H\"{}\"",
            self.identifier()?,
            self.certificate_sha1.to_ascii_uppercase()
        ))
    }
}

fn signing_language_from_env() -> crate::i18n::Language {
    std::env::var("DISCODEX_LANGUAGE")
        .ok()
        .and_then(|value| value.parse::<crate::i18n::Language>().ok())
        .unwrap_or_default()
}

fn signing_user_text(language: crate::i18n::Language, key: crate::i18n::MessageKey) -> String {
    language.message(key).to_owned()
}

fn signing_operation_error(language: crate::i18n::Language, detail: &str) -> String {
    let detail = crate::discord::sanitize_discord_diagnostic(detail);
    language
        .message(crate::i18n::MessageKey::MacSigningOperationFailed)
        .replace("{detail}", &detail)
}

fn validate_identifier(identifier: &str) -> Result<()> {
    ensure!(
        !identifier.is_empty()
            && identifier
                .bytes()
                .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_') }),
        "invalid local code-sign identifier"
    );
    Ok(())
}

fn parse_identity_requirement(requirement: &str) -> Result<(String, String)> {
    let value = requirement
        .strip_prefix("identifier \"")
        .context("owned signing requirement has an unsupported format")?;
    let (identifier, certificate) = value
        .split_once("\" and certificate leaf = H\"")
        .context("owned signing requirement has an unsupported format")?;
    let fingerprint = certificate
        .strip_suffix('"')
        .context("owned signing requirement has an unsupported format")?;
    validate_identifier(identifier)?;
    ensure!(
        fingerprint.len() == 40 && fingerprint.bytes().all(|b| b.is_ascii_hexdigit()),
        "owned signing requirement has an invalid certificate fingerprint"
    );
    Ok((identifier.to_owned(), fingerprint.to_ascii_uppercase()))
}

fn extract_designated_requirement(stdout: &str, stderr: &str) -> Result<String> {
    stdout
        .lines()
        .chain(stderr.lines())
        .find_map(|line| line.trim().strip_prefix("designated => "))
        .map(str::to_owned)
        .context("existing Mac binary did not report a designated requirement")
}

fn binary_argument(args: &[String]) -> Result<PathBuf> {
    ensure!(
        args.len() == 2 && args[0] == "--binary" && !args[1].is_empty(),
        "mac-sign requires --binary <staged-binary>"
    );
    Ok(PathBuf::from(&args[1]))
}

#[cfg(target_os = "macos")]
pub use platform::{expected_requirement, verify_binary};

/// Records the identifier from a worker binary only after its stored ownership
/// requirement and this installation's certificate both verify the signature.
#[cfg(target_os = "macos")]
pub(crate) fn adopt_owned_binary_identifier(
    binary: &Path,
    owned_requirement: &str,
) -> Result<String> {
    platform::adopt_owned_binary_identifier(binary, owned_requirement)
}

#[cfg(target_os = "macos")]
pub fn sign_binary(binary: &Path) -> Result<String> {
    sign_binary_with_language(binary, signing_language_from_env())
}

#[cfg(target_os = "macos")]
pub fn sign_binary_with_language(binary: &Path, language: crate::i18n::Language) -> Result<String> {
    platform::sign_binary_with_language(binary, language).map_err(|error| {
        anyhow::anyhow!(
            "{}",
            signing_operation_error(language, &format!("{error:#}"))
        )
    })
}

#[cfg(target_os = "macos")]
pub fn sign(args: &[String]) -> Result<()> {
    sign_binary_with_language(&binary_argument(args)?, signing_language_from_env())?;
    println!(
        "{}",
        signing_language_from_env().message(crate::i18n::MessageKey::MacSigningStagedBinarySigned)
    );
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn sign_in_login_session(args: &[String]) -> Result<()> {
    let language = signing_language_from_env();
    platform::sign_in_login_session(args, language).map_err(|error| {
        anyhow::anyhow!(
            "{}",
            signing_operation_error(language, &format!("{error:#}"))
        )
    })
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use anyhow::{Context, bail};
    use std::{
        ffi::CString,
        fs,
        io::{Read, Write},
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    type CfRef = *const std::ffi::c_void;

    #[repr(C)]
    struct KeyImportParameters {
        version: u32,
        flags: u32,
        passphrase: CfRef,
        alert_title: CfRef,
        alert_prompt: CfRef,
        access: CfRef,
        key_usage: u32,
        key_attributes: u32,
    }

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecKeychainOpen(
            path_name: *const std::ffi::c_char,
            keychain: *mut *mut std::ffi::c_void,
        ) -> i32;
        fn SecKeychainUnlock(
            keychain: *mut std::ffi::c_void,
            password_length: u32,
            password: CfRef,
            use_password: u8,
        ) -> i32;
        fn SecTrustedApplicationCreateFromPath(
            path: *const std::ffi::c_char,
            app: *mut *mut std::ffi::c_void,
        ) -> i32;
        fn SecAccessCreate(
            descriptor: CfRef,
            trusted: CfRef,
            access: *mut *mut std::ffi::c_void,
        ) -> i32;
        fn SecKeychainItemImport(
            data: CfRef,
            filename: CfRef,
            format: *mut u32,
            item_type: *mut u32,
            flags: u32,
            params: *const KeyImportParameters,
            keychain: *mut std::ffi::c_void,
            out_items: *mut CfRef,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: CfRef);
        fn CFDataCreate(allocator: CfRef, bytes: *const u8, length: isize) -> CfRef;
        fn CFArrayCreate(
            allocator: CfRef,
            values: *const CfRef,
            count: isize,
            callbacks: *const std::ffi::c_void,
        ) -> CfRef;
        fn CFStringCreateWithBytes(
            allocator: CfRef,
            bytes: *const u8,
            length: isize,
            encoding: u32,
            external: u8,
        ) -> CfRef;
        static kCFTypeArrayCallBacks: std::ffi::c_void;
    }

    fn unlock_keychain_with_system_prompt(
        path: &Path,
        language: crate::i18n::Language,
    ) -> Result<()> {
        use std::os::unix::ffi::OsStrExt;

        let path = CString::new(path.as_os_str().as_bytes())
            .context("login keychain path contains a null byte")?;
        let mut keychain = std::ptr::null_mut();
        let open_status = unsafe { SecKeychainOpen(path.as_ptr(), &mut keychain) };
        let open_error = signing_user_text(
            language,
            crate::i18n::MessageKey::MacSigningKeychainOpenFailed,
        );
        ensure!(open_status == 0, "{open_error} (OSStatus {open_status})");
        let unlock_status = unsafe { SecKeychainUnlock(keychain, 0, std::ptr::null(), 0) };
        unsafe { CFRelease(keychain) };
        let unlock_error = signing_user_text(
            language,
            crate::i18n::MessageKey::MacSigningKeychainUnlockFailed,
        );
        ensure!(
            unlock_status == 0,
            "{unlock_error} (OSStatus {unlock_status})"
        );
        Ok(())
    }

    fn signing_roots(home: &Path) -> (PathBuf, PathBuf) {
        (
            home.join("Library/Application Support/Discodex/signing"),
            home.join("Library/Application Support/Codex Worker/signing"),
        )
    }

    fn has_signing_material(root: &Path) -> bool {
        root.join("identity.json").is_file() || root.join("certificate.pem").is_file()
    }

    // Move the existing metadata directory intact when the new path is available.
    // If migration cannot complete, keep using the old path instead of creating
    // a replacement key; the keychain path and designated requirement stay fixed.
    fn resolve_signing_root(new_root: &Path, legacy_root: &Path) -> PathBuf {
        if new_root.exists() {
            if new_root.join("identity.json").is_file()
                || !legacy_root.join("identity.json").is_file() && has_signing_material(new_root)
            {
                return new_root.to_path_buf();
            }
            if has_signing_material(legacy_root) {
                return legacy_root.to_path_buf();
            }
            return new_root.to_path_buf();
        }
        if !legacy_root.exists() {
            return new_root.to_path_buf();
        }
        let Some(parent) = new_root.parent() else {
            return legacy_root.to_path_buf();
        };
        if fs::create_dir_all(parent).is_err() {
            return legacy_root.to_path_buf();
        }
        match fs::rename(legacy_root, new_root) {
            Ok(()) => new_root.to_path_buf(),
            Err(_) if new_root.exists() && has_signing_material(new_root) => new_root.to_path_buf(),
            Err(_) => legacy_root.to_path_buf(),
        }
    }

    fn signing_root() -> Result<PathBuf> {
        let home =
            PathBuf::from(std::env::var_os("HOME").context("HOME is required for local signing")?);
        let (new_root, legacy_root) = signing_roots(&home);
        Ok(resolve_signing_root(&new_root, &legacy_root))
    }

    #[cfg(test)]
    mod signing_path_tests {
        use super::{resolve_signing_root, signing_roots};
        use std::fs;

        #[test]
        fn selects_the_new_signing_root_after_legacy_migration() {
            let home = tempfile::tempdir().unwrap();
            let (new_root, legacy_root) = signing_roots(home.path());
            fs::create_dir_all(&legacy_root).unwrap();
            fs::write(legacy_root.join("identity.json"), b"existing identity").unwrap();
            fs::write(legacy_root.join("certificate.pem"), b"existing certificate").unwrap();

            assert_eq!(resolve_signing_root(&new_root, &legacy_root), new_root);
        }

        #[test]
        fn selects_legacy_signing_root_when_destination_has_no_signing_material() {
            let home = tempfile::tempdir().unwrap();
            let (new_root, legacy_root) = signing_roots(home.path());
            fs::create_dir_all(&new_root).unwrap();
            fs::create_dir_all(&legacy_root).unwrap();
            fs::write(legacy_root.join("identity.json"), b"existing identity").unwrap();

            assert_eq!(resolve_signing_root(&new_root, &legacy_root), legacy_root);
        }

        #[test]
        fn selects_legacy_signing_root_over_an_incomplete_destination_certificate() {
            let home = tempfile::tempdir().unwrap();
            let (new_root, legacy_root) = signing_roots(home.path());
            fs::create_dir_all(&new_root).unwrap();
            fs::create_dir_all(&legacy_root).unwrap();
            fs::write(new_root.join("certificate.pem"), b"incomplete certificate").unwrap();
            fs::write(legacy_root.join("identity.json"), b"existing identity").unwrap();

            assert_eq!(resolve_signing_root(&new_root, &legacy_root), legacy_root);
        }

        #[test]
        fn prefers_populated_destination_if_both_paths_have_identities() {
            let home = tempfile::tempdir().unwrap();
            let (new_root, legacy_root) = signing_roots(home.path());
            fs::create_dir_all(&new_root).unwrap();
            fs::create_dir_all(&legacy_root).unwrap();
            fs::write(new_root.join("identity.json"), b"new identity").unwrap();
            fs::write(legacy_root.join("identity.json"), b"legacy identity").unwrap();

            assert_eq!(resolve_signing_root(&new_root, &legacy_root), new_root);
        }
    }

    fn load_identity(root: &Path) -> Result<Identity> {
        let identity: Identity =
            serde_json::from_slice(&fs::read(root.join("identity.json")).context(
                "read persistent Mac signing identity; run mac-sign on a staged binary first",
            )?)
            .context("parse persistent Mac signing identity")?;
        identity.validate()?;
        ensure!(
            identity.keychain.is_file(),
            "signing keychain is missing; restore the existing identity instead of generating a replacement"
        );
        Ok(identity)
    }

    fn read_designated_requirement(binary: &Path) -> Result<String> {
        let output = Command::new("/usr/bin/codesign")
            .args(["-d", "-r-"])
            .arg(binary)
            .stdin(Stdio::null())
            .output()
            .context("read designated requirement from owned Mac binary")?;
        ensure!(
            output.status.success(),
            "could not read designated requirement from existing Mac binary: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        extract_designated_requirement(
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        )
    }

    pub fn adopt_owned_binary_identifier(binary: &Path, owned_requirement: &str) -> Result<String> {
        ensure!(
            fs::symlink_metadata(binary)?.file_type().is_file(),
            "owned signing identity adoption requires a regular worker binary"
        );
        let (owned_identifier, owned_fingerprint) = parse_identity_requirement(owned_requirement)?;
        let root = signing_root()?;
        let identity = load_identity(&root)?;
        ensure!(
            owned_fingerprint == identity.certificate_sha1.to_ascii_uppercase(),
            "owned binary requirement does not use this installation's local signing certificate"
        );
        let observed_requirement = read_designated_requirement(binary)?;
        ensure!(
            parse_identity_requirement(&observed_requirement)?
                == (owned_identifier.clone(), owned_fingerprint.clone()),
            "existing worker binary requirement does not match its ownership record"
        );
        verify_binary(binary, owned_requirement)
            .context("verify existing worker against its ownership record and local certificate")?;
        if let Some(identifier) = &identity.identifier {
            ensure!(
                identifier == &owned_identifier,
                "existing worker identifier conflicts with the recorded local signing identity"
            );
            return Ok(owned_requirement.to_owned());
        }

        let _lock = {
            let lock_file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .mode(0o600)
                .open(root.join("setup.lock"))
                .context("open local signing setup lock")?;
            lock_file
                .try_lock()
                .map_err(|_| anyhow::anyhow!("another signing operation is active"))?;
            SetupLock { _file: lock_file }
        };
        let mut updated = identity;
        updated.identifier = Some(owned_identifier);
        updated.validate()?;
        let mut metadata = tempfile::NamedTempFile::new_in(&root)
            .context("stage adopted local signing metadata")?;
        metadata
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        metadata.write_all(&serde_json::to_vec_pretty(&updated)?)?;
        metadata.as_file().sync_all()?;
        metadata
            .persist(root.join("identity.json"))
            .map_err(|error| error.error)
            .context("persist adopted local signing identifier")?;
        Ok(owned_requirement.to_owned())
    }

    pub fn expected_requirement() -> Result<String> {
        load_identity(&signing_root()?)?.requirement()
    }

    // Never include command arguments or command output here: key creation/import
    // must not expose private material through an error report.
    fn private_command(
        command: &mut Command,
        action: &str,
        language: crate::i18n::Language,
    ) -> Result<Vec<u8>> {
        let output = command
            .stdin(Stdio::null())
            .output()
            .with_context(|| action.to_owned())?;
        if !output.status.success() {
            let detail = if command.get_program() == std::ffi::OsStr::new("/usr/bin/security") {
                String::from_utf8_lossy(&output.stderr).trim().to_owned()
            } else {
                "certificate setup command failed".to_owned()
            };
            let guidance = signing_user_text(
                language,
                crate::i18n::MessageKey::MacSigningCommandAuthorizationGuidance,
            );
            bail!("{action}: {detail}. {guidance}");
        }
        Ok(output.stdout)
    }

    struct SetupLock {
        _file: fs::File,
    }

    fn make_pkcs12_bundle(key: &Path, certificate: &Path, passphrase: &str) -> Result<Vec<u8>> {
        let mut child = Command::new("/usr/bin/openssl")
            .args(["pkcs12", "-export", "-inkey"])
            .arg(key)
            .arg("-in")
            .arg(certificate)
            .args(["-passout", "stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("prepare encrypted in-memory key import")?;
        {
            let mut stdin = child
                .stdin
                .take()
                .context("open private import passphrase pipe")?;
            stdin.write_all(passphrase.as_bytes())?;
            stdin.write_all(b"\n")?;
        }
        let output = child
            .wait_with_output()
            .context("finish encrypted key export")?;
        ensure!(
            output.status.success(),
            "could not prepare encrypted in-memory key import"
        );
        Ok(output.stdout)
    }

    fn import_nonextractable_key(
        bundle: &[u8],
        passphrase: &str,
        keychain_path: &Path,
        language: crate::i18n::Language,
    ) -> Result<()> {
        use std::os::unix::ffi::OsStrExt;

        let keychain_name = CString::new(keychain_path.as_os_str().as_bytes())?;
        let mut keychain = std::ptr::null_mut();
        let open_status = unsafe { SecKeychainOpen(keychain_name.as_ptr(), &mut keychain) };
        ensure!(
            open_status == 0,
            "could not open signing keychain for key import (OSStatus {open_status})"
        );

        let data =
            unsafe { CFDataCreate(std::ptr::null(), bundle.as_ptr(), bundle.len() as isize) };
        let pass = unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                passphrase.as_ptr(),
                passphrase.len() as isize,
                0x0800_0100,
                0,
            )
        };
        let descriptor = b"Discodex local code-signing identity";
        let label = unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                descriptor.as_ptr(),
                descriptor.len() as isize,
                0x0800_0100,
                0,
            )
        };
        let trusted_path = b"/usr/bin/codesign\0";
        let mut trusted_app = std::ptr::null_mut();
        let trusted_status = unsafe {
            SecTrustedApplicationCreateFromPath(trusted_path.as_ptr().cast(), &mut trusted_app)
        };
        let trusted_values = [trusted_app as CfRef];
        let trusted_array = if trusted_status == 0 && !trusted_app.is_null() {
            unsafe {
                CFArrayCreate(
                    std::ptr::null(),
                    trusted_values.as_ptr(),
                    1,
                    std::ptr::addr_of!(kCFTypeArrayCallBacks),
                )
            }
        } else {
            std::ptr::null()
        };
        let mut access = std::ptr::null_mut();
        let access_status = if trusted_status == 0 && !label.is_null() && !trusted_array.is_null() {
            unsafe { SecAccessCreate(label, trusted_array, &mut access) }
        } else {
            -50
        };
        let mut format = 12; // kSecFormatPKCS12
        let mut item_type = 5; // kSecItemTypeAggregate
        let parameters = KeyImportParameters {
            version: 0,
            flags: 1, // kSecKeyImportOnlyOne
            passphrase: pass,
            alert_title: std::ptr::null(),
            alert_prompt: std::ptr::null(),
            access,
            key_usage: 0x0000_0004, // CSSM_KEYUSE_SIGN
            // Permanent and sensitive; intentionally omit CSSM_KEYATTR_EXTRACTABLE.
            key_attributes: 0x0000_0001 | 0x0000_0008,
        };
        let import_status = if !data.is_null() && !pass.is_null() && access_status == 0 {
            unsafe {
                SecKeychainItemImport(
                    data,
                    std::ptr::null(),
                    &mut format,
                    &mut item_type,
                    0,
                    &parameters,
                    keychain,
                    std::ptr::null_mut(),
                )
            }
        } else if trusted_status != 0 {
            trusted_status
        } else if access_status != 0 {
            access_status
        } else {
            -108
        };
        for value in [
            access as CfRef,
            trusted_array,
            trusted_app as CfRef,
            label,
            pass,
            data,
            keychain as CfRef,
        ] {
            if !value.is_null() {
                unsafe { CFRelease(value) };
            }
        }
        let import_error = signing_user_text(
            language,
            crate::i18n::MessageKey::MacSigningIdentityImportFailed,
        );
        ensure!(
            import_status == 0,
            "{import_error} (OSStatus {import_status})"
        );
        Ok(())
    }

    fn identity(temporary_parent: &Path, language: crate::i18n::Language) -> Result<Identity> {
        let root = signing_root()?;
        fs::create_dir_all(&root).context("create local signing directory")?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let lock_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open(root.join("setup.lock"))
            .context("open local signing setup lock")?;
        lock_file
            .try_lock()
            .map_err(|_| anyhow::anyhow!("another signing operation is active"))?;
        let _lock = SetupLock { _file: lock_file };
        ensure!(
            temporary_parent.is_dir(),
            "private signing temporary directory is unavailable"
        );
        if root.join("identity.json").exists() {
            return load_identity(&root);
        }
        ensure!(
            !root.join("certificate.pem").exists(),
            "a signing certificate already exists without metadata; restore its identity record rather than rotating the key"
        );
        let keychain_output = private_command(
            Command::new("/usr/bin/security").args(["default-keychain", "-d", "user"]),
            "find user keychain",
            language,
        )?;
        let keychain = PathBuf::from(
            std::str::from_utf8(&keychain_output)?
                .trim()
                .trim_matches('"'),
        );
        ensure!(
            keychain.is_absolute() && keychain.is_file(),
            "user keychain is unavailable"
        );
        let temporary = tempfile::Builder::new()
            .prefix(".setup-")
            .tempdir_in(temporary_parent)?;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
        let key = temporary.path().join("key.pem");
        let cert = temporary.path().join("certificate.pem");
        let config = temporary.path().join("openssl.cnf");
        fs::write(
            &config,
            format!(
                "[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=signing\n[dn]\nCN=Discodex Local Signing {}\n[signing]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=critical,codeSigning\nsubjectKeyIdentifier=hash\n",
                uuid::Uuid::new_v4()
            ),
        )?;
        private_command(
            Command::new("/usr/bin/openssl")
                .args([
                    "req", "-new", "-newkey", "rsa:3072", "-nodes", "-x509", "-days", "3650",
                    "-sha256", "-config",
                ])
                .arg(&config)
                .arg("-keyout")
                .arg(&key)
                .arg("-out")
                .arg(&cert),
            "create local code-signing certificate",
            language,
        )?;
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600))?;
        let fingerprint = private_command(
            Command::new("/usr/bin/openssl")
                .args(["x509", "-in"])
                .arg(&cert)
                .args(["-noout", "-fingerprint", "-sha1"]),
            "read local certificate fingerprint",
            language,
        )?;
        let fingerprint = std::str::from_utf8(&fingerprint)?
            .trim()
            .split_once('=')
            .context("missing certificate fingerprint")?
            .1
            .replace(':', "");
        let identity = Identity {
            version: 1,
            certificate_sha1: fingerprint,
            keychain,
            identifier: Some(DEFAULT_IDENTIFIER.to_owned()),
        };
        identity.requirement()?;
        unlock_keychain_with_system_prompt(&identity.keychain, language)?;
        // PKCS#12 requires a nonempty interoperable passphrase on supported macOS
        // releases. Keep it in memory and feed it only through a pipe/API parameter.
        let mut passphrase = uuid::Uuid::new_v4().simple().to_string();
        let mut bundle = match make_pkcs12_bundle(&key, &cert, &passphrase) {
            Ok(bundle) => bundle,
            Err(error) => {
                unsafe {
                    passphrase.as_bytes_mut().fill(0);
                }
                return Err(error);
            }
        };
        let import_result =
            import_nonextractable_key(&bundle, &passphrase, &identity.keychain, language);
        unsafe {
            passphrase.as_bytes_mut().fill(0);
        }
        bundle.fill(0);
        import_result?;
        // Persist the imported identity before its first use. Failed import has
        // not signed any binary, so a later setup can safely retry from scratch.
        fs::copy(&cert, root.join("certificate.pem"))?;
        let mut metadata = tempfile::NamedTempFile::new_in(&root)?;
        metadata
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        metadata.write_all(&serde_json::to_vec_pretty(&identity)?)?;
        metadata.as_file().sync_all()?;
        metadata
            .persist_noclobber(root.join("identity.json"))
            .map_err(|e| e.error)?;
        // TempDir removes the unencrypted key and PKCS#12 copy on success/error.
        Ok(identity)
    }

    #[derive(Serialize, Deserialize)]
    struct LoginSessionSignResult {
        requirement: Option<String>,
        error: Option<String>,
    }

    fn login_session_arguments(args: &[String]) -> Result<(PathBuf, PathBuf)> {
        let (mut binary, mut result) = (None, None);
        let mut index = 0;
        while index < args.len() {
            let option = args[index].as_str();
            ensure!(
                matches!(option, "--binary" | "--result"),
                "unknown login-session signing option"
            );
            let value = args
                .get(index + 1)
                .context("login-session option requires a path")?;
            ensure!(
                !value.is_empty() && !value.starts_with('-'),
                "login-session option requires a path"
            );
            let slot = if option == "--binary" {
                &mut binary
            } else {
                &mut result
            };
            ensure!(
                slot.replace(PathBuf::from(value)).is_none(),
                "duplicate login-session signing option"
            );
            index += 2;
        }
        Ok((
            binary.context("login-session signing requires --binary")?,
            result.context("login-session signing requires --result")?,
        ))
    }

    fn login_agent_plist(
        label: &str,
        executable: &Path,
        binary: &Path,
        result: &Path,
        home: &Path,
        language: crate::i18n::Language,
    ) -> String {
        fn xml(value: &str) -> String {
            value
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace(char::from(39), "&apos;")
        }
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string><string>mac-sign-in-login-session</string><string>--binary</string><string>{}</string><string>--result</string><string>{}</string></array><key>EnvironmentVariables</key><dict><key>HOME</key><string>{}</string><key>DISCODEX_LANGUAGE</key><string>{}</string></dict><key>ProcessType</key><string>Interactive</string><key>LimitLoadToSessionType</key><string>Aqua</string><key>RunAtLoad</key><true/><key>KeepAlive</key><false/></dict></plist>\n",
            xml(label),
            xml(&executable.display().to_string()),
            xml(&binary.display().to_string()),
            xml(&result.display().to_string()),
            xml(&home.display().to_string()),
            xml(language.code())
        )
    }

    fn run_launchctl(args: &[&str]) -> Result<()> {
        let output = Command::new("/bin/launchctl")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .context("run launchctl for temporary signing helper")?;
        ensure!(
            output.status.success(),
            "launchctl could not manage temporary signing helper"
        );
        Ok(())
    }

    fn launchd_service_loaded(target: &str) -> Result<bool> {
        let output = Command::new("/bin/launchctl")
            .args(["print", target])
            .stdin(Stdio::null())
            .output()
            .context("inspect temporary signing LaunchAgent")?;
        if output.status.success() {
            return Ok(true);
        }
        let detail = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if detail.contains("Could not find service") || detail.contains("No such process") {
            Ok(false)
        } else {
            bail!("could not confirm temporary signing LaunchAgent state");
        }
    }

    fn stop_agent_and_cleanup(target: &str, job: tempfile::TempDir) -> Result<()> {
        let bootout = run_launchctl(&["bootout", target]);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match launchd_service_loaded(target) {
                Ok(false) => {
                    drop(job);
                    return Ok(());
                }
                Ok(true) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
                Ok(true) => break,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
                Err(_) => break,
            }
        }
        let retained = job.keep();
        let bootout_detail = bootout
            .err()
            .map(|error| format!(" ({error:#})"))
            .unwrap_or_default();
        bail!(
            "temporary signing LaunchAgent could not be confirmed stopped{bootout_detail}; private job files were retained at {}",
            retained.display()
        )
    }

    fn current_gui_domain() -> Result<String> {
        let output = Command::new("/usr/bin/id")
            .arg("-u")
            .stdin(Stdio::null())
            .output()
            .context("read current user ID for login-session signing")?;
        ensure!(output.status.success(), "could not read current user ID");
        let uid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        ensure!(
            !uid.is_empty() && uid.bytes().all(|byte| byte.is_ascii_digit()),
            "invalid user ID"
        );
        Ok(format!("gui/{uid}"))
    }

    fn write_private_result(path: &Path, value: &LoginSessionSignResult) -> Result<()> {
        let parent = path
            .parent()
            .context("private signing result has no parent directory")?;
        let mut file =
            tempfile::NamedTempFile::new_in(parent).context("stage private signing result")?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(&serde_json::to_vec(value)?)
            .context("write private signing result")?;
        file.as_file()
            .sync_all()
            .context("flush private signing result")?;
        file.persist_noclobber(path)
            .map_err(|error| error.error)
            .context("publish private signing result")?;
        Ok(())
    }

    fn sign_binary_direct(
        path: &Path,
        temporary_parent: &Path,
        language: crate::i18n::Language,
    ) -> Result<String> {
        ensure!(
            fs::symlink_metadata(path)?.file_type().is_file(),
            "signing target must be a regular staged binary"
        );
        let identity = identity(temporary_parent, language)?;
        unlock_keychain_with_system_prompt(&identity.keychain, language)?;
        let requirement = identity.requirement()?;
        let identifier = identity.identifier()?;
        let output = Command::new("/usr/bin/codesign")
            .args(["--force", "--sign"])
            .arg(&identity.certificate_sha1)
            .arg("--keychain")
            .arg(&identity.keychain)
            .args([
                "--identifier",
                identifier,
                "--timestamp=none",
                "--requirements",
            ])
            .arg(format!("=designated => {requirement}"))
            .arg(path)
            .stdin(Stdio::null())
            .output()
            .context("sign staged Mac binary")?;
        if !output.status.success() {
            let guidance = signing_user_text(
                language,
                crate::i18n::MessageKey::MacSigningPreservedIdentityGuidance,
            );
            bail!(
                "Mac signing failed: {}. {guidance}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        verify_binary(path, &requirement)?;
        Ok(requirement)
    }

    pub fn sign_binary_with_language(
        path: &Path,
        language: crate::i18n::Language,
    ) -> Result<String> {
        ensure!(
            fs::symlink_metadata(path)?.file_type().is_file(),
            "signing target must be a regular staged binary"
        );
        if std::env::var_os("SSH_CONNECTION").is_some() {
            sign_binary_in_login_session(path, language)
        } else {
            let root = signing_root()?;
            sign_binary_direct(path, &root, language)
        }
    }

    fn sign_binary_in_login_session(
        path: &Path,
        language: crate::i18n::Language,
    ) -> Result<String> {
        let binary = path
            .canonicalize()
            .context("resolve staged binary for login-session signing")?;
        let home = PathBuf::from(
            std::env::var_os("HOME").context("HOME is required for login-session signing")?,
        )
        .canonicalize()
        .context("resolve login-session home directory")?;
        let root = signing_root()?;
        fs::create_dir_all(&root).context("create local signing directory")?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let job = tempfile::Builder::new()
            .prefix(".login-sign-")
            .tempdir_in(&root)
            .context("create private login-session signing job")?;
        fs::set_permissions(job.path(), fs::Permissions::from_mode(0o700))?;
        let result_path = job.path().join("result.json");
        let plist_path = job.path().join("agent.plist");
        let executable = std::env::current_exe()
            .context("resolve current worker executable")?
            .canonicalize()
            .context("canonicalize current worker executable")?;
        let identity = load_identity(&root)?;
        let label = format!(
            "{}.signing.{}",
            identity.identifier()?,
            uuid::Uuid::new_v4()
        );
        let plist = login_agent_plist(&label, &executable, &binary, &result_path, &home, language);
        let mut plist_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&plist_path)
            .context("create private temporary signing LaunchAgent")?;
        plist_file.write_all(plist.as_bytes())?;
        plist_file.sync_all()?;
        let domain = current_gui_domain()?;
        let target = format!("{domain}/{label}");
        let bootstrap = run_launchctl(&[
            "bootstrap",
            &domain,
            plist_path
                .to_str()
                .context("LaunchAgent path is not UTF-8")?,
        ]);
        if let Err(error) = bootstrap {
            return match stop_agent_and_cleanup(&target, job) {
                Ok(()) => Err(error).context(
                    "start signing helper in the Aqua login session; unlock the login keychain if macOS requests authorization",
                ),
                Err(cleanup_error) => Err(anyhow::anyhow!(
                    "login-session helper bootstrap failed ({error:#}); {cleanup_error:#}"
                )),
            };
        }

        let deadline = Instant::now() + Duration::from_secs(180);
        let result = loop {
            match fs::File::open(&result_path) {
                Ok(mut file) => {
                    let mut bytes = Vec::new();
                    let read_result = file
                        .read_to_end(&mut bytes)
                        .context("read private signing result");
                    break read_result.and_then(|_| {
                        serde_json::from_slice::<LoginSessionSignResult>(&bytes)
                            .context("parse private signing result")
                    });
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    break Err(anyhow::anyhow!(
                        "login-session signing helper timed out after 180 seconds"
                    ));
                }
                Err(error) => break Err(error).context("read private signing result"),
            }
        };
        let cleanup = stop_agent_and_cleanup(&target, job);
        let result = match (result, cleanup) {
            (Ok(result), Ok(())) => result,
            (Err(error), Ok(())) => return Err(error),
            (Ok(_), Err(error)) => return Err(error),
            (Err(error), Err(cleanup_error)) => {
                return Err(anyhow::anyhow!(
                    "login-session signing failed ({error:#}); {cleanup_error:#}"
                ));
            }
        };
        if let Some(error) = result.error {
            bail!("login-session signing failed: {error}");
        }
        let requirement = result
            .requirement
            .context("login-session helper returned no signing requirement")?;
        let expected = expected_requirement()
            .context("read the signing identity persisted by the login-session helper")?;
        ensure!(
            requirement == expected,
            "login-session helper returned a different signing identity"
        );
        verify_binary(&binary, &expected)
            .context("verify staged binary against the persisted signing identity")?;
        Ok(expected)
    }

    pub fn sign_in_login_session(args: &[String], language: crate::i18n::Language) -> Result<()> {
        let (binary, result_path) = login_session_arguments(args)?;
        let job_dir = result_path
            .parent()
            .context("private login-session result has no parent directory")?
            .canonicalize()
            .context("resolve private login-session job directory")?;
        let root = signing_root()?
            .canonicalize()
            .context("resolve local signing directory")?;
        ensure!(
            job_dir.starts_with(&root)
                && job_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".login-sign-")),
            "login-session helper result must be inside its private signing job directory"
        );
        ensure!(
            fs::metadata(&job_dir)?.permissions().mode() & 0o777 == 0o700,
            "login-session signing job directory must be private"
        );
        let result = match sign_binary_direct(&binary, &job_dir, language) {
            Ok(requirement) => LoginSessionSignResult {
                requirement: Some(requirement),
                error: None,
            },
            Err(error) => LoginSessionSignResult {
                requirement: None,
                error: Some(format!("{error:#}")),
            },
        };
        write_private_result(&result_path, &result)
    }

    #[cfg(test)]
    mod login_session_tests {
        use super::{login_agent_plist, login_session_arguments};
        use std::path::{Path, PathBuf};

        #[test]
        fn parses_private_helper_arguments() {
            let args =
                ["--binary", "/tmp/stage", "--result", "/tmp/job/result.json"].map(str::to_owned);
            assert_eq!(
                login_session_arguments(&args).unwrap(),
                (
                    PathBuf::from("/tmp/stage"),
                    PathBuf::from("/tmp/job/result.json")
                )
            );
            assert!(login_session_arguments(&[]).is_err());
            assert!(login_session_arguments(&["--binary".into(), "x".into()]).is_err());
            assert!(
                login_session_arguments(&[
                    "--binary".into(),
                    "x".into(),
                    "--binary".into(),
                    "y".into(),
                    "--result".into(),
                    "z".into()
                ])
                .is_err()
            );
        }

        #[test]
        fn launch_agent_escapes_paths_and_requests_aqua_interactive_session() {
            let plist = login_agent_plist(
                "io.example.signing",
                Path::new("/tmp/worker<&.app"),
                Path::new("/tmp/build/'candidate'"),
                Path::new("/tmp/job/result&.json"),
                Path::new("/Users/example<&"),
                crate::i18n::Language::English,
            );
            assert!(plist.contains("<string>/tmp/worker&lt;&amp;.app</string>"));
            assert!(plist.contains("<string>/tmp/build/&apos;candidate&apos;</string>"));
            assert!(plist.contains("<key>ProcessType</key><string>Interactive</string>"));
            assert!(plist.contains("<key>LimitLoadToSessionType</key><string>Aqua</string>"));
            assert!(plist.contains("<key>RunAtLoad</key><true/>"));
            assert!(plist.contains("<key>KeepAlive</key><false/>"));
            assert!(plist.contains("<key>HOME</key><string>/Users/example&lt;&amp;</string>"));
            assert!(!plist.contains("/tmp/worker<&.app"));
        }
    }

    pub fn verify_binary(path: &Path, requirement: &str) -> Result<()> {
        let output = Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict", "--test-requirement"])
            .arg(format!("={requirement}"))
            .arg(path)
            .stdin(Stdio::null())
            .output()
            .context("verify Mac binary identity")?;
        ensure!(
            output.status.success(),
            "Mac binary does not satisfy the pinned signing identity: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity() -> Identity {
        Identity {
            version: 1,
            certificate_sha1: "ab".repeat(20),
            keychain: PathBuf::from(if cfg!(windows) {
                "C:\\keys\\login.keychain-db"
            } else {
                "/keys/login.keychain-db"
            }),
            identifier: Some(DEFAULT_IDENTIFIER.to_owned()),
        }
    }
    #[test]
    fn signing_failure_message_is_localized_and_redacts_credentials() {
        assert_eq!(
            signing_operation_error(
                crate::i18n::Language::Korean,
                "codesign rejected api_key=fixture-secret",
            ),
            "Mac 서명 작업에 실패했습니다: codesign rejected api_key=[secret redacted]"
        );
    }

    #[test]
    fn restored_identity_has_the_same_requirement() {
        let first = identity();
        let restored: Identity =
            serde_json::from_slice(&serde_json::to_vec(&first).unwrap()).unwrap();
        assert_eq!(
            first.requirement().unwrap(),
            restored.requirement().unwrap()
        );
        let mut rotated = identity();
        rotated.certificate_sha1 = "cd".repeat(20);
        assert_ne!(first.requirement().unwrap(), rotated.requirement().unwrap());
    }
    #[test]
    fn invalid_signing_records_fail_closed() {
        let mut record = identity();
        record.version = 2;
        assert!(record.requirement().is_err());
        record.version = 1;
        record.certificate_sha1 = "not a fingerprint".into();
        assert!(record.requirement().is_err());
        record.certificate_sha1 = "ab".repeat(20);
        record.keychain = PathBuf::from("relative");
        assert!(record.requirement().is_err());
    }
    #[test]
    fn identity_identifier_round_trips_and_legacy_metadata_fails_closed() {
        let current = identity();
        let saved = serde_json::to_value(&current).unwrap();
        let restored: Identity = serde_json::from_value(saved).unwrap();
        assert_eq!(restored.identifier.as_deref(), Some(DEFAULT_IDENTIFIER));

        let legacy = serde_json::json!({
            "version": 1,
            "certificate_sha1": "ab".repeat(20),
            "keychain": if cfg!(windows) { "C:\\keys\\login.keychain-db" } else { "/keys/login.keychain-db" }
        });
        let legacy: Identity = serde_json::from_value(legacy).unwrap();
        assert!(legacy.validate().is_ok());
        assert!(legacy.requirement().is_err());
    }

    #[test]
    fn parses_only_supported_identity_requirements() {
        let fingerprint = "AB".repeat(20);
        assert_eq!(
            parse_identity_requirement(&format!(
                "identifier \"io.github.example.discodex\" and certificate leaf = H\"{fingerprint}\""
            ))
            .unwrap(),
            ("io.github.example.discodex".to_owned(), fingerprint)
        );
        assert!(
            parse_identity_requirement("identifier \"bad id\" and certificate leaf = H\"aa\"")
                .is_err()
        );
    }

    #[test]
    fn signing_requirement_fingerprints_are_case_insensitive() {
        let lower = format!(
            r#"identifier "io.github.example.discodex" and certificate leaf = H"{}""#,
            "a".repeat(40)
        );
        let upper = format!(
            r#"identifier "io.github.example.discodex" and certificate leaf = H"{}""#,
            "A".repeat(40)
        );
        assert_eq!(
            parse_identity_requirement(&lower).unwrap(),
            parse_identity_requirement(&upper).unwrap()
        );
        let other_identifier =
            upper.replace("io.github.example.discodex", "io.github.example.other");
        assert_ne!(
            parse_identity_requirement(&lower).unwrap(),
            parse_identity_requirement(&other_identifier).unwrap()
        );
    }

    #[test]
    fn extracts_codesign_requirement_from_stdout_or_stderr() {
        let requirement = r#"identifier "io.github.example.discodex" and certificate leaf = H"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA""#;
        assert_eq!(
            extract_designated_requirement(
                &format!("designated => {requirement}\n"),
                "Executable=/tmp/owned-worker\n"
            )
            .unwrap(),
            requirement
        );
        assert_eq!(
            extract_designated_requirement(
                "",
                &format!("Executable=/tmp/owned-worker\ndesignated => {requirement}\n")
            )
            .unwrap(),
            requirement
        );
        assert!(extract_designated_requirement("", "Executable=/tmp/unsigned\n").is_err());
    }

    #[test]
    fn sign_cli_requires_one_staged_binary() {
        assert_eq!(
            binary_argument(&["--binary".into(), "worker".into()]).unwrap(),
            Path::new("worker")
        );
        for args in [
            vec![],
            vec!["worker".into()],
            vec!["--binary".into()],
            vec!["--binary".into(), "".into()],
            vec!["--binary".into(), "worker".into(), "extra".into()],
        ] {
            assert!(binary_argument(&args).is_err());
        }
    }
}
