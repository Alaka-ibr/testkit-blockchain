use soroban_sdk::testutils::storage::{Instance as _, Persistent as _, Temporary as _};
use soroban_sdk::testutils::Ledger as _;
use soroban_sdk::{Address, Env, IntoVal, Val};

use crate::core::{TestEnv, TestkitError};

/// Which of Soroban's three storage kinds an entry lives in. TTL semantics
/// differ per kind — see [`TestEnv::ttl_of`] and [`TestEnv::expire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageKind {
    /// Cheapest, expires soonest, and once gone is gone — used for data
    /// that's fine to lose (e.g. rate-limit counters, session state).
    Temporary,
    /// Long-lived, rent-paying storage. Expired entries can be restored by
    /// re-writing them; reading an expired entry panics.
    Persistent,
    /// The contract's own instance data (its "self" storage) plus its
    /// code. There is exactly one instance entry per contract, so
    /// [`TestEnv::ttl_of`] ignores the `key` parameter for this kind.
    Instance,
}

/// The existence/expiry status of a storage entry, as distinguished by
/// [`TestEnv::ttl_status`].
///
/// There is deliberately no third "expired but not yet restored" variant.
/// Soroban protocol 23 introduced automatic entry restoration, and
/// `soroban-sdk`'s test environment emulates it (see that SDK's
/// `_migrating::v23_archived_testing` migration notes): *any* inspection of
/// a [`StorageKind::Persistent`] or [`StorageKind::Instance`] entry —
/// including just asking for its TTL — silently restores it first if it had
/// expired, to a fresh `min_persistent_entry_ttl`. There is no read-only way
/// to observe "expired, not yet restored" for those two kinds; the
/// inspection itself is what restores it. [`StorageKind::Temporary`] has the
/// opposite behavior — an expired temporary entry is deleted outright, so it
/// is indistinguishable from one that was never written. Either way, this
/// crate cannot and does not fabricate a distinct expired state — this type
/// names that fact instead of hiding it (#119).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryTtlStatus {
    /// No entry exists for this key — either it was never written, or (for
    /// [`StorageKind::Temporary`] only) it was written and has since
    /// expired and been deleted.
    Missing,
    /// The entry exists and has at least this many ledgers of TTL
    /// remaining. For [`StorageKind::Persistent`]/[`StorageKind::Instance`],
    /// this may reflect a just-performed automatic restoration rather than
    /// the entry having been continuously live.
    Active(u32),
}

impl TestEnv {
    /// The current TTL of a storage entry, in ledgers.
    ///
    /// Ignores `key` for [`StorageKind::Instance`], which has one TTL per
    /// contract rather than one per key — passing a different key for the
    /// same contract returns the exact same value, since there is only one
    /// instance TTL to report regardless of which key you ask about. If
    /// your test is parameterizing over keys and includes
    /// [`StorageKind::Instance`] in that sweep expecting per-key TTLs,
    /// that's almost certainly not what you want — call this once per
    /// contract for `Instance` instead, with any key (e.g. `()`).
    ///
    /// # Panics
    ///
    /// Panics if the entry does not exist. Does **not** panic merely
    /// because a [`StorageKind::Persistent`] or [`StorageKind::Instance`]
    /// entry has expired — `soroban-sdk`'s test environment emulates
    /// protocol 23's automatic restoration, so accessing an expired entry
    /// of either kind transparently restores it instead of erroring (see
    /// [`EntryTtlStatus`] for the full explanation). Use
    /// [`TestEnv::ttl_status`] for a non-panicking check of whether an
    /// entry exists at all.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use soroban_testkit::ttl::StorageKind;
    /// use soroban_sdk::{contract, contractimpl, symbol_short, Env, Symbol};
    ///
    /// #[contract]
    /// struct Store;
    ///
    /// #[contractimpl]
    /// impl Store {
    ///     pub fn set(env: Env, key: Symbol, value: i128) {
    ///         env.storage().persistent().set(&key, &value);
    ///     }
    /// }
    ///
    /// # fn main() {
    /// let env = TestEnv::new();
    /// let id = env.env().register(Store, ());
    /// StoreClient::new(env.env(), &id).set(&symbol_short!("k"), &1);
    ///
    /// let ttl = env.ttl_of(&id, StorageKind::Persistent, symbol_short!("k"));
    /// assert!(ttl > 0);
    /// # }
    /// ```
    pub fn ttl_of<K: IntoVal<Env, Val>>(
        &self,
        contract: &Address,
        kind: StorageKind,
        key: K,
    ) -> u32 {
        let key_val = key.into_val(self.env());
        self.ttl_of_val(contract, kind, &key_val)
    }

