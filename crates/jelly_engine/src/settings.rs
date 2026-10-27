//! The application settings, kept in `settings.json` in the data directory so the user
//! can read and edit them by hand.
//!
//! The file is read once at startup and rewritten whole on every change, through a
//! synced temporary file and a rename, so a crash mid-write leaves the previous file
//! intact. A missing file means the defaults and is written so the user can find it. A
//! file that does not parse is a startup failure rather than a silent fallback, because
//! a malformed value must not silently change a setting. A missing key reads as its
//! default; unknown keys are ignored and omitted from subsequent writes.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{self, ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use jelly_core::{
    AssetId, ChainId, CoreError, FiatCurrency, ProxySetting, SecretText, TokenId, wire_enum,
};
use serde::{Deserialize, Serialize, Serializer};
use tokio::sync::RwLock;
use zeroize::Zeroizing;

use crate::WalletEngine;
use crate::error::{StartupError, blocking};

const SETTINGS_FILE: &str = "settings.json";

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// What a typed send amount counts in.
    pub enum AmountEntry {
        /// The fiat currency, converted to the asset at its current price.
        Fiat => "fiat",
        /// The asset's own units.
        Units => "units",
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// What the window shows behind the page, where the operating system can draw it.
    pub enum BackdropSetting {
        /// A blur of whatever is behind the window, under the canvas.
        Blur => "blur",
        /// The opaque canvas.
        Solid => "solid",
    }
}

/// What `settings.json` holds. `Debug` reports only whether each API key is present, and
/// an empty stored key counts as present, so a log never echoes one.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    pub auto_lock_minutes: u32,
    #[serde(serialize_with = "serialize_api_key")]
    pub etherscan_api_key: Option<SecretText>,
    #[serde(serialize_with = "serialize_api_key")]
    pub trongrid_api_key: Option<SecretText>,
    pub is_price_feed_enabled: bool,
    pub is_price_chart_enabled: bool,
    pub proxy: Option<ProxySetting>,
    pub backdrop: BackdropSetting,
    /// The chain whose asset was shown last.
    pub shown_chain: Option<ChainId>,
    /// The tokens shown in place of their chains' native coins, at most one per chain.
    pub shown_tokens: BTreeSet<TokenId>,
    pub fiat_currency: FiatCurrency,
    pub amount_entry: AmountEntry,
}

fn serialize_api_key<'a, S: Serializer>(
    key: impl IntoIterator<Item = &'a SecretText>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match key.into_iter().next() {
        Some(key) => key.with_str(|key| serializer.serialize_some(key)),
        None => serializer.serialize_none(),
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            auto_lock_minutes: 15,
            etherscan_api_key: None,
            trongrid_api_key: None,
            is_price_feed_enabled: true,
            is_price_chart_enabled: true,
            proxy: None,
            backdrop: BackdropSetting::Blur,
            shown_chain: None,
            shown_tokens: BTreeSet::new(),
            fiat_currency: FiatCurrency::Usd,
            amount_entry: AmountEntry::Fiat,
        }
    }
}

impl fmt::Debug for AppSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppSettings")
            .field("auto_lock_minutes", &self.auto_lock_minutes)
            .field("has_etherscan_api_key", &self.etherscan_api_key.is_some())
            .field("has_trongrid_api_key", &self.trongrid_api_key.is_some())
            .field("is_price_feed_enabled", &self.is_price_feed_enabled)
            .field("is_price_chart_enabled", &self.is_price_chart_enabled)
            .field("proxy", &self.proxy)
            .field("backdrop", &self.backdrop)
            .field("shown_chain", &self.shown_chain)
            .field("shown_tokens", &self.shown_tokens)
            .field("fiat_currency", &self.fiat_currency)
            .field("amount_entry", &self.amount_entry)
            .finish()
    }
}

impl AppSettings {
    /// Shows `asset` in place of whatever its chain showed, and makes its chain the one
    /// shown. A native asset puts its chain back to the coin.
    fn show_asset(&mut self, asset: AssetId) {
        self.shown_chain = Some(asset.chain());
        self.shown_tokens.retain(|token| token.chain() != asset.chain());
        if let AssetId::Token(token) = asset {
            self.shown_tokens.insert(token);
        }
    }

