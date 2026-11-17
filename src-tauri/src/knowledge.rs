//! Built-in game knowledge. For a game the tool knows (GTA VI), the AI gets the real facts and names,
//! the speech-to-text is told the unusual words, and misspelt names in captions are put right.
//! You can add your own lines in `knowledge/gta6.txt` inside the data folder.

use crate::models::{Settings, Word};
use std::path::Path;

pub struct Pack {
    pub id: &'static str,
    pub name: &'static str,
    facts: &'static str,
    /// Words the speech-to-text should expect.
    pub terms: &'static [&'static str],
    pub hashtags: &'static [&'static str],
    /// Words that tell us a video is about this game.
    keywords: &'static [&'static str],
    /// Lowercase word -> the way it is written.
    casing: &'static [(&'static str, &'static str)],
}

pub static GTA6: Pack = Pack {
    id: "gta6",
    name: "GTA VI",
    facts: include_str!("knowledge/gta6.txt"),
    terms: &[
        "GTA 6", "GTA VI", "Grand Theft Auto VI", "Rockstar Games", "Take-Two", "Leonida", "Vice City", "Leonida Keys", "Grassrivers",
        "Port Gellhorn", "Ambrosia", "Mount Kalaga", "Lucia Caminos", "Jason Duval", "Cal Hampton", "Boobie Ike", "Dre'Quan Priest",
        "Real Dimez", "Only Raw Records", "Brian Heder",
    ],
    hashtags: &["gta6", "gtavi", "vicecity", "leonida", "rockstargames", "lucia", "jasonduval", "gta6trailer"],
    keywords: &["gta 6", "gta vi", "gta6", "gtavi", "grand theft auto", "vice city", "leonida", "lucia caminos", "jason duval", "lucia", "rockstar"],
    casing: &[
        ("leonida", "Leonida"), ("lucia", "Lucia"), ("caminos", "Caminos"), ("duval", "Duval"), ("rockstar", "Rockstar"),
        ("gta", "GTA"), ("gtavi", "GTA VI"), ("gellhorn", "Gellhorn"), ("grassrivers", "Grassrivers"), ("kalaga", "Kalaga"), ("dimez", "Dimez"),
    ],
};

fn count_hits(hay: &str, p: &Pack) -> usize {
    p.keywords.iter().filter(|k| hay.contains(*k)).count()
}

/// The pack for this video: forced by the setting, or noticed from the title (and, once there is one, the speech).
pub fn pack_for(settings: &Settings, title: &str, words: &[Word]) -> Option<&'static Pack> {
    match settings.knowledge_profile.as_str() {
        "off" => None,
        "gta6" => Some(&GTA6),
        _ => {
            let mut hay = title.to_lowercase();
            if !words.is_empty() {
                hay.push(' ');
                hay.push_str(&words.iter().take(3000).map(|w| w.text.to_lowercase()).collect::<Vec<_>>().join(" "));
            }
            let in_title = count_hits(&title.to_lowercase(), &GTA6) > 0;
            (in_title || count_hits(&hay, &GTA6) >= 2).then_some(&GTA6)
        }
    }
}

/// The text added to the AI's instructions.
pub fn block(p: &Pack, data_dir: &Path) -> String {
    let mut s = format!("GAME KNOWLEDGE for this video ({}). Use it to understand names and context. Do not contradict it and do not invent facts beyond it.\n{}", p.name, p.facts.trim());
    if let Ok(extra) = std::fs::read_to_string(data_dir.join("knowledge").join(format!("{}.txt", p.id))) {
        let extra = extra.trim();
        if !extra.is_empty() {
            s.push_str("\n\nYour own notes:\n");
            s.push_str(&extra.chars().take(6000).collect::<String>());
        }
    }
    s
}

/// Unusual words for the speech-to-text, including any in the user's own notes after a "Terms:" line.
pub fn terms(p: &Pack, data_dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = p.terms.iter().map(|t| t.to_string()).collect();
    if let Ok(extra) = std::fs::read_to_string(data_dir.join("knowledge").join(format!("{}.txt", p.id))) {
        for line in extra.lines() {
            if let Some(rest) = line.trim().strip_prefix("Terms:") {
                v.extend(rest.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty() && t.split_whitespace().count() <= 6));
            }
        }
    }
    v.truncate(200);
    v
}

/// "Names: a, b, c." as a short hint for Whisper-style models.
pub fn prompt_hint(terms: &[String]) -> String {
    let mut s = String::from("Names and terms: ");
    for t in terms {
        if s.len() + t.len() > 380 {
            break;
        }
        s.push_str(t);
        s.push_str(", ");
    }
    s.trim_end_matches(", ").to_string() + "."
}

/// Put the game's names in the right case. Returns how many words changed.
pub fn fix_words(p: &Pack, words: &mut [Word]) -> usize {
    let mut n = 0;
    for i in 0..words.len() {
        let core: String = words[i].text.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        if core.is_empty() {
            continue;
        }
        let lead: String = words[i].text.chars().take_while(|c| !c.is_alphanumeric()).collect();
        let tail: String = words[i].text.chars().rev().take_while(|c| !c.is_alphanumeric()).collect::<Vec<_>>().into_iter().rev().collect();
        let fixed = if core == "vice" && words.get(i + 1).map(|w| w.text.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase() == "city").unwrap_or(false) {
            Some("Vice")
        } else if core == "city" && i > 0 && words[i - 1].text.trim_matches(|c: char| !c.is_alphanumeric()).eq_ignore_ascii_case("vice") {
            Some("City")
        } else {
            p.casing.iter().find(|(k, _)| *k == core).map(|(_, v)| *v)
        };
        if let Some(f) = fixed {
            let new = format!("{lead}{f}{tail}");
            if new != words[i].text {
                words[i].text = new;
                n += 1;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(t: &str) -> Word {
        Word { text: t.into(), start: 0.0, end: 0.1, speaker: None }
    }

    #[test]
    fn the_game_is_noticed_from_the_title_or_the_speech() {
        let mut s = Settings::default();
        s.knowledge_profile = "auto".into();
        assert!(pack_for(&s, "GTA 6 Trailer 3 breakdown", &[]).is_some());
        assert!(pack_for(&s, "Podcast ep 12", &[]).is_none());
        let speech: Vec<Word> = "so in vice city lucia and jason".split(' ').map(w).collect();
        assert!(pack_for(&s, "Podcast ep 12", &speech).is_some());
        s.knowledge_profile = "off".into();
        assert!(pack_for(&s, "GTA 6", &[]).is_none());
        s.knowledge_profile = "gta6".into();
        assert!(pack_for(&s, "anything", &[]).is_some());
    }

    #[test]
    fn names_get_their_capitals() {
        let mut v: Vec<Word> = "welcome to vice city, lucia and gta, in leonida.".split(' ').map(w).collect();
        let n = fix_words(&GTA6, &mut v);
        let text: Vec<&str> = v.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(text, ["welcome", "to", "Vice", "City,", "Lucia", "and", "GTA,", "in", "Leonida."]);
        assert_eq!(n, 5);
    }

    #[test]
    fn hints_stay_short() {
        let t = terms(&GTA6, Path::new("."));
        assert!(t.contains(&"Leonida".to_string()));
        assert!(prompt_hint(&t).len() < 420);
    }
}