    /// A non-panicking check of whether a storage entry exists, and its TTL
    /// if so. See [`EntryTtlStatus`] for why there is no third "expired"
    /// state — that distinction isn't observable in this SDK version for
    /// any storage kind (#119).
    ///
    /// Like [`TestEnv::ttl_of`], the `key` only selects which entry to check
    /// existence for; for [`StorageKind::Instance`] the reported TTL itself
    /// is always the one instance-wide value regardless of which key you
    /// asked about, even though the existence check is per-key.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use soroban_testkit::ttl::{EntryTtlStatus, StorageKind};
    /// use soroban_sdk::{contract, contractimpl, symbol_short, Env, Symbol};
    ///
    /// #[contract]
    /// struct Store;
    ///
    /// #[contractimpl]
    /// impl Store {
    ///     pub fn set(env: Env, key: Symbol, value: i128) {
    ///         env.storage().persistent().set(&key, &value);
    ///     }
    /// }
    ///
    /// # fn main() {
    /// let env = TestEnv::new();
    /// let id = env.env().register(Store, ());
    ///
    /// assert_eq!(
    ///     env.ttl_status(&id, StorageKind::Persistent, symbol_short!("k")),
    ///     EntryTtlStatus::Missing
    /// );
    ///
    /// StoreClient::new(env.env(), &id).set(&symbol_short!("k"), &1);
    /// assert!(matches!(
    ///     env.ttl_status(&id, StorageKind::Persistent, symbol_short!("k")),
    ///     EntryTtlStatus::Active(_)
    /// ));
    /// # }
    /// ```
    pub fn ttl_status<K: IntoVal<Env, Val>>(
        &self,
        contract: &Address,
        kind: StorageKind,
        key: K,
    ) -> EntryTtlStatus {
        let key_val = key.into_val(self.env());
        self.env().as_contract(contract, || {
            let exists = match kind {
                StorageKind::Temporary => self.env().storage().temporary().has(&key_val),
                StorageKind::Persistent => self.env().storage().persistent().has(&key_val),
                StorageKind::Instance => self.env().storage().instance().has(&key_val),
            };
            if !exists {
                return EntryTtlStatus::Missing;
            }
            let ttl = match kind {
                StorageKind::Temporary => self.env().storage().temporary().get_ttl(&key_val),
                StorageKind::Persistent => self.env().storage().persistent().get_ttl(&key_val),
                StorageKind::Instance => self.env().storage().instance().get_ttl(),
            };
            EntryTtlStatus::Active(ttl)
        })
    }

