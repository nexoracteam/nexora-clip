//! Hindi and Urdu in Roman letters ("Hinglish" / Roman Urdu) for captions: चूना जो उसको -> chuna jo usko, یار کیا ہے -> yaar kya hai.
//! This is not a translation: it is the same word spelled the way people type it in chats.
//! Everyday words come from a list; the rest are spelled from their letters, which is readable even if not always how a person would type it.

const HINDI_WORDS: &[(&str, &str)] = &[
    ("में", "mein"), ("मैं", "main"), ("हैं", "hain"), ("है", "hai"), ("नहीं", "nahi"), ("नही", "nahi"), ("हूँ", "hoon"), ("हूं", "hoon"),
    ("क्या", "kya"), ("क्यों", "kyun"), ("यह", "yeh"), ("ये", "ye"), ("वह", "woh"), ("वो", "wo"), ("और", "aur"), ("भी", "bhi"), ("ही", "hi"),
    ("की", "ki"), ("का", "ka"), ("के", "ke"), ("को", "ko"), ("से", "se"), ("पर", "par"), ("जी", "ji"), ("तो", "to"), ("हाँ", "haan"), ("हां", "haan"),
    ("यहाँ", "yahan"), ("यहां", "yahan"), ("वहाँ", "wahan"), ("वहां", "wahan"), ("कहाँ", "kahan"), ("कहां", "kahan"), ("था", "tha"), ("थी", "thi"),
    ("थे", "the"), ("रहा", "raha"), ("रही", "rahi"), ("रहे", "rahe"), ("कभी", "kabhi"), ("अभी", "abhi"), ("सभी", "sabhi"), ("भाई", "bhai"),
    ("मुझे", "mujhe"), ("तुम", "tum"), ("हम", "hum"), ("आप", "aap"), ("बहुत", "bahut"), ("अच्छा", "accha"), ("ठीक", "theek"), ("लिए", "liye"),
    ("एक", "ek"), ("दो", "do"), ("कुछ", "kuch"), ("सब", "sab"), ("कैसे", "kaise"), ("कैसा", "kaisa"), ("फिर", "phir"), ("बस", "bas"), ("गया", "gaya"),
    ("गई", "gayi"), ("किया", "kiya"), ("करना", "karna"), ("कर", "kar"), ("लेकिन", "lekin"), ("अगर", "agar"), ("तब", "tab"), ("जब", "jab"),
    ("यार", "yaar"), ("देखो", "dekho"), ("चलो", "chalo"), ("मतलब", "matlab"), ("सच", "sach"), ("बात", "baat"), ("लोग", "log"), ("दिन", "din"),
];

const URDU_WORDS: &[(&str, &str)] = &[
    ("ہے", "hai"), ("ہیں", "hain"), ("میں", "main"), ("کیا", "kya"), ("نہیں", "nahi"), ("اور", "aur"), ("یہ", "yeh"), ("وہ", "woh"), ("کے", "ke"),
    ("کی", "ki"), ("کا", "ka"), ("کو", "ko"), ("سے", "se"), ("پر", "par"), ("تو", "to"), ("بھی", "bhi"), ("تھا", "tha"), ("تھی", "thi"), ("تھے", "the"),
    ("ہو", "ho"), ("ہوں", "hoon"), ("ایک", "ek"), ("بہت", "bohat"), ("آپ", "aap"), ("ہم", "hum"), ("تم", "tum"), ("یار", "yaar"), ("بھائی", "bhai"),
    ("اچھا", "acha"), ("ٹھیک", "theek"), ("لیکن", "lekin"), ("کہ", "keh"), ("جو", "jo"), ("اس", "is"), ("ان", "in"), ("سب", "sab"), ("کچھ", "kuch"),
    ("کیوں", "kyun"), ("کیسے", "kaise"), ("ابھی", "abhi"), ("اب", "ab"), ("پھر", "phir"), ("بس", "bas"), ("ہاں", "haan"), ("جی", "ji"), ("مجھے", "mujhe"),
    ("رہا", "raha"), ("رہی", "rahi"), ("گیا", "gaya"), ("کر", "kar"), ("کرنا", "karna"), ("دیکھو", "dekho"), ("چلو", "chalo"), ("مطلب", "matlab"),
];

fn is_devanagari(c: char) -> bool {
    ('\u{0900}'..='\u{097F}').contains(&c)
}

fn is_arabic(c: char) -> bool {
    ('\u{0600}'..='\u{06FF}').contains(&c) || ('\u{0750}'..='\u{077F}').contains(&c) || ('\u{FB50}'..='\u{FDFF}').contains(&c) || ('\u{FE70}'..='\u{FEFF}').contains(&c)
}

/// Is this text in a script that captions should show in Roman letters (Hindi, Urdu)?
pub fn needs_roman(text: &str) -> bool {
    text.chars().any(|c| is_devanagari(c) || is_arabic(c))
}

