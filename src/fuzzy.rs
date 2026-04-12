use std::sync::LazyLock;

use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};

static FUZZY_MATCHER: LazyLock<SkimMatcherV2> =
    LazyLock::new(|| SkimMatcherV2::default().respect_case());

pub(crate) struct SearchMatcher {
    query: String,
    ignore_case: bool,
    fuzzy: bool,
}

impl SearchMatcher {
    pub fn new(query: &str, ignore_case: bool, fuzzy: bool) -> Self {
        let query = if ignore_case {
            query.to_lowercase()
        } else {
            query.into()
        };
        Self {
            query,
            ignore_case,
            fuzzy,
        }
    }

    /// Quick check if string matches without computing match positions
    pub fn matches(&self, s: &str) -> bool {
        if self.query.is_empty() {
            return false;
        }
        if self.fuzzy {
            let result = if self.ignore_case {
                FUZZY_MATCHER.fuzzy_match(&s.to_lowercase(), &self.query)
            } else {
                FUZZY_MATCHER.fuzzy_match(s, &self.query)
            };
            result.is_some()
        } else if self.ignore_case {
            s.to_lowercase().contains(&self.query)
        } else {
            s.contains(&self.query)
        }
    }

    pub fn matched_position(&self, s: &str) -> Option<Vec<usize>> {
        if self.query.is_empty() {
            return None;
        }
        if self.fuzzy {
            let result = if self.ignore_case {
                FUZZY_MATCHER.fuzzy_indices(&s.to_lowercase(), &self.query)
            } else {
                FUZZY_MATCHER.fuzzy_indices(s, &self.query)
            };
            result.map(|(_, indices)| indices)
        } else {
            let result = if self.ignore_case {
                s.to_lowercase().find(&self.query)
            } else {
                s.find(&self.query)
            };
            result.map(|p| (p..(p + self.query.len())).collect())
        }
    }
}