    /// Turning the feed on enters amounts in fiat and turning it off enters them in units,
    /// whatever was chosen before. Choosing an entry never touches the feed.
    const fn set_price_feed_enabled(&mut self, is_enabled: bool) {
        self.is_price_feed_enabled = is_enabled;
        self.amount_entry = if is_enabled { AmountEntry::Fiat } else { AmountEntry::Units };
    }
}

/// The settings file and the copy of it the engine answers from.
#[derive(Debug)]
pub(crate) struct Settings {
    path: PathBuf,
    current: RwLock<AppSettings>,
}

impl Settings {
    /// Reads the file, or writes the defaults when there is none.
    pub(crate) fn load(data_dir: &Path) -> Result<Self, StartupError> {
        let path = data_dir.join(SETTINGS_FILE);
        let settings = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&Zeroizing::new(bytes))
                .map_err(|source| StartupError::SettingsInvalid { source: Box::new(source) })?,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let defaults = AppSettings::default();
                write_atomically(&path, &defaults)
                    .map_err(|source| StartupError::DataDirUnwritable { source })?;
                defaults
            }
            Err(source) => {
                return Err(StartupError::Failed {
                    context: "reading the settings",
                    source: Box::new(source),
                });
            }
        };
        Ok(Self { path, current: RwLock::new(settings) })
    }

    pub(crate) async fn get(&self) -> AppSettings {
        self.current.read().await.clone()
    }

    /// Applies `change` to a copy, writes the file, and only then publishes the copy, so
    /// a failed write leaves memory and disk agreeing.
    pub(crate) async fn update(
        &self,
        change: impl FnOnce(&mut AppSettings),
    ) -> Result<(), CoreError> {
        let mut guard = self.current.write().await;
        let mut next = guard.clone();
        change(&mut next);
        if next == *guard {
            return Ok(());
        }
        let path = self.path.clone();
        let written = next.clone();
        blocking(move || {
            write_atomically(&path, &written)
                .map_err(|error| CoreError::storage("the settings file", error))
        })
        .await?;
        *guard = next;
        Ok(())
    }
}

/// Pretty-printed, because the file exists to be read by a person.
fn write_atomically(path: &Path, settings: &AppSettings) -> io::Result<()> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(directory)?;
    let bytes = Zeroizing::new(serde_json::to_vec_pretty(settings)?);
    let mut staged =
        tempfile::Builder::new().prefix(".settings-").suffix(".tmp").tempfile_in(directory)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|error| error.error)?;
    Ok(())
}

impl WalletEngine {
    pub async fn app_settings(&self) -> AppSettings {
        self.inner.settings.get().await
    }

    /// Retunes the live session's auto-lock, including from off to a value and
    /// back. Zero switches it off.
    pub async fn set_auto_lock_minutes(&self, minutes: u32) -> Result<(), CoreError> {
        self.inner.settings.update(|settings| settings.auto_lock_minutes = minutes).await?;
        self.inner.session.set_timeout(auto_lock_duration(minutes));
        Ok(())
    }

    pub async fn set_etherscan_api_key(&self, key: Option<SecretText>) -> Result<(), CoreError> {
        self.inner.settings.update(|settings| settings.etherscan_api_key = key).await?;
        self.endpoints_changed();
        Ok(())
    }

    pub async fn set_trongrid_api_key(&self, key: Option<SecretText>) -> Result<(), CoreError> {
        self.inner.settings.update(|settings| settings.trongrid_api_key = key).await?;
        self.endpoints_changed();
        Ok(())
    }

    pub async fn set_price_feed_enabled(&self, is_enabled: bool) -> Result<(), CoreError> {
        self.inner.settings.update(|settings| settings.set_price_feed_enabled(is_enabled)).await
    }

    pub async fn set_price_chart_enabled(&self, is_enabled: bool) -> Result<(), CoreError> {
        self.inner.settings.update(|settings| settings.is_price_chart_enabled = is_enabled).await
    }

    pub async fn set_amount_entry(&self, entry: AmountEntry) -> Result<(), CoreError> {
        self.inner.settings.update(|settings| settings.amount_entry = entry).await
    }

