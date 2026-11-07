//! Where a sign-in is kept: the macOS keychain, never a file.

use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

/// `errSecItemNotFound`: asking for something that isn't there is not a failure.
const ITEM_NOT_FOUND: i32 = -25300;

/// One saved secret. The keychain implementation is the real one; the in-memory one is
/// for tests, which must never touch the user's keychain.
pub trait SecretStore: Send + Sync {
    fn get(&self) -> Result<Option<String>, String>;
    fn set(&self, value: &str) -> Result<(), String>;
    fn delete(&self) -> Result<(), String>;
}

/// A generic-password item in the user's login keychain.
pub struct Keychain {
    service: String,
    account: String,
}

impl Keychain {
    pub fn new(service: &str, account: &str) -> Self {
        Self {
            service: service.to_owned(),
            account: account.to_owned(),
        }
    }

    /// The item holding the GitHub sign-in.
    pub fn github() -> Self {
        Self::new("Ramus", "github.com")
    }
}

impl SecretStore for Keychain {
    fn get(&self) -> Result<Option<String>, String> {
        match get_generic_password(&self.service, &self.account) {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
            Err(error) => Err(format!("the keychain refused: {error}")),
        }
    }

    fn set(&self, value: &str) -> Result<(), String> {
        set_generic_password(&self.service, &self.account, value.as_bytes())
            .map_err(|error| format!("couldn't save to the keychain: {error}"))
    }

    fn delete(&self) -> Result<(), String> {
        match delete_generic_password(&self.service, &self.account) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
            Err(error) => Err(format!("couldn't remove it from the keychain: {error}")),
        }
    }
}

/// Keeps the secret in memory only.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryStore(std::sync::Mutex<Option<String>>);

#[cfg(test)]
impl SecretStore for MemoryStore {
    fn get(&self) -> Result<Option<String>, String> {
        Ok(self.0.lock().expect("memory store").clone())
    }

    fn set(&self, value: &str) -> Result<(), String> {
        *self.0.lock().expect("memory store") = Some(value.to_owned());
        Ok(())
    }

    fn delete(&self) -> Result<(), String> {
        *self.0.lock().expect("memory store") = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips_and_forgets() {
        let store = MemoryStore::default();
        assert_eq!(store.get().unwrap(), None);
        store.set("one").unwrap();
        store.set("two").unwrap();
        assert_eq!(store.get().unwrap().as_deref(), Some("two"));
        store.delete().unwrap();
        store.delete().unwrap();
        assert_eq!(store.get().unwrap(), None);
    }

    /// The real keychain, under a name no real sign-in uses, removed afterwards.
    #[test]
    fn the_keychain_round_trips_a_secret() {
        // A CI runner has no unlocked login keychain to write to.
        if std::env::var_os("CI").is_some() {
            eprintln!("skipped: no login keychain on CI");
            return;
        }
        let store = Keychain::new(
            "Ramus-test-delete-me",
            &format!("round-trip-{}", std::process::id()),
        );
        store.delete().unwrap();
        assert_eq!(store.get().unwrap(), None);
        store.set("s3cret").unwrap();
        assert_eq!(store.get().unwrap().as_deref(), Some("s3cret"));
        store.set("changed").unwrap();
        assert_eq!(store.get().unwrap().as_deref(), Some("changed"));
        store.delete().unwrap();
        assert_eq!(store.get().unwrap(), None);
    }
}
