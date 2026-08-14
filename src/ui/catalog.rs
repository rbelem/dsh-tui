//! The composer's `/` and `@` catalogs (Q16/Q17).
//!
//! Sourcing per the web's evidence (see the e2e/catalog research in the
//! bundle lane): the web's `/` menu pulls `command.list` from an
//! in-process host command registry — there is NO `command.*` RPC in the
//! gateway's rpc-map (packages/host/apiproxy/src/api/rpc-map.ts), so the
//! TUI mirrors the core slash commands statically (client-side registry,
//! like the web's contribution model). The `@` menu sources `skill.list`,
//! the skill domain's only RPC (packages/host/apiproxy/src/api/skills.ts).
//! Subagents (the web's other `@` group) derive from the session-list
//! snapshot's running children with zero RPC, and permission presets are a
//! `/permission` command decoration — both deferred (TODOs below).

use crate::i18n::Locale;

/// One popup row: the group header key, the display label, the hint, and
/// the text Enter inserts (command text or skill name, both with the
/// trailing-space convention that closes the trigger).
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    /// i18n key for the group header line (`catalog.group.*`).
    pub group: &'static str,
    pub label: String,
    pub hint: String,
    pub insert: String,
}

impl CatalogEntry {
    fn command(text: &str, hint_key: &'static str, locale: Locale) -> Self {
        CatalogEntry {
            group: "catalog.group.commands",
            label: text.to_string(),
            hint: crate::i18n::tr(locale, hint_key).to_string(),
            insert: text.to_string(),
        }
    }

    fn skill(name: &str, description: &str) -> Self {
        CatalogEntry {
            group: "catalog.group.skills",
            label: name.to_string(),
            hint: description.to_string(),
            // The insert carries the `@` trigger (the web inserts `@label `):
            // popup accept replaces the whole buffer, so the trigger must
            // ride along with the skill name.
            insert: format!("@{name}"),
        }
    }
}

/// The mirrored core slash commands: (text, hint key). Inserting the text is
/// correct — the gateway dispatches slash text via `session.prompt` (the
/// response's `command:{kind:'success',text?}` slot); `command.execute` (the
/// web's dedicated path with lifecycle nodes) is a later lane.
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "composer.hint_help"),
    ("/compact", "composer.hint_compact"),
    ("/clear", "composer.hint_clear"),
    ("/model", "composer.hint_model"),
    ("/plan", "composer.hint_plan"),
    ("/permission", "composer.hint_permission"),
    ("/skill", "composer.hint_skill"),
];

/// Build the `/` command entries for `locale`.
pub fn slash_entries(locale: Locale) -> Vec<CatalogEntry> {
    SLASH_COMMANDS
        .iter()
        .map(|(text, key)| CatalogEntry::command(text, key, locale))
        .collect()
}

/// Build the `@` skill entries from a `skill.list` result (raw
/// descriptions — no localization; they come from the host).
pub fn skill_entries(skills: &[crate::wire::skills::SkillEntry]) -> Vec<CatalogEntry> {
    skills
        .iter()
        .map(|skill| CatalogEntry::skill(&skill.name, &skill.description))
        .collect()
}

/// Case-insensitive substring filter on the label (the typed suffix after
/// the trigger; the web fuzzy-matches subsequences — v1 keeps substring).
pub fn filter_entries(entries: &[CatalogEntry], suffix: &str) -> Vec<CatalogEntry> {
    let suffix = suffix.to_ascii_lowercase();
    if suffix.is_empty() {
        return entries.to_vec();
    }
    entries
        .iter()
        .filter(|entry| entry.label.to_ascii_lowercase().contains(&suffix))
        .cloned()
        .collect()
}

/// TODO (Q17): the Ctrl+P global launcher — a unified command/skill palette
/// across surfaces. Deferred.
/// TODO: the web's `@` subagent group derives from the session list's
/// running children (zero RPC) — the TUI's session summaries carry no child
/// roster yet. TODO: permission presets are a `/permission` command
/// decoration in the web, not an `@` source.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Locale;

    #[test]
    fn slash_entries_cover_the_core_commands() {
        let entries = slash_entries(Locale::En);
        assert_eq!(entries.len(), SLASH_COMMANDS.len());
        assert_eq!(entries[0].label, "/help");
        assert!(entries.iter().all(|e| e.group == "catalog.group.commands"));
        assert!(entries.iter().all(|e| e.insert == e.label));
    }

    #[test]
    fn skill_entries_carry_the_host_descriptions() {
        let skills = vec![
            crate::wire::skills::SkillEntry {
                name: "commit".into(),
                description: "write a commit message".into(),
                when_to_use: None,
                model_invocable: true,
            },
            crate::wire::skills::SkillEntry {
                name: "triage".into(),
                description: "sort the inbox".into(),
                when_to_use: Some("mail piles up".into()),
                model_invocable: false,
            },
        ];
        let entries = skill_entries(&skills);
        assert_eq!(entries[0].label, "commit");
        assert_eq!(entries[0].hint, "write a commit message");
        assert_eq!(entries[1].insert, "@triage", "the @ trigger rides along");
        assert!(entries.iter().all(|e| e.group == "catalog.group.skills"));
    }

    #[test]
    fn filter_entries_is_case_insensitive_substring() {
        let entries = slash_entries(Locale::En);
        assert_eq!(filter_entries(&entries, "").len(), entries.len());
        assert_eq!(filter_entries(&entries, "MOD").len(), 1);
        assert_eq!(filter_entries(&entries, "MOD")[0].label, "/model");
        assert!(filter_entries(&entries, "zzz").is_empty());
    }
}
