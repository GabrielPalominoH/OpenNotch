//! Spanish text -> IPA (the symbols Kokoro was trained on), rule based. Latin-American pronunciation (seseo, yeismo).
//! espeak-ng would do this too, but it is a C library to ship; Spanish spelling is regular enough for rules.

const WEAK: &str = "iuü";
const STRONG: &str = "aeo";

fn is_vowel(c: char) -> bool {
    "aeiouáéíóúü".contains(c)
}

fn strip_accent(c: char) -> char {
    match c {
        'á' => 'a',
        'é' => 'e',
        'í' => 'i',
        'ó' => 'o',
        'ú' => 'u',
        _ => c,
    }
}

const UNITS: [&str; 30] = [
    "cero", "uno", "dos", "tres", "cuatro", "cinco", "seis", "siete", "ocho", "nueve", "diez", "once", "doce", "trece", "catorce", "quince",
    "dieciséis", "diecisiete", "dieciocho", "diecinueve", "veinte", "veintiuno", "veintidós", "veintitrés", "veinticuatro", "veinticinco",
    "veintiséis", "veintisiete", "veintiocho", "veintinueve",
];
const TENS: [&str; 10] = ["", "", "", "treinta", "cuarenta", "cincuenta", "sesenta", "setenta", "ochenta", "noventa"];
const HUNDREDS: [&str; 10] = ["", "ciento", "doscientos", "trescientos", "cuatrocientos", "quinientos", "seiscientos", "setecientos", "ochocientos", "novecientos"];

/// 0..=999_999_999 in words.
pub fn number_words(n: u64) -> String {
    match n {
        0..=29 => UNITS[n as usize].to_string(),
        30..=99 => {
            let (t, u) = ((n / 10) as usize, n % 10);
            if u == 0 { TENS[t].to_string() } else { format!("{} y {}", TENS[t], UNITS[u as usize]) }
        }
        100 => "cien".into(),
        101..=999 => {
            let rest = n % 100;
            let h = HUNDREDS[(n / 100) as usize];
            if rest == 0 { h.to_string() } else { format!("{h} {}", number_words(rest)) }
        }
        1000..=999_999 => {
            let (k, rest) = (n / 1000, n % 1000);
            let head = if k == 1 { "mil".to_string() } else { format!("{} mil", number_words(k).replace("uno", "un")) };
            if rest == 0 { head } else { format!("{head} {}", number_words(rest)) }
        }
        1_000_000..=999_999_999 => {
            let (m, rest) = (n / 1_000_000, n % 1_000_000);
            let head = if m == 1 { "un millón".to_string() } else { format!("{} millones", number_words(m).replace("uno", "un")) };
            if rest == 0 { head } else { format!("{head} {}", number_words(rest)) }
        }
        _ => n.to_string().chars().map(|d| UNITS[d.to_digit(10).unwrap() as usize]).collect::<Vec<_>>().join(" "),
    }
}

