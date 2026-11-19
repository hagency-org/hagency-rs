//! The language of the notices Hagency writes into Matrix rooms (activity,
//! waiting, uncertain-outcome and approval receipts). Presentation only: it
//! never changes what a notice means, when it is sent, or any authority.
use serde::{Deserialize, Serialize};

/// An operator-chosen notice language. English is the default; `zh` keeps
/// the wording of the retained product the notices were ported from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeLanguage {
    #[default]
    En,
    Id,
    Zh,
}

impl NoticeLanguage {
    pub const ALL: [Self; 3] = [Self::En, Self::Id, Self::Zh];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Id => "id",
            Self::Zh => "zh",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::NoticeLanguage;

    #[test]
    fn notice_language_is_a_closed_lowercase_set_defaulting_to_english() {
        assert_eq!(NoticeLanguage::default(), NoticeLanguage::En);
        for language in NoticeLanguage::ALL {
            let json = serde_json::to_string(&language).unwrap();
            assert_eq!(json, format!("\"{}\"", language.as_str()));
            assert_eq!(
                serde_json::from_str::<NoticeLanguage>(&json).unwrap(),
                language
            );
        }
        for refused in ["\"EN\"", "\"fr\"", "\"\"", "1"] {
            assert!(serde_json::from_str::<NoticeLanguage>(refused).is_err());
        }
    }
}
