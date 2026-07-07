//! PoE2-style search queries for the color-mods editor, mirroring the
//! in-game search bar: space-separated terms are ANDed, each term is a
//! case-insensitive regex, `"quoted blocks"` keep spaces inside one term,
//! and a leading `!` (inside or outside the quotes, both work in game)
//! excludes rows the term matches.
//!
//! One deliberate extension over the game: a term also matches as a literal
//! substring. This keeps plain-text queries working exactly as before regex
//! support — including pasted stat ids like `map_monsters_damage_+%`, which
//! compile as regex but would never match their own text — and gives invalid
//! regex (e.g. a `(` mid-typing) a graceful meaning instead of no results.

use regex::{Regex, RegexBuilder};

pub struct SearchQuery {
    terms: Vec<Term>,
}

struct Term {
    negated: bool,
    /// `None` when the term is not a valid regex; the literal still applies.
    regex: Option<Regex>,
    /// Lowercased raw term, matched with `contains` against lowercased text.
    literal: String,
}

impl SearchQuery {
    pub fn parse(query: &str) -> Self {
        let mut terms = Vec::new();
        let mut rest = query;
        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            let mut negated = false;
            if let Some(stripped) = rest.strip_prefix('!') {
                negated = true;
                rest = stripped;
            }
            let raw = if let Some(quoted) = rest.strip_prefix('"') {
                match quoted.find('"') {
                    Some(end) => {
                        rest = &quoted[end + 1..];
                        &quoted[..end]
                    }
                    // Unclosed quote: take the rest, as the game does.
                    None => {
                        rest = "";
                        quoted
                    }
                }
            } else {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let raw = &rest[..end];
                rest = &rest[end..];
                raw
            };
            // In game the `!` conventionally sits inside the quotes.
            let term = match raw.strip_prefix('!') {
                Some(stripped) => {
                    negated = true;
                    stripped
                }
                None => raw,
            };
            if term.is_empty() {
                continue;
            }
            terms.push(Term {
                negated,
                regex: RegexBuilder::new(term)
                    .case_insensitive(true)
                    .size_limit(1 << 20)
                    .build()
                    .ok(),
                literal: term.to_lowercase(),
            });
        }
        Self { terms }
    }

    /// True when every term matches (or, if negated, fails to match) at
    /// least one of the two fields. Both fields must already be lowercase.
    pub fn matches(&self, stat_id_lower: &str, text_lower: &str) -> bool {
        self.terms.iter().all(|term| {
            let hit = term.hits(stat_id_lower) || term.hits(text_lower);
            hit != term.negated
        })
    }
}

impl Term {
    fn hits(&self, text_lower: &str) -> bool {
        if let Some(regex) = &self.regex {
            if regex.is_match(text_lower) {
                return true;
            }
        }
        text_lower.contains(&self.literal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(query: &str, text: &str) -> bool {
        SearchQuery::parse(query).matches("", &text.to_lowercase())
    }

    #[test]
    fn plain_words_match_as_substrings_in_any_order() {
        let text = "{0}% increased chance to be [Omen|Omens]";
        assert!(matches("increase chance to be omen", text));
        assert!(matches("omen chance", text));
        assert!(!matches("omen damage", text));
        // Empty and whitespace-only queries match everything.
        assert!(matches("", text));
        assert!(matches("   ", text));
    }

    #[test]
    fn regex_terms_match_case_insensitively() {
        assert!(matches("fire|cold", "Adds {0} Cold Damage"));
        assert!(matches("fire|cold", "Adds {0} FIRE Damage"));
        assert!(!matches("fire|cold", "Adds {0} Lightning Damage"));
        assert!(matches(
            r"\{0\}% (more|increased)",
            "{0}% more Monster Life"
        ));
        assert!(matches("^adds", "Adds {0} Cold Damage"));
        assert!(!matches("^cold", "Adds {0} Cold Damage"));
    }

    #[test]
    fn quoted_blocks_keep_phrases_together() {
        let text = "{0}% more Monster Life";
        assert!(matches("\"more monster\"", text));
        assert!(!matches("\"monster more\"", text));
        // Unquoted, both words still match independently (AND).
        assert!(matches("monster more", text));
        // Unclosed quote takes the rest of the query.
        assert!(matches("\"more monster", text));
    }

    #[test]
    fn bang_negates_inside_or_outside_quotes() {
        let text = "{0}% more Monster Damage";
        assert!(matches("damage !life", text));
        assert!(!matches("damage !monster", text));
        assert!(!matches("\"!more monster\"", text));
        assert!(!matches("!\"more monster\"", text));
        // A bare `!` contributes nothing.
        assert!(matches("! damage", text));
    }

    #[test]
    fn literal_fallback_covers_invalid_regex_and_stat_id_pastes() {
        // `+5` is not a valid regex (dangling repeat) but should match text.
        assert!(matches("+5", "+5 to Level of all Skills"));
        // A pasted stat id is a *valid* regex (`_+%` = underscores then %)
        // that never matches its own text; the literal fallback finds it.
        let query = SearchQuery::parse("map_monsters_damage_+%");
        assert!(query.matches("map_monsters_damage_+%", ""));
        assert!(!query.matches("map_monsters_life_+%", ""));
    }

    #[test]
    fn terms_match_either_stat_id_or_display_text() {
        let query = SearchQuery::parse("ritual omen");
        assert!(query.matches(
            "map_ritual_omen_chance_+%",
            "favours in map have {0}% increased chance to be omens",
        ));
        // One term hitting the id and the other the text still counts.
        let query = SearchQuery::parse("ritual_omen increased");
        assert!(query.matches(
            "map_ritual_omen_chance_+%",
            "favours in map have {0}% increased chance to be omens",
        ));
    }
}
