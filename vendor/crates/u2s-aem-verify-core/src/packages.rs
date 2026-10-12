//! Which FileVault packages an instance's AEM has installed, and who still
//! needs each one. Pure bookkeeping: nothing here talks to AEM. Callers in
//! `crate::flow` and `crate::interactive` install and uninstall through
//! [`crate::aem_client::AemClient`] and record the outcome here.
//!
//! **Keyed by form JCR path.** Two packages carrying the same form path
//! write the same JCR nodes, so installing one while the other is in use
//! would change the form under an open session. Several sessions may share
//! one install, but only of the identical package (same bytes), which
//! [`PackageTable::acquire`] enforces.
//!
//! **Holders, not owners.** Every open session holds its package, and
//! `verify_run` holds its own for the length of its run. A package whose
//! last holder let go stays recorded (holder count zero) until
//! [`PackageTable::take_unreferenced`] hands it to the caller to uninstall.
//! That is also how `keep_on_failure` keeps a failed run's package around:
//! the run releases its hold but skips the uninstall, and the next call
//! cleans it up first.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Identifies a package's bytes within this process, so a second session
/// opening the same package can share the install. Not stable across
/// processes, which is all it needs to be: the table lives and dies with
/// the server.
pub(crate) fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstalledPackage {
    /// The CRX path `AemClient::upload_and_install` returned, which is
    /// what uninstalling needs.
    install_path: String,
    fingerprint: u64,
    holders: usize,
}

/// What [`PackageTable::acquire`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Acquired {
    /// The identical package is already installed and in use; the caller
    /// now holds it too and must not install it again.
    Shared,
    /// Nothing usable is installed for this form path: the caller installs
    /// and then records it with [`PackageTable::installed`].
    MustInstall,
}

/// A different package for the same form path is still held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FormPathInUse {
    pub form_path: String,
    pub holders: usize,
}

#[derive(Debug, Default)]
pub(crate) struct PackageTable {
    by_form_path: BTreeMap<String, InstalledPackage>,
}

impl PackageTable {
    /// Takes a hold on `form_path`'s package with `fingerprint`, or reports
    /// that it must be installed first. An unheld leftover for the same
    /// path counts as nothing installed: callers clear leftovers with
    /// [`Self::take_unreferenced`] before acquiring, and installing again
    /// over one simply replaces it.
    pub(crate) fn acquire(
        &mut self,
        form_path: &str,
        fingerprint: u64,
    ) -> Result<Acquired, FormPathInUse> {
        match self.by_form_path.get_mut(form_path) {
            Some(existing) if existing.holders > 0 && existing.fingerprint == fingerprint => {
                existing.holders += 1;
                Ok(Acquired::Shared)
            }
            Some(existing) if existing.holders > 0 => Err(FormPathInUse {
                form_path: form_path.to_owned(),
                holders: existing.holders,
            }),
            _ => Ok(Acquired::MustInstall),
        }
    }

    /// Records a package the caller just installed after
    /// [`Acquired::MustInstall`], held once by that caller.
    pub(crate) fn installed(&mut self, form_path: &str, fingerprint: u64, install_path: String) {
        self.by_form_path.insert(
            form_path.to_owned(),
            InstalledPackage {
                install_path,
                fingerprint,
                holders: 1,
            },
        );
    }

    /// Lets go of one hold on `form_path`'s package. The package stays
    /// recorded until [`Self::take_unreferenced`] collects it.
    pub(crate) fn release(&mut self, form_path: &str) {
        let package = self
            .by_form_path
            .get_mut(form_path)
            .unwrap_or_else(|| panic!("released {form_path:?}, which was never installed"));
        assert!(
            package.holders > 0,
            "released {form_path:?} more often than it was held"
        );
        package.holders -= 1;
    }

    /// Forgets every package nobody holds and returns their install paths,
    /// for the caller to uninstall.
    pub(crate) fn take_unreferenced(&mut self) -> Vec<String> {
        let unreferenced: Vec<String> = self
            .by_form_path
            .iter()
            .filter(|(_, package)| package.holders == 0)
            .map(|(form_path, _)| form_path.clone())
            .collect();
        unreferenced
            .into_iter()
            .filter_map(|form_path| self.by_form_path.remove(&form_path))
            .map(|package| package.install_path)
            .collect()
    }

    /// Whether anything at all is recorded, held or not.
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.by_form_path.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORM: &str = "/content/forms/af/a/form";

    #[test]
    fn a_first_open_must_install_and_a_second_identical_open_shares_it() {
        let mut table = PackageTable::default();
        assert_eq!(table.acquire(FORM, 7), Ok(Acquired::MustInstall));
        table.installed(FORM, 7, "/etc/packages/a.zip".to_owned());
        assert_eq!(table.acquire(FORM, 7), Ok(Acquired::Shared));
    }

    #[test]
    fn a_different_package_for_a_held_form_path_is_refused() {
        let mut table = PackageTable::default();
        table.acquire(FORM, 7).unwrap();
        table.installed(FORM, 7, "/etc/packages/a.zip".to_owned());
        assert_eq!(
            table.acquire(FORM, 8),
            Err(FormPathInUse {
                form_path: FORM.to_owned(),
                holders: 1
            })
        );
    }

    #[test]
    fn a_package_is_uninstalled_only_once_its_last_holder_lets_go() {
        let mut table = PackageTable::default();
        table.acquire(FORM, 7).unwrap();
        table.installed(FORM, 7, "/etc/packages/a.zip".to_owned());
        table.acquire(FORM, 7).unwrap();

        table.release(FORM);
        assert!(table.take_unreferenced().is_empty(), "one holder remains");
        table.release(FORM);
        assert_eq!(table.take_unreferenced(), ["/etc/packages/a.zip"]);
        assert!(table.is_empty());
    }

    /// `keep_on_failure`: the run lets go but skips the uninstall, and the
    /// leftover neither blocks nor gets shared by the next open.
    #[test]
    fn an_unheld_leftover_neither_blocks_nor_is_shared() {
        let mut table = PackageTable::default();
        table.acquire(FORM, 7).unwrap();
        table.installed(FORM, 7, "/etc/packages/a.zip".to_owned());
        table.release(FORM);

        assert_eq!(table.acquire(FORM, 7), Ok(Acquired::MustInstall));
        assert_eq!(table.acquire(FORM, 8), Ok(Acquired::MustInstall));
    }

    #[test]
    #[should_panic(expected = "more often than it was held")]
    fn releasing_more_than_held_is_a_bug() {
        let mut table = PackageTable::default();
        table.installed(FORM, 7, "/etc/packages/a.zip".to_owned());
        table.release(FORM);
        table.release(FORM);
    }

    #[test]
    fn the_same_bytes_fingerprint_the_same_and_different_bytes_differently() {
        assert_eq!(fingerprint(b"package"), fingerprint(b"package"));
        assert_ne!(fingerprint(b"package"), fingerprint(b"packagf"));
    }
}
