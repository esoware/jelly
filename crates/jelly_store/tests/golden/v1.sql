CREATE TABLE addresses (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  wallet_id        INTEGER NOT NULL REFERENCES wallets(id) ON DELETE CASCADE,
  chain            TEXT    NOT NULL,
  address          TEXT    NOT NULL,
  key_source       TEXT    NOT NULL CHECK (key_source IN ('derived','imported','subaddress')),
  derivation_path  TEXT,
  vault_entry      TEXT,
  subaddress_major     INTEGER CHECK (subaddress_major IS NULL OR subaddress_major >= 0),
  subaddress_minor     INTEGER CHECK (subaddress_minor IS NULL OR subaddress_minor >= 0),
  script_kind      TEXT CHECK (script_kind IS NULL OR script_kind IN
    ('p2tr','p2wpkh','p2sh_p2wpkh','p2pkh','ton_v5r1','ton_v4r2','ton_v3r2')),
  derivation_index INTEGER CHECK (derivation_index IS NULL OR derivation_index >= 0),
  label            TEXT,
  created_at       INTEGER NOT NULL,
  CHECK (
    (key_source = 'derived'
       AND derivation_path IS NOT NULL AND vault_entry IS NULL
       AND subaddress_major IS NULL AND subaddress_minor IS NULL)
    OR (key_source = 'imported'
       AND vault_entry IS NOT NULL AND derivation_path IS NULL
       AND subaddress_major IS NULL AND subaddress_minor IS NULL)
    OR (key_source = 'subaddress'
       AND subaddress_major IS NOT NULL AND subaddress_minor IS NOT NULL
       AND derivation_path IS NULL AND vault_entry IS NULL)
  ),
  UNIQUE (wallet_id, chain, address)
) STRICT;

CREATE TABLE balances (
  address_id INTEGER NOT NULL REFERENCES addresses(id) ON DELETE CASCADE,
  asset      TEXT    NOT NULL,
  amount     BLOB    NOT NULL CHECK (length(amount) = 16),
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (address_id, asset)
) STRICT, WITHOUT ROWID;

CREATE TABLE contacts (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  wallet_id  INTEGER NOT NULL REFERENCES wallets(id) ON DELETE CASCADE,
  chain      TEXT    NOT NULL,
  address    TEXT    NOT NULL,
  name       TEXT    NOT NULL CHECK (length(name) > 0),
  note       TEXT,
  created_at INTEGER NOT NULL,
  UNIQUE (wallet_id, chain, address)
) STRICT;

CREATE TABLE endpoint_health (
  chain                TEXT    NOT NULL,
  url                  TEXT    NOT NULL,
  score_permille       INTEGER NOT NULL CHECK (score_permille BETWEEN 0 AND 1000),
  successes            INTEGER NOT NULL CHECK (successes >= 0),
  failures             INTEGER NOT NULL CHECK (failures >= 0),
  consecutive_failures INTEGER NOT NULL CHECK (consecutive_failures >= 0),
  latency_ms           INTEGER CHECK (latency_ms IS NULL OR latency_ms >= 0),
  last_success         INTEGER,
  last_failure         INTEGER,
  last_error           TEXT,
  PRIMARY KEY (chain, url)
) STRICT, WITHOUT ROWID;

CREATE TABLE endpoint_overrides (
  chain   TEXT    NOT NULL,
  url     TEXT    NOT NULL,
  display TEXT    NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
  pinned  INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0,1)),
  PRIMARY KEY (chain, url)
) STRICT, WITHOUT ROWID;

CREATE TABLE scan_cursors (
  address_id INTEGER NOT NULL REFERENCES addresses(id) ON DELETE CASCADE,
  namespace  TEXT    NOT NULL,
  cursor     BLOB    NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (address_id, namespace)
) STRICT, WITHOUT ROWID;

CREATE TABLE tls_pins (
  chain       TEXT    NOT NULL,
  host        TEXT    NOT NULL,
  spki_sha256 BLOB    NOT NULL CHECK (length(spki_sha256) = 32),
  first_seen  INTEGER NOT NULL,
  PRIMARY KEY (chain, host)
) STRICT, WITHOUT ROWID;

CREATE TABLE txs (
  id           INTEGER PRIMARY KEY,
  wallet_id    INTEGER NOT NULL REFERENCES wallets(id) ON DELETE CASCADE,
  address_id   INTEGER NOT NULL REFERENCES addresses(id) ON DELETE CASCADE,
  chain        TEXT    NOT NULL,
  asset        TEXT    NOT NULL,
  txid         TEXT    NOT NULL,
  direction    TEXT    NOT NULL CHECK (direction IN ('incoming','outgoing','self')),
  outcome      TEXT    NOT NULL CHECK (outcome IN ('success','failed','replaced','unknown')),
  detection    TEXT    NOT NULL CHECK (detection IN ('full','balance_delta')),
  amount       BLOB    NOT NULL CHECK (length(amount) = 16),
  fee          BLOB    CHECK (fee IS NULL OR length(fee) = 16),
  rent         BLOB    CHECK (rent IS NULL OR length(rent) = 16),
  counterparty TEXT,
  pays_for     TEXT,
  height       INTEGER CHECK (height IS NULL OR height >= 0),
  timestamp    INTEGER,
  created_at   INTEGER NOT NULL,
  sort_time    INTEGER NOT NULL GENERATED ALWAYS AS (COALESCE(timestamp, created_at)) STORED,
  UNIQUE (address_id, asset, txid)
) STRICT;

CREATE INDEX idx_txs_chain_txid ON txs (chain, txid);

CREATE INDEX idx_txs_wallet_time ON txs (wallet_id, sort_time DESC, id DESC);

CREATE TABLE wallet_chain_state (
  wallet_id             INTEGER NOT NULL REFERENCES wallets(id) ON DELETE CASCADE,
  chain                 TEXT    NOT NULL,
  enabled               INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
  poll_secs             INTEGER CHECK (poll_secs IS NULL OR poll_secs > 0),
  restore_height INTEGER CHECK (restore_height IS NULL OR restore_height >= 0),
  synced_height         INTEGER CHECK (synced_height IS NULL OR synced_height >= 0),
  PRIMARY KEY (wallet_id, chain)
) STRICT, WITHOUT ROWID;

CREATE TABLE wallet_settings (
  wallet_id         INTEGER PRIMARY KEY REFERENCES wallets(id) ON DELETE CASCADE,
  passphrase_policy TEXT    NOT NULL CHECK (passphrase_policy IN ('none','stored','prompt')),
  account_index    INTEGER NOT NULL DEFAULT 0 CHECK (account_index >= 0)
) STRICT;

CREATE TABLE wallets (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT    NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  color      TEXT NOT NULL DEFAULT '#71717a'
    CHECK (color GLOB '#[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]')
) STRICT;
