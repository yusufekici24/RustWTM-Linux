//! Text order as people expect it in a list: "file2" before "file10",
//! "Édge" next to "edge", capitals mixed in with small letters. Case and
//! the usual Latin accents (Turkish included) are folded; runs of digits
//! compare by value. Ties fall back to a plain comparison so the order is
//! total and repeatable.

use std::cmp::Ordering;

fn fold(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => 'a',
        'ç' | 'Ç' | 'ć' | 'č' => 'c',
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
        'ğ' | 'Ğ' => 'g',
        'í' | 'ì' | 'î' | 'ï' | 'ı' | 'İ' | 'Í' | 'Ì' | 'Î' | 'Ï' => 'i',
        'ñ' | 'Ñ' => 'n',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' | 'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' | 'Ø' => 'o',
        'ş' | 'Ş' | 'š' | 'ß' => 's',
        'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => 'u',
        'ý' | 'ÿ' | 'Ý' => 'y',
        'ž' | 'Ž' => 'z',
        c => c.to_lowercase().next().unwrap_or(c),
    }
}

pub fn natural(a: &str, b: &str) -> Ordering {
    let mut x = a.chars().peekable();
    let mut y = b.chars().peekable();
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return lower(a, b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(p), Some(q)) if p.is_ascii_digit() && q.is_ascii_digit() => {
                let mut n = String::new();
                while let Some(c) = x.peek().copied().filter(char::is_ascii_digit) {
                    n.push(c);
                    x.next();
                }
                let mut m = String::new();
                while let Some(c) = y.peek().copied().filter(char::is_ascii_digit) {
                    m.push(c);
                    y.next();
                }
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let order = n.len().cmp(&m.len()).then_with(|| n.cmp(m));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(p), Some(q)) => {
                let order = fold(p).cmp(&fold(q));
                if order != Ordering::Equal {
                    return order;
                }
                x.next();
                y.next();
            }
        }
    }
}

fn lower(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_like_people() {
        let mut v = vec!["file10", "File2", "édge", "Edge", "file1", "Çay", "cam"];
        v.sort_by(|a, b| natural(a, b));
        assert_eq!(
            v,
            ["cam", "Çay", "Edge", "édge", "file1", "File2", "file10"]
        );
        assert_eq!(natural("a", "a"), Ordering::Equal);
    }
}