fn vowel(c: char) -> Option<&'static str> {
    Some(match c {
        'अ' => "a", 'आ' => "aa", 'इ' => "i", 'ई' => "ee", 'उ' => "u", 'ऊ' => "oo", 'ऋ' => "ri", 'ए' => "e", 'ऐ' => "ai", 'ओ' => "o", 'औ' => "au", 'ऑ' => "o", 'ऍ' => "e",
        _ => return None,
    })
}

fn matra(c: char) -> Option<&'static str> {
    Some(match c {
        'ा' => "a", 'ि' => "i", 'ी' => "ee", 'ु' => "u", 'ू' => "oo", 'ृ' => "ri", 'े' => "e", 'ै' => "ai", 'ो' => "o", 'ौ' => "au", 'ॉ' => "o", 'ॅ' => "e",
        _ => return None,
    })
}

fn consonant(c: char) -> Option<&'static str> {
    Some(match c {
        'क' => "k", 'ख' => "kh", 'ग' => "g", 'घ' => "gh", 'ङ' => "n", 'च' => "ch", 'छ' => "chh", 'ज' => "j", 'झ' => "jh", 'ञ' => "n", 'ट' => "t", 'ठ' => "th",
        'ड' => "d", 'ढ' => "dh", 'ण' => "n", 'त' => "t", 'थ' => "th", 'द' => "d", 'ध' => "dh", 'न' => "n", 'प' => "p", 'फ' => "ph", 'ब' => "b", 'भ' => "bh",
        'म' => "m", 'य' => "y", 'र' => "r", 'ल' => "l", 'व' => "v", 'श' => "sh", 'ष' => "sh", 'स' => "s", 'ह' => "h", 'ळ' => "l",
        _ => return None,
    })
}

enum Tail {
    Matra(&'static str),
    Virama,
    Inherent,
}

struct Unit {
    cons: String,
    tail: Tail,
}

/// One Devanagari word (no spaces) in Roman letters.
fn hindi_word(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    let mut units: Vec<Unit> = Vec::new();
    let mut out_pre = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(v) = vowel(c) {
            units.push(Unit { cons: v.to_string(), tail: Tail::Virama });
            i += 1;
            continue;
        }
        let nukta_next = chars.get(i + 1) == Some(&'़');
        let base = if nukta_next {
            match c { 'क' => Some("q"), 'ख' => Some("kh"), 'ग' => Some("gh"), 'ज' => Some("z"), 'फ' => Some("f"), 'ड' => Some("r"), 'ढ' => Some("rh"), _ => consonant(c) }
        } else {
            consonant(c)
        };
        if let Some(b) = base {
            i += if nukta_next { 2 } else { 1 };
            let tail = match chars.get(i) {
                Some(&n) if matra(n).is_some() => { i += 1; Tail::Matra(matra(n).unwrap()) }
                Some('्') => { i += 1; Tail::Virama }
                _ => Tail::Inherent,
            };
            units.push(Unit { cons: b.to_string(), tail });
            continue;
        }
        match c {
            'ं' | 'ँ' => attach(&mut units, &mut out_pre, "n"),
            'ः' => attach(&mut units, &mut out_pre, "h"),
            '।' | '॥' => out_pre.push('.'),
            '़' | '्' => {}
            d if ('०'..='९').contains(&d) => out_pre.push(char::from_digit(d as u32 - '०' as u32, 10).unwrap()),
            other => out_pre.push(other),
        }
        i += 1;
    }
    let n = units.len();
    let mut out = String::new();
    let mut prev_voiced = false;
    for (k, u) in units.iter().enumerate() {
        out.push_str(&u.cons);
        match &u.tail {
            Tail::Matra(m) => { out.push_str(m); prev_voiced = true; }
            Tail::Virama => { prev_voiced = matches!(u.tail, Tail::Virama) && vowel_unit(&u.cons); }
            Tail::Inherent => {
                let last = k + 1 == n;
                let next_has_vowel = units.get(k + 1).map(|x| matches!(x.tail, Tail::Matra(_)) || vowel_unit(&x.cons)).unwrap_or(false);
                // The hidden "a" is not said at the end of a word, or between two sounded syllables (la-r-ka -> larka).
                let drop = last || (k > 0 && prev_voiced && next_has_vowel);
                if !drop {
                    out.push('a');
                    prev_voiced = true;
                } else {
                    prev_voiced = false;
                }
            }
        }
    }
    out.push_str(&out_pre);
    out
}

fn vowel_unit(s: &str) -> bool {
    matches!(s, "a" | "aa" | "i" | "ee" | "u" | "oo" | "ri" | "e" | "ai" | "o" | "au")
}

/// A nasal sign (anusvara) is said after the sound before it.
fn attach(units: &mut Vec<Unit>, pre: &mut String, s: &str) {
    if units.is_empty() {
        pre.push_str(s);
    } else {
        units.push(Unit { cons: s.to_string(), tail: Tail::Virama });
    }
}