/// Digits and a few symbols to words, so the phonemizer only sees letters and punctuation.
pub fn normalize(text: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            // decimal "3,5" / "3.5" -> "tres coma cinco"
            let int: String = chars[i..j].iter().collect();
            out.push_str(&int.parse::<u64>().map(number_words).unwrap_or(int));
            if j + 1 < chars.len() && (chars[j] == ',' || chars[j] == '.') && chars[j + 1].is_ascii_digit() {
                let mut k = j + 1;
                while k < chars.len() && chars[k].is_ascii_digit() {
                    k += 1;
                }
                out.push_str(" coma ");
                let frac: String = chars[j + 1..k].iter().collect();
                out.push_str(&frac.chars().map(|d| UNITS[d.to_digit(10).unwrap() as usize]).collect::<Vec<_>>().join(" "));
                j = k;
            }
            i = j;
            continue;
        }
        match c {
            '%' => out.push_str(" por ciento"),
            '*' | '×' => out.push_str(" por "),
            '+' => out.push_str(" más "),
            '=' => out.push_str(" igual a "),
            '&' => out.push_str(" y "),
            '°' => out.push_str(" grados"),
            '€' => out.push_str(" euros"),
            '$' => out.push_str(" dólares"),
            '/' => out.push(' '),
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

const CLITICS: [&str; 24] = [
    "el", "la", "los", "las", "de", "en", "y", "a", "que", "se", "me", "te", "lo", "le", "un", "su", "mi", "tu", "con", "por", "al", "del", "no", "o",
];

/// One word (letters only, lowercase) -> IPA with a stress mark.
fn word_ipa(w: &str) -> String {
    let cs: Vec<char> = w.chars().collect();
    let n = cs.len();
    // vowel nuclei: a run of vowels is one syllable unless it is a hiatus (strong+strong, or an accented weak vowel)
    let mut nuclei: Vec<usize> = Vec::new(); // index of the nucleus vowel
    let mut i = 0;
    while i < n {
        if !is_vowel(cs[i]) {
            i += 1;
            continue;
        }
        let mut j = i;
        let mut main = i;
        while j + 1 < n && is_vowel(cs[j + 1]) {
            let (a, b) = (cs[j], cs[j + 1]);
            let hiatus = ("íú".contains(a) || "íú".contains(b)) || (STRONG.contains(strip_accent(a)) && STRONG.contains(strip_accent(b)));
            if hiatus {
                nuclei.push(main);
                main = j + 1;
            } else if STRONG.contains(strip_accent(b)) || (WEAK.contains(strip_accent(a)) && !STRONG.contains(strip_accent(a))) && !STRONG.contains(strip_accent(b)) {
                main = j + 1;
            }
            j += 1;
        }
        nuclei.push(main);
        i = j + 1;
    }
    // "qu"/"gu" + e/i: the u is silent and does not form a nucleus
    nuclei.retain(|&k| !(cs[k] == 'u' && k > 0 && (cs[k - 1] == 'q' || (cs[k - 1] == 'g' && k + 1 < n && "eiéí".contains(cs[k + 1])))));
    // stress position
    let accented = nuclei.iter().position(|&k| "áéíóú".contains(cs[k]));
    let last = cs.last().copied().unwrap_or(' ');
    let stressed = match accented {
        Some(p) => Some(p),
        None => match nuclei.len() {
            0 => None,
            1 => (!CLITICS.contains(&w)).then_some(0),
            len => Some(if "aeiouns".contains(last) { len - 2 } else { len - 1 }),
        },
    };
    let stress_at = stressed.map(|p| nuclei[p]);

    let at = |k: isize| -> char { if k < 0 || k as usize >= n { ' ' } else { cs[k as usize] } };
    let mut out = String::new();
    let mut k = 0usize;
    while k < n {
        let c = cs[k];
        let prev = if at(k as isize - 1) == 'h' { at(k as isize - 2) } else { at(k as isize - 1) };
        let next = at(k as isize + 1);
        let after_vowel = is_vowel(prev);
        let before_vowel = is_vowel(next);
        let mut skip = 0;
        if Some(k) == stress_at {
            out.push('ˈ');
        }
        match c {
            'a' | 'á' => out.push('a'),
            'e' | 'é' => out.push('e'),
            'o' | 'ó' => out.push('o'),
            'í' => out.push('i'),
            'ú' => out.push('u'),
            'i' => {
                if Some(k) == stress_at || nuclei.contains(&k) {
                    out.push('i')
                } else if before_vowel && !after_vowel {
                    out.push('j')
                } else {
                    out.push('ɪ')
                }
            }
            'u' => {
                if Some(k) == stress_at || nuclei.contains(&k) {
                    out.push('u')
                } else if before_vowel && !after_vowel {
                    out.push('w')
                } else {
                    out.push('ʊ')
                }
            }
            'ü' => out.push('w'),
            'b' | 'v' => out.push(if after_vowel && (before_vowel || next == 'l' || next == 'r') { 'β' } else { 'b' }),
            'c' => {
                if next == 'h' {
                    out.push('ʧ');
                    skip = 1;
                } else if "eiéí".contains(next) {
                    out.push('s')
                } else {
                    out.push('k')
                }
            }
            'd' => out.push(if after_vowel && (before_vowel || next == 'r') && k + 1 < n { 'ð' } else if k + 1 == n && after_vowel { 'ð' } else { 'd' }),
            'f' => out.push('f'),
            'g' => {
                if "eiéí".contains(next) {
                    out.push('x')
                } else if next == 'u' && "eiéí".contains(at(k as isize + 2)) {
                    out.push(if after_vowel { 'ɣ' } else { 'ɡ' });
                    skip = 1;
                } else {
                    out.push(if after_vowel && (before_vowel || next == 'r' || next == 'l') { 'ɣ' } else { 'ɡ' })
                }
            }
            'h' => {}
            'j' => out.push('x'),
            'k' => out.push('k'),
            'l' => {
                if next == 'l' {
                    out.push('ʝ');
                    skip = 1;
                } else {
                    out.push('l')
                }
            }
            'm' => out.push('m'),
            'n' => out.push(if "kgjqx".contains(next) { 'ŋ' } else if "bpvm".contains(next) { 'm' } else { 'n' }),
            'ñ' => out.push('ɲ'),
            'p' => out.push('p'),
            'q' => {
                out.push('k');
                if next == 'u' {
                    skip = 1;
                }
            }
            'r' => {
                if next == 'r' {
                    out.push('r');
                    skip = 1;
                } else if k == 0 || "nls".contains(prev) {
                    out.push('r')
                } else {
                    out.push('ɾ')
                }
            }
            's' => out.push('s'),
            't' => out.push('t'),
            'w' => out.push('w'),
            'x' => out.push_str(if k == 0 { "s" } else { "ks" }),
            'y' => {
                if k + 1 == n || !before_vowel {
                    out.push(if Some(k) == stress_at { 'i' } else { 'i' })
                } else {
                    out.push('ʝ')
                }
            }
            'z' => out.push('s'),
            _ => {}
        }
        k += 1 + skip;
    }
    out
}

/// Whole text -> one IPA string (words separated by spaces, punctuation kept for pauses).
pub fn phonemize(text: &str) -> String {
    let text = normalize(&text.to_lowercase());
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if !word.is_empty() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            out.push_str(&word_ipa(word));
            word.clear();
        }
    };
    for c in text.chars() {
        if c.is_alphabetic() || c == '\'' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            match c {
                ',' | '.' | '!' | '?' | ';' | ':' | '…' | '—' | '"' | '(' | ')' => out.push(c),
                '¿' | '¡' => {}
                _ if c.is_whitespace() => {
                    if !out.is_empty() && !out.ends_with(' ') {
                        out.push(' ')
                    }
                }
                _ => {}
            }
        }
    }
    flush(&mut word, &mut out);
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words() {
        assert_eq!(word_ipa("hola"), "ˈola");
        assert_eq!(word_ipa("casa"), "kˈasa");
        assert_eq!(word_ipa("música"), "mˈusika");
        assert_eq!(word_ipa("tiene"), "tjˈene");
        assert_eq!(word_ipa("bueno"), "bwˈeno");
    }

    #[test]
    fn spelling_rules() {
        assert_eq!(word_ipa("perro"), "pˈero");
        assert_eq!(word_ipa("pero"), "pˈeɾo");
        assert_eq!(word_ipa("queso"), "kˈeso");
        assert_eq!(word_ipa("gente"), "xˈente");
        assert_eq!(word_ipa("guerra"), "ɡˈera");
        assert_eq!(word_ipa("niño"), "nˈiɲo");
        assert_eq!(word_ipa("calle"), "kˈaʝe");
        assert_eq!(word_ipa("ciento"), "sjˈento");
        assert_eq!(word_ipa("pantalla"), "pantˈaʝa");
        assert_eq!(word_ipa("recordatorios"), "rekoɾdatˈoɾjos");
        assert_eq!(word_ipa("aire"), "ˈaɪɾe");
        assert_eq!(word_ipa("país"), "paˈis");
        assert_eq!(word_ipa("hablar"), "aβlˈaɾ");
    }

    #[test]
    fn numbers() {
        assert_eq!(number_words(15), "quince");
        assert_eq!(number_words(180), "ciento ochenta");
        assert_eq!(number_words(31), "treinta y uno");
        assert_eq!(number_words(2026), "dos mil veintiséis");
        assert_eq!(number_words(21_000), "veintiun mil");
        assert_eq!(normalize("15 * 12"), "quince  por  doce");
    }

    #[test]
    fn sentence() {
        assert_eq!(phonemize("¿Qué hora es?"), "kˈe ˈoɾa ˈes?");
    }
}