    /// Advance the ledger far enough that every entry — regardless of
    /// `kind` — expires.
    ///
    /// `kind` is accepted for symmetry with the rest of this module and to
    /// document intent at the call site, but does not change the amount
    /// advanced: from outside a contract, there is no way to know how much
    /// TTL headroom a specific entry has without calling
    /// [`TestEnv::ttl_of`] on it individually, and an entry of any kind
    /// may have been extended up to the network's `max_entry_ttl`. So this
    /// reads `max_entry_ttl` from the environment's current ledger info at
    /// runtime (never hardcoded — see [`crate::ledger::LEDGER_CLOSE_TIME_SECS`] for the
    /// same reasoning applied to ledger close time) and advances one
    /// ledger past it, which guarantees expiry for every kind at once.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if `max_entry_ttl` is already
    /// `u32::MAX` — advancing one ledger past it would overflow ledger
    /// arithmetic. Silently saturating that advance amount instead (as a
    /// naive `saturating_add` would) is actively misleading: it reports
    /// success while advancing by less than what's actually required to
    /// guarantee expiry, so this reports the overflow instead (#120). See
    /// [`TestEnv::try_expire`] for a non-panicking version.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use soroban_testkit::ttl::StorageKind;
    ///
    /// let env = TestEnv::new();
    /// env.expire(StorageKind::Persistent);
    /// ```
    pub fn expire(&self, kind: StorageKind) {
        if let Err(e) = self.try_expire(kind) {
            panic!("{}", e);
        }
    }

    /// Attempt to advance the ledger far enough that every entry —
    /// regardless of `kind` — expires.
    ///
    /// Returns `Err(TestkitError::Misuse)` instead of panicking if
    /// `max_entry_ttl` is already `u32::MAX`, since advancing one ledger
    /// past it would overflow ledger arithmetic (#120). See
    /// [`TestEnv::expire`] for the panicking version and for why this
    /// doesn't just saturate the advance amount instead.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use soroban_testkit::ttl::StorageKind;
    ///
    /// let env = TestEnv::new();
    /// env.try_expire(StorageKind::Persistent).expect("expire should succeed");
    /// ```
    pub fn try_expire(&self, kind: StorageKind) -> Result<(), TestkitError> {
        let _ = kind;
        let max_entry_ttl = self.env().ledger().get().max_entry_ttl;
        let advance_by = max_entry_ttl.checked_add(1).ok_or_else(|| {
            TestkitError::Misuse(format!(
                "cannot guarantee expiry: max_entry_ttl ({max_entry_ttl}) is already u32::MAX, \
                 so advancing one ledger past it overflows ledger arithmetic"
            ))
        })?;
        self.try_advance_ledgers(advance_by)
    }

    /// Assert that running `f` extends the TTL of the entry at `contract`/
    /// `kind`/`key` beyond what it was before `f` ran.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] showing the before
    /// and after TTLs if `f` did not increase it.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use soroban_testkit::ttl::StorageKind;
    /// use soroban_sdk::{contract, contractimpl, symbol_short, Env, Symbol};
    ///
    /// #[contract]
    /// struct Store;
    ///
    /// #[contractimpl]
    /// impl Store {
    ///     pub fn set(env: Env, key: Symbol, value: i128) {
    ///         env.storage().persistent().set(&key, &value);
    ///     }
    ///     pub fn touch(env: Env, key: Symbol) {
    ///         env.storage().persistent().extend_ttl(&key, 5_000, 10_000);
    ///     }
    /// }
    ///
    /// # fn main() {
    /// let env = TestEnv::new();
    /// let id = env.env().register(Store, ());
    /// let client = StoreClient::new(env.env(), &id);
    /// client.set(&symbol_short!("k"), &1);
    ///
    /// env.assert_bumps_ttl(&id, StorageKind::Persistent, symbol_short!("k"), || {
    ///     client.touch(&symbol_short!("k"));
    /// });
    /// # }
    /// ```
    pub fn assert_bumps_ttl<K: IntoVal<Env, Val>>(
        &self,
        contract: &Address,
        kind: StorageKind,
        key: K,
        f: impl FnOnce(),
    ) {
        self.assert_bumps_ttl_by_at_least(contract, kind, key, 1, f);
    }