    pub async fn set_proxy(&self, proxy: Option<ProxySetting>) -> Result<(), CoreError> {
        self.inner.settings.update(|settings| settings.proxy = proxy).await?;
        self.endpoints_changed();
        Ok(())
    }

    pub async fn set_backdrop(&self, backdrop: BackdropSetting) -> Result<(), CoreError> {
        self.inner.settings.update(|settings| settings.backdrop = backdrop).await
    }

    pub async fn set_shown_asset(&self, asset: AssetId) -> Result<(), CoreError> {
        self.inner.settings.update(|settings| settings.show_asset(asset)).await
    }

    pub async fn set_fiat_currency(&self, currency: FiatCurrency) -> Result<(), CoreError> {
        self.inner.settings.update(|settings| settings.fiat_currency = currency).await
    }
}

pub(crate) fn auto_lock_duration(minutes: u32) -> Duration {
    Duration::from_secs(u64::from(minutes).saturating_mul(60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_convert_to_a_timeout_and_zero_is_off() {
        assert_eq!(auto_lock_duration(0), Duration::ZERO);
        assert_eq!(auto_lock_duration(15), Duration::from_mins(15));
        // The largest value stays a long timeout rather than wrapping into a short one.
        assert!(auto_lock_duration(u32::MAX) > Duration::from_secs(60));
    }

    #[test]
    fn the_defaults_round_trip_through_the_file_form() {
        let text = serde_json::to_string_pretty(&AppSettings::default()).unwrap();
        let parsed: AppSettings = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, AppSettings::default());
        assert!(text.contains("\"autoLockMinutes\": 15"), "{text}");
    }

    #[test]
    fn backdrops_and_amount_entries_are_stored_as_their_wire_strings() {
        assert_eq!(BackdropSetting::ALL.map(BackdropSetting::as_str), ["blur", "solid"]);
        assert_eq!(AmountEntry::ALL.map(AmountEntry::as_str), ["fiat", "units"]);
        for backdrop in BackdropSetting::ALL {
            let stored = serde_json::to_value(backdrop).unwrap();
            assert_eq!(stored, backdrop.as_str());
            assert_eq!(serde_json::from_value::<BackdropSetting>(stored).unwrap(), backdrop);
            assert_eq!(BackdropSetting::parse(backdrop.as_str()), Some(backdrop));
        }
        for entry in AmountEntry::ALL {
            let stored = serde_json::to_value(entry).unwrap();
            assert_eq!(stored, entry.as_str());
            assert_eq!(serde_json::from_value::<AmountEntry>(stored).unwrap(), entry);
            assert_eq!(AmountEntry::parse(entry.as_str()), Some(entry));
        }
    }

    #[test]
    fn a_file_missing_a_key_reads_as_its_default() {
        let parsed: AppSettings = serde_json::from_str(r#"{"autoLockMinutes":5}"#).unwrap();
        assert_eq!(parsed.auto_lock_minutes, 5);
        assert!(parsed.is_price_feed_enabled);
        assert!(parsed.is_price_chart_enabled);
        assert_eq!(parsed.backdrop, BackdropSetting::Blur);
        assert_eq!(parsed.shown_chain, None);
    }

    #[tokio::test]
    async fn unknown_settings_are_ignored_and_omitted_from_later_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(SETTINGS_FILE);
        let original =
            br#"{"unusedSetting":{"value":"unused"},"autoLockMinutes":5,"backdrop":"solid"}"#;
        fs::write(&path, original).unwrap();

        let settings = Settings::load(directory.path()).unwrap();
        let loaded = settings.get().await;
        assert_eq!(loaded.auto_lock_minutes, 5);
        assert_eq!(loaded.backdrop, BackdropSetting::Solid);
        assert_eq!(fs::read(&path).unwrap(), original);

        settings.update(|settings| settings.auto_lock_minutes = 10).await.unwrap();
        let stored: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert!(stored.get("unusedSetting").is_none());
        assert_eq!(stored["backdrop"], "solid");
        assert_eq!(stored["autoLockMinutes"], 10);
    }

    #[test]
    fn a_malformed_known_setting_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(SETTINGS_FILE), br#"{"autoLockMinutes":"five"}"#).unwrap();
        assert!(matches!(
            Settings::load(directory.path()),
            Err(StartupError::SettingsInvalid { .. })
        ));
    }

    #[test]
    fn an_unknown_token_is_refused() {
        let refused = serde_json::from_str::<AppSettings>(r#"{"shownTokens":["usdc_spi"]}"#);
        assert!(refused.is_err(), "a misspelt token must not be silently dropped");
    }

    #[test]
    fn showing_an_asset_replaces_only_its_own_chain() {
        let mut settings = AppSettings::default();
        settings.show_asset(AssetId::Token(TokenId::UsdtErc20));
        settings.show_asset(AssetId::Token(TokenId::UsdcSpl));
        assert_eq!(settings.shown_tokens, BTreeSet::from([TokenId::UsdtErc20, TokenId::UsdcSpl]));
        assert_eq!(settings.shown_chain, Some(ChainId::Solana));

        settings.show_asset(AssetId::Token(TokenId::UsdcErc20));
        assert_eq!(settings.shown_tokens, BTreeSet::from([TokenId::UsdcErc20, TokenId::UsdcSpl]));

        settings.show_asset(AssetId::Native(ChainId::Solana));
        assert_eq!(settings.shown_tokens, BTreeSet::from([TokenId::UsdcErc20]));

        settings.show_asset(AssetId::Native(ChainId::Bitcoin));
        assert_eq!(settings.shown_tokens, BTreeSet::from([TokenId::UsdcErc20]));
        assert_eq!(settings.shown_chain, Some(ChainId::Bitcoin));
    }

    #[test]
    fn switching_the_price_feed_overrides_the_amount_entry() {
        let mut settings =
            AppSettings { amount_entry: AmountEntry::Units, ..AppSettings::default() };
        settings.set_price_feed_enabled(true);
        assert_eq!(settings.amount_entry, AmountEntry::Fiat);

        settings.set_price_feed_enabled(false);
        assert!(!settings.is_price_feed_enabled);
        assert_eq!(settings.amount_entry, AmountEntry::Units);
    }

    #[test]
    fn debug_leaves_out_the_api_keys() {
        let settings = AppSettings {
            etherscan_api_key: Some(SecretText::new(String::from("SUPERSECRETVALUE"))),
            ..AppSettings::default()
        };
        let stored = serde_json::to_string(&settings).unwrap();
        let parsed: AppSettings = serde_json::from_str(&stored).unwrap();
        assert_eq!(parsed, settings);
        let rendered = format!("{settings:?}");
        assert!(!rendered.contains("SUPERSECRETVALUE"), "{rendered}");
        assert!(rendered.contains("has_etherscan_api_key: true"), "{rendered}");
        assert!(rendered.contains("has_trongrid_api_key: false"), "{rendered}");
    }

    #[tokio::test]
    async fn a_change_is_on_disk_before_it_is_answered() {
        let directory = tempfile::tempdir().unwrap();
        let settings = Settings::load(directory.path()).unwrap();
        assert!(directory.path().join(SETTINGS_FILE).is_file(), "the defaults were written");

        settings.update(|settings| settings.auto_lock_minutes = 3).await.unwrap();
        let reloaded = Settings::load(directory.path()).unwrap();
        assert_eq!(reloaded.get().await.auto_lock_minutes, 3);
        assert_eq!(settings.get().await.auto_lock_minutes, 3);
    }

    #[test]
    fn a_file_that_does_not_parse_is_a_startup_failure() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(SETTINGS_FILE), b"{ not json").unwrap();
        assert!(matches!(
            Settings::load(directory.path()),
            Err(StartupError::SettingsInvalid { .. })
        ));
    }

    /// The file is there and cannot be read, which says nothing about whether the
    /// directory can be written.
    #[test]
    fn a_file_that_cannot_be_read_is_a_read_failure() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join(SETTINGS_FILE)).unwrap();
        let loaded = Settings::load(directory.path());
        assert!(
            matches!(loaded, Err(StartupError::Failed { context: "reading the settings", .. })),
            "{loaded:?}"
        );
    }
}
