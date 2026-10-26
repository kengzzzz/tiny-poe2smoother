//! In-game style search: space-separated terms are ANDed regexes, "quotes" group
//! words, and `!` excludes. Terms also match as literal substrings, so pasted stat
//! ids like `map_monsters_damage_+%` and half-typed regex still work.

use regex::{Regex, RegexBuilder};

pub struct SearchQuery {
    terms: Vec<Term>,
}

struct Term {
    negated: bool,
    /// `None` when the term isn't a valid regex. The literal still applies.
    regex: Option<Regex>,
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

    /// Both fields must already be lowercase.
    pub fn matches(&self, stat_id_lower: &str, text_lower: &str) -> bool {
        self.terms.iter().all(|term| {
            let hit = term.hits(stat_id_lower) || term.hits(text_lower);
            hit != term.negated
        })
    }
}

impl Term {
    fn hits(&self, text_lower: &str) -> bool {
        if let Some(regex) = &self.regex
            && regex.is_match(text_lower)
        {
            return true;
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
        // A pasted stat id is valid regex that never matches its own text.
        let query = SearchQuery::parse("map_monsters_damage_+%");
        assert!(query.matches("map_monsters_damage_+%", ""));
        assert!(!query.matches("map_monsters_life_+%", ""));
    }

    #[test]
    fn tester_reported_regex_queries_parse_and_match_without_panicking() {
        let queries = [
            "\"y: r\"",
            "\"(rra)\"",
            "\"(1[0-8]) us\"",
            "\"^w|ts f|f rare m|rity$|ndo\"",
        ];
        let texts = [
            "waystone drop chance: +{0}%",
            "{0}% increased rarity of items found in this area",
            "15 uses remaining",
            "{0}% more monster life",
            "",
        ];
        for query in queries {
            let parsed = SearchQuery::parse(query);
            for text in texts {
                parsed.matches("map_monsters_damage_+%", text);
            }
        }
        // `(…)` is a regex group, as in game.
        assert!(matches("\"(1[0-8]) us\"", "15 uses remaining"));
        assert!(!matches("\"(1[0-8]) us\"", "19 uses remaining"));
        assert!(matches(
            "\"^w|ts f|f rare m|rity$|ndo\"",
            "waystone drop chance"
        ));
        assert!(matches(
            "\"^w|ts f|f rare m|rity$|ndo\"",
            "{0}% increased rarity"
        ));
        assert!(matches("\"y: r\"", "rarity: rare"));
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
