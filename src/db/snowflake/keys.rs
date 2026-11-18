//! The keys the Snowflake tests sign with: drawn here, never committed.
//!
//! A private key in the tree is what a secret scanner reports in every clone,
//! and these keys guard nothing -- nothing here authenticates to an account.
//! So one is generated into `target/` (ignored) the first time a test needs
//! it, and the two encrypted names are header-only, because `key_pair` refuses
//! an encrypted key by its header before it decodes anything under it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rsa::pkcs8::{EncodePrivateKey, LineEnding};

/// Where the generated files live. Anchored at the manifest rather than the
/// current directory, which a test harness does not promise.
pub(super) fn scratch() -> PathBuf {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-keys");
    std::fs::create_dir_all(&directory).expect("a directory for the generated keys");
    directory
}

/// A committed public half, for the fingerprint this test pins against
/// openssl's own arithmetic. Public key material is nobody's secret.
pub(super) fn public(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("dev/snowflake")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// A 2048-bit PKCS#8 private key, drawn on first use and kept for the next
/// test process.
///
/// `nextest` gives every test its own process, so several can arrive at once:
/// one holds the lock and writes, the rest wait for the file it renames into
/// place. Renaming is atomic, so a reader never sees half a key; a lock left
/// behind by a killed run is stale after five minutes and is taken over.
pub(super) fn signing() -> String {
    let key = scratch().join("snowflake-2048.p8");
    let lock = scratch().join("snowflake-2048.lock");
    // How long a waiter gives the generating process. CI builds dependencies
    // unoptimised, where a 2048-bit key can take half a minute; the file is
    // kept for every later run, so this is paid once per machine and build.
    const BUDGET: Duration = Duration::from_secs(300);
    let deadline = std::time::Instant::now() + BUDGET;
    while std::time::Instant::now() < deadline {
        if std::fs::read_to_string(&key).is_ok_and(|pem| pem.contains("PRIVATE KEY")) {
            return key.display().to_string();
        }
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock)
        {
            Ok(_) => {
                let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
                    .expect("a key is generated");
                let pem = private.to_pkcs8_pem(LineEnding::LF).expect("PKCS#8");
                let temporary = scratch().join(format!("snowflake-2048-{}.p8", std::process::id()));
                std::fs::write(&temporary, pem.as_bytes()).expect("the generated key is written");
                std::fs::rename(&temporary, &key).expect("the key is moved into place");
                let _ = std::fs::remove_file(&lock);
                return key.display().to_string();
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(&lock)
                    .and_then(|lock| lock.modified())
                    .is_ok_and(|written| {
                        written.elapsed().unwrap_or_default() > Duration::from_secs(300)
                    });
                if stale {
                    let _ = std::fs::remove_file(&lock);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => panic!("{}: {error}", lock.display()),
        }
    }
    panic!(
        "no key appeared at {} within {:?}; delete {} and run the tests again",
        key.display(),
        BUDGET,
        lock.display()
    );
}

/// A key file `key_pair` refuses as encrypted. Only the header is read before
/// it refuses, so the body below it is a placeholder rather than a key.
pub(super) fn encrypted(name: &str, header: &str, extra: &str) -> String {
    let path = scratch().join(name);
    if !path.exists() {
        let body = format!("-----BEGIN {header}-----\n{extra}AAAA\n-----END {header}-----\n");
        std::fs::write(&path, body).expect("the encrypted fixture is written");
    }
    path.display().to_string()
}