    /// Assert that running `f` extends the TTL of the entry at `contract`/
    /// `kind`/`key` by at least `min_increase` ledgers, not merely by any
    /// amount greater than zero.
    ///
    /// [`TestEnv::assert_bumps_ttl`] only checks `after > before`, which
    /// passes even for a one-ledger increase — too weak to catch a
    /// `extend_ttl` call whose threshold argument regressed to something far
    /// below what the contract actually needs (#123). Use this when the
    /// exact minimum extension matters to the test, not just its direction.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] showing the before
    /// and after TTLs, and the required minimum increase, if `f` did not
    /// increase the TTL by at least `min_increase`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use soroban_testkit::ttl::StorageKind;
    /// use soroban_sdk::{contract, contractimpl, symbol_short, Env, Symbol};
    ///
    /// #[contract]
    /// struct Store;
    ///
    /// #[contractimpl]
    /// impl Store {
    ///     pub fn set(env: Env, key: Symbol, value: i128) {
    ///         env.storage().persistent().set(&key, &value);
    ///     }
    ///     pub fn touch(env: Env, key: Symbol) {
    ///         env.storage().persistent().extend_ttl(&key, 5_000, 10_000);
    ///     }
    /// }
    ///
    /// # fn main() {
    /// let env = TestEnv::new();
    /// let id = env.env().register(Store, ());
    /// let client = StoreClient::new(env.env(), &id);
    /// client.set(&symbol_short!("k"), &1);
    ///
    /// env.assert_bumps_ttl_by_at_least(&id, StorageKind::Persistent, symbol_short!("k"), 5_000, || {
    ///     client.touch(&symbol_short!("k"));
    /// });
    /// # }
    /// ```
    pub fn assert_bumps_ttl_by_at_least<K: IntoVal<Env, Val>>(
        &self,
        contract: &Address,
        kind: StorageKind,
        key: K,
        min_increase: u32,
        f: impl FnOnce(),
    ) {
        let key_val = key.into_val(self.env());
        let before = self.ttl_of_val(contract, kind, &key_val);
        f();
        let after = self.ttl_of_val(contract, kind, &key_val);
        let actual_increase = after.saturating_sub(before);
        if actual_increase < min_increase {
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "expected the call to extend the {kind:?} TTL from {before} by at least \
                     {min_increase}, but it only went to {after} (an increase of {actual_increase})"
                ))
            );
        }
    }

    /// Assert that `f` completes without panicking after every entry of
    /// `kind` has expired (via [`TestEnv::expire`]) — i.e. that the
    /// contract handles an expired/missing entry gracefully instead of
    /// trapping on it.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] naming the
    /// underlying panic if `f` panics.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use soroban_testkit::ttl::StorageKind;
    ///
    /// let env = TestEnv::new();
    /// env.assert_survives_expiry(StorageKind::Temporary, || {
    ///     // A closure that never touches the expired entry trivially survives.
    /// });
    /// ```
    pub fn assert_survives_expiry(&self, kind: StorageKind, f: impl FnOnce()) {
        self.expire(kind);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        if let Err(payload) = result {
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "expected the contract to survive {kind:?} storage expiry gracefully, \
                     but it panicked: {}",
                    crate::internal::panic_message(payload.as_ref())
                ))
            );
        }
    }

    fn ttl_of_val(&self, contract: &Address, kind: StorageKind, key_val: &Val) -> u32 {
        self.env().as_contract(contract, || match kind {
            StorageKind::Temporary => self.env().storage().temporary().get_ttl(key_val),
            StorageKind::Persistent => self.env().storage().persistent().get_ttl(key_val),
            StorageKind::Instance => self.env().storage().instance().get_ttl(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use vault::{DataKey, Vault, VaultClient};

    #[test]
    fn ttl_of_decreases_as_ledgers_advance() {
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());
        let client = VaultClient::new(env.env(), &id);
        client.set_record(&1);

        let before = env.ttl_of(&id, StorageKind::Persistent, DataKey::Record);
        env.advance_ledgers(10);
        let after = env.ttl_of(&id, StorageKind::Persistent, DataKey::Record);

        assert!(after < before, "expected {after} < {before}");
        assert_eq!(before - after, 10);
    }

    #[test]
    #[should_panic(expected = "expected the call to extend the Persistent TTL")]
    fn assert_bumps_ttl_fails_on_a_contract_that_reads_without_bumping() {
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());
        let client = VaultClient::new(env.env(), &id);
        client.set_record(&1);

        env.assert_bumps_ttl(&id, StorageKind::Persistent, DataKey::Record, || {
            client.touch_record();
        });
    }

    #[test]
    fn assert_bumps_ttl_passes_on_the_fixed_contract() {
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());
        let client = VaultClient::new(env.env(), &id);
        client.set_record(&1);

        env.assert_bumps_ttl(&id, StorageKind::Persistent, DataKey::Record, || {
            client.touch_record_checked();
        });
    }

    #[test]
    #[should_panic(expected = "expected the contract to survive")]
    fn assert_survives_expiry_fails_on_a_contract_that_panics_on_missing_entry() {
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());
        let client = VaultClient::new(env.env(), &id);
        client.set_temp(&42);

        env.assert_survives_expiry(StorageKind::Temporary, || {
            client.read_temp_unchecked();
        });
    }

    #[test]
    fn assert_survives_expiry_passes_on_the_fixed_contract() {
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());
        let client = VaultClient::new(env.env(), &id);
        client.set_temp(&42);

        env.assert_survives_expiry(StorageKind::Temporary, || {
            assert_eq!(client.read_temp_checked(), 0);
        });
    }

    // --- #119: ttl_status distinguishes "never written" from "exists" ---

    #[test]
    fn ttl_status_is_missing_for_a_never_written_persistent_key() {
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());

        assert_eq!(
            env.ttl_status(&id, StorageKind::Persistent, DataKey::Record),
            EntryTtlStatus::Missing
        );
    }

    #[test]
    fn ttl_status_is_missing_for_a_never_written_temporary_key() {
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());

        assert_eq!(
            env.ttl_status(&id, StorageKind::Temporary, DataKey::Temp),
            EntryTtlStatus::Missing
        );
    }

    #[test]
    fn ttl_status_is_missing_for_a_temporary_key_that_expired() {
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());
        let client = VaultClient::new(env.env(), &id);
        client.set_temp(&42);

        env.expire(StorageKind::Temporary);

        assert_eq!(
            env.ttl_status(&id, StorageKind::Temporary, DataKey::Temp),
            EntryTtlStatus::Missing
        );
    }

    #[test]
    fn ttl_status_is_active_for_an_existing_persistent_key() {
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());
        let client = VaultClient::new(env.env(), &id);
        client.set_record(&1);

        assert!(matches!(
            env.ttl_status(&id, StorageKind::Persistent, DataKey::Record),
            EntryTtlStatus::Active(_)
        ));
    }

    // --- #120: expire() reports clock overflow instead of a silent,
    // misleadingly-short advance ---

    #[test]
    fn try_expire_errors_when_max_entry_ttl_is_already_u32_max() {
        let env = TestEnv::new();
        env.env()
            .ledger()
            .with_mut(|li| li.max_entry_ttl = u32::MAX);

        let result = env.try_expire(StorageKind::Persistent);

        assert!(
            matches!(result, Err(TestkitError::Misuse(_))),
            "expected a Misuse error, got {result:?}"
        );
    }

    #[test]
    #[should_panic(expected = "misuse of testkit API")]
    fn expire_panics_with_misuse_when_max_entry_ttl_is_already_u32_max() {
        let env = TestEnv::new();
        env.env()
            .ledger()
            .with_mut(|li| li.max_entry_ttl = u32::MAX);

        env.expire(StorageKind::Persistent);
    }

    #[test]
    fn try_expire_still_succeeds_for_a_normal_max_entry_ttl() {
        let env = TestEnv::new();
        assert!(env.try_expire(StorageKind::Persistent).is_ok());
    }

    // --- #122: Instance TTL inspection ignores the key argument ---

    #[test]
    fn ttl_of_ignores_key_for_instance_kind() {
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());
        let client = VaultClient::new(env.env(), &id);
        client.initialize(&Address::generate(env.env()));

        let ttl_via_owner_key = env.ttl_of(&id, StorageKind::Instance, DataKey::Owner);
        let ttl_via_balance_key = env.ttl_of(&id, StorageKind::Instance, DataKey::Balance);
        let ttl_via_unrelated_key = env.ttl_of(&id, StorageKind::Instance, DataKey::Temp);

        assert_eq!(ttl_via_owner_key, ttl_via_balance_key);
        assert_eq!(ttl_via_owner_key, ttl_via_unrelated_key);
    }

    #[test]
    fn ttl_status_existence_check_for_instance_is_still_per_key() {
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());
        let client = VaultClient::new(env.env(), &id);
        client.initialize(&Address::generate(env.env()));

        // Owner/Balance were set by `initialize`; Record/Temp never are —
        // existence is per-key even though the reported TTL (checked above
        // in `ttl_of_ignores_key_for_instance_kind`) is not.
        assert!(matches!(
            env.ttl_status(&id, StorageKind::Instance, DataKey::Owner),
            EntryTtlStatus::Active(_)
        ));
        assert_eq!(
            env.ttl_status(&id, StorageKind::Instance, DataKey::Record),
            EntryTtlStatus::Missing
        );
    }

    // --- #123: assert_bumps_ttl_by_at_least checks a real threshold, not
    // just "any increase" ---

    #[test]
    #[should_panic(expected = "but it only went to")]
    fn assert_bumps_ttl_by_at_least_fails_below_threshold() {
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());
        let client = VaultClient::new(env.env(), &id);
        client.set_record(&1);

        // touch_record_checked extends to a TTL of 10_000; the entry's TTL
        // right after set_record is far below that, so the real increase is
        // nowhere near large enough to clear an unreasonably high bar.
        env.assert_bumps_ttl_by_at_least(
            &id,
            StorageKind::Persistent,
            DataKey::Record,
            1_000_000,
            || {
                client.touch_record_checked();
            },
        );
    }

    #[test]
    fn assert_bumps_ttl_by_at_least_passes_when_increase_exactly_meets_threshold() {
        // Measure the real increase in one fresh environment first...
        let measuring_env = TestEnv::new();
        let measuring_id = measuring_env.env().register(Vault, ());
        let measuring_client = VaultClient::new(measuring_env.env(), &measuring_id);
        measuring_client.set_record(&1);
        let before = measuring_env.ttl_of(&measuring_id, StorageKind::Persistent, DataKey::Record);
        measuring_client.touch_record_checked();
        let after = measuring_env.ttl_of(&measuring_id, StorageKind::Persistent, DataKey::Record);
        let actual_increase = after - before;

        // ...then assert against that exact value in a second, untouched
        // environment, proving the threshold check accepts an increase that
        // exactly meets (not just exceeds) the minimum. A persistent write
        // never shrinks an already-long TTL back down, so reusing the first
        // environment here would make `before` already-bumped and the
        // measured increase wouldn't repeat.
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());
        let client = VaultClient::new(env.env(), &id);
        client.set_record(&1);

        env.assert_bumps_ttl_by_at_least(
            &id,
            StorageKind::Persistent,
            DataKey::Record,
            actual_increase,
            || {
                client.touch_record_checked();
            },
        );
    }

    #[test]
    fn assert_bumps_ttl_by_at_least_passes_above_threshold() {
        let env = TestEnv::new();
        let id = env.env().register(Vault, ());
        let client = VaultClient::new(env.env(), &id);
        client.set_record(&1);

        env.assert_bumps_ttl_by_at_least(&id, StorageKind::Persistent, DataKey::Record, 1, || {
            client.touch_record_checked();
        });
    }
}
