//! Schema migrations on the raw TOML table, before typed deserialization.

use toml::{Table, Value};

use super::model::SCHEMA_VERSION;

type Migration = fn(&mut Table);

/// `MIGRATIONS[n]` upgrades a version-`n` file to version `n + 1`.
const MIGRATIONS: &[Migration] = &[v0_to_v1];

const _: () = assert!(MIGRATIONS.len() == SCHEMA_VERSION as usize);

/// Version 0 is a file without `version` (written before the schema was
/// versioned); its layout is the same as version 1.
fn v0_to_v1(_: &mut Table) {}

const HIDE_SECRET_VALUES: &str = "hide_secret_values";

/// The "Development" preset as it was stored before #109. Its agent keys
/// are ignored since #15, but old files still have them.
const OLD_DEVELOPMENT: &[(&str, OldValue)] = &[
    ("mcp_enabled", OldValue::Bool(true)),
    ("max_profile", OldValue::Str("deploy")),
    ("temporary_objects", OldValue::Bool(true)),
    ("deploy", OldValue::Str("apply-with-confirmation")),
    ("exec", OldValue::Str("confirm-each")),
    ("session_ttl_minutes", OldValue::Int(120)),
    ("namespaces", OldValue::Str("multiple")),
];

/// Keys added to the preset later, with the preset's value; a file may
/// lack them.
const OLD_DEVELOPMENT_LATER: &[(&str, OldValue)] = &[
    ("agent_secrets", OldValue::Str("reveal-on-request")),
    ("require_server_enforced", OldValue::Bool(false)),
    ("agent_other_kinds", OldValue::Bool(false)),
    ("protected", OldValue::Bool(false)),
];

enum OldValue {
    Bool(bool),
    Str(&'static str),
    Int(i64),
}

impl OldValue {
    fn matches(&self, value: &Value) -> bool {
        match (self, value) {
            (Self::Bool(want), Value::Boolean(have)) => want == have,
            (Self::Str(want), Value::String(have)) => want == have,
            (Self::Int(want), Value::Integer(have)) => want == have,
            _ => false,
        }
    }
}

/// Policies saved before `hide_secret_values` existed (#109) lack the key,
/// which then defaults to hiding. One that was the Development preset gets
/// `false`, as that preset showed Secret values, so the user keeps seeing
/// them. Other old policies keep hiding values, and contexts without a
/// stored policy hide them.
///
/// Not a schema migration: the key is additive, and a version bump would
/// make files of this version read-only for older Kubach. Every save writes
/// the key, so a missing one always means an older file.
pub(super) fn show_secrets_of_old_development_policies(table: &mut Table) {
    let Some(entries) = table
        .get_mut("security")
        .and_then(|security| security.get_mut("context"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for entry in entries.iter_mut().filter_map(Value::as_table_mut) {
        if entry.contains_key(HIDE_SECRET_VALUES) {
            continue;
        }
        let development = OLD_DEVELOPMENT
            .iter()
            .all(|(key, want)| entry.get(*key).is_some_and(|have| want.matches(have)))
            && OLD_DEVELOPMENT_LATER
                .iter()
                .all(|(key, want)| entry.get(*key).is_none_or(|have| want.matches(have)));
        if development {
            entry.insert(HIDE_SECRET_VALUES.into(), Value::Boolean(false));
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Current,
    Migrated {
        from: u32,
    },
    /// Written by a newer Kubach; loaded as is, unknown keys ignored.
    Newer {
        version: u32,
    },
}

pub(super) fn migrate(table: &mut Table) -> Result<Outcome, String> {
    migrate_with(table, MIGRATIONS)
}

fn migrate_with(table: &mut Table, migrations: &[Migration]) -> Result<Outcome, String> {
    let latest = migrations.len() as u32;
    let version = match table.get("version") {
        None => 0,
        Some(Value::Integer(version)) => {
            u32::try_from(*version).map_err(|_| format!("invalid schema version {version}"))?
        }
        Some(other) => return Err(format!("invalid schema version {other}")),
    };
    if version > latest {
        return Ok(Outcome::Newer { version });
    }
    if version == latest {
        return Ok(Outcome::Current);
    }
    for migration in &migrations[version as usize..] {
        migration(table);
    }
    table.insert("version".into(), Value::Integer(latest.into()));
    Ok(Outcome::Migrated { from: version })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rename_theme(table: &mut Table) {
        if let Some(theme) = table.remove("theme") {
            let general = table
                .entry("general")
                .or_insert_with(|| Value::Table(Table::new()));
            general
                .as_table_mut()
                .unwrap()
                .insert("theme".into(), theme);
        }
    }

    fn add_marker(table: &mut Table) {
        table.insert("marker".into(), Value::Boolean(true));
    }

    #[test]
    fn runs_pending_migrations_in_order() {
        let mut table: Table = toml::from_str("theme = \"light\"").unwrap();
        let outcome = migrate_with(&mut table, &[rename_theme, add_marker]).unwrap();
        assert_eq!(outcome, Outcome::Migrated { from: 0 });
        assert_eq!(table["general"]["theme"].as_str(), Some("light"));
        assert_eq!(table["marker"].as_bool(), Some(true));
        assert_eq!(table["version"].as_integer(), Some(2));

        let mut table: Table = toml::from_str("version = 1").unwrap();
        let outcome = migrate_with(&mut table, &[rename_theme, add_marker]).unwrap();
        assert_eq!(outcome, Outcome::Migrated { from: 1 });
        assert_eq!(table["marker"].as_bool(), Some(true));
    }

    #[test]
    fn current_and_newer_files_are_untouched() {
        let mut table: Table = toml::from_str("version = 1").unwrap();
        assert_eq!(migrate(&mut table).unwrap(), Outcome::Current);

        let mut table: Table = toml::from_str("version = 99\nfuture = 1").unwrap();
        assert_eq!(migrate(&mut table).unwrap(), Outcome::Newer { version: 99 });
        assert_eq!(table["version"].as_integer(), Some(99));
    }

    #[test]
    fn unversioned_file_becomes_current() {
        let mut table: Table = toml::from_str("[general]\ntheme = \"light\"").unwrap();
        assert_eq!(migrate(&mut table).unwrap(), Outcome::Migrated { from: 0 });
        assert_eq!(
            table["version"].as_integer(),
            Some(i64::from(SCHEMA_VERSION))
        );
    }

    #[test]
    fn rejects_bad_versions() {
        for text in ["version = -1", "version = \"one\""] {
            let mut table: Table = toml::from_str(text).unwrap();
            assert!(migrate(&mut table).is_err(), "{text}");
        }
    }
}