fn urdu_letter(c: char) -> Option<(&'static str, bool)> {
    // (roman, is a vowel letter)
    Some(match c {
        'ا' => ("a", true), 'آ' => ("aa", true), 'أ' => ("a", true), 'ع' => ("a", true), 'و' => ("o", true), 'ی' | 'ي' => ("i", true), 'ے' | 'ۓ' => ("e", true), 'ئ' => ("i", true),
        'ب' => ("b", false), 'پ' => ("p", false), 'ت' => ("t", false), 'ٹ' => ("t", false), 'ث' => ("s", false), 'ج' => ("j", false), 'چ' => ("ch", false),
        'ح' => ("h", false), 'خ' => ("kh", false), 'د' => ("d", false), 'ڈ' => ("d", false), 'ذ' => ("z", false), 'ر' => ("r", false), 'ڑ' => ("r", false),
        'ز' => ("z", false), 'ژ' => ("zh", false), 'س' => ("s", false), 'ش' => ("sh", false), 'ص' => ("s", false), 'ض' => ("z", false), 'ط' => ("t", false),
        'ظ' => ("z", false), 'غ' => ("gh", false), 'ف' => ("f", false), 'ق' => ("q", false), 'ک' | 'ك' => ("k", false), 'گ' => ("g", false), 'ل' => ("l", false),
        'م' => ("m", false), 'ن' | 'ں' => ("n", false), 'ہ' | 'ھ' | 'ه' | 'ۃ' => ("h", false), 'ء' => ("", false),
        _ => return None,
    })
}

fn urdu_word(word: &str) -> String {
    let mut out = String::new();
    let mut prev_consonant = false;
    for c in word.chars() {
        match urdu_letter(c) {
            Some((r, vowel)) => {
                // Urdu leaves most short vowels out: put an "a" between two consonants so it can be read.
                if !vowel && prev_consonant && !out.is_empty() && !matches!(r, "") {
                    let last = out.chars().last().unwrap_or('a');
                    if !"aeiou".contains(last) {
                        out.push('a');
                    }
                }
                out.push_str(r);
                prev_consonant = !vowel && !r.is_empty();
            }
            None => {
                if !('\u{064B}'..='\u{065F}').contains(&c) && c != '\u{0670}' {
                    out.push(c);
                }
                prev_consonant = false;
            }
        }
    }
    out
}

/// A word, or word with punctuation around it, in Roman letters. Latin text comes back unchanged.
pub fn to_roman(text: &str) -> String {
    if !needs_roman(text) {
        return text.to_string();
    }
    let mut out = String::new();
    let mut seg = String::new();
    let mut seg_is_non_latin = false;
    let flush = |seg: &mut String, non_latin: bool, out: &mut String| {
        if seg.is_empty() {
            return;
        }
        if non_latin {
            let dev = seg.chars().any(is_devanagari);
            let table = if dev { HINDI_WORDS } else { URDU_WORDS };
            match table.iter().find(|(k, _)| *k == seg.as_str()) {
                Some((_, v)) => out.push_str(v),
                None => out.push_str(&if dev { hindi_word(seg) } else { urdu_word(seg) }),
            }
        } else {
            out.push_str(seg);
        }
        seg.clear();
    };
    for c in text.chars() {
        let non = is_devanagari(c) || is_arabic(c);
        if non != seg_is_non_latin {
            flush(&mut seg, seg_is_non_latin, &mut out);
            seg_is_non_latin = non;
        }
        seg.push(c);
    }
    flush(&mut seg, seg_is_non_latin, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everyday_hindi_words_use_their_chat_spelling() {
        for (hi, ro) in [("क्या", "kya"), ("है", "hai"), ("नहीं", "nahi"), ("भाई", "bhai"), ("बहुत", "bahut"), ("मैं", "main")] {
            assert_eq!(to_roman(hi), ro);
        }
    }

    #[test]
    fn other_hindi_words_are_spelled_from_their_letters() {
        assert_eq!(to_roman("समझ"), "samajh");
        assert_eq!(to_roman("कमल"), "kamal");
        assert_eq!(to_roman("लड़का"), "larka");
        assert_eq!(to_roman("चलता"), "chalta");
    }

    #[test]
    fn punctuation_and_latin_text_survive() {
        assert_eq!(to_roman("क्या?"), "kya?");
        assert_eq!(to_roman("GTA,"), "GTA,");
        assert_eq!(to_roman("है।"), "hai.");
    }

    #[test]
    fn urdu_words_are_readable() {
        assert_eq!(to_roman("یار"), "yaar");
        assert_eq!(to_roman("نہیں"), "nahi");
        assert!(!needs_roman(&to_roman("سمجھ")));
        assert!(needs_roman("سمجھ") && needs_roman("समझ") && !needs_roman("hello"));
    }
}
