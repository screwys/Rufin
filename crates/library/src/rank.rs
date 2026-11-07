//! Rank keys order playlist and queue entries. A key sorts by its bytes, and a key between two
//! neighbours always exists, so an insert or a move writes only the rows it places.
//!
//! A key is an integer part, then an optional fraction. The first byte of the integer part
//! gives its length: 0x80 and above count positive integers with 1, 2, ... digits, below 0x80
//! count negative ones. Appending increments the integer, so a long run of appends keeps keys a
//! few bytes long. Inserting between two keys with the same integer grows the fraction, which
//! never ends in a zero byte.

const ZERO: [u8; 2] = [0x80, 0x00];

fn integer(key: &[u8]) -> &[u8] {
    let head = key[0];
    let digits = if head >= 0x80 {
        usize::from(head - 0x7F)
    } else {
        usize::from(0x80 - head)
    };
    &key[..(1 + digits).min(key.len())]
}

fn increment(integer: &[u8]) -> Option<Vec<u8>> {
    let mut key = integer.to_vec();
    for digit in key[1..].iter_mut().rev() {
        if *digit < 0xFF {
            *digit += 1;
            return Some(key);
        }
        *digit = 0;
    }
    match key[0] {
        0x7F => Some(ZERO.to_vec()),
        0xFF => None,
        head => {
            key[0] = head + 1;
            if key[0] >= 0x80 {
                key.push(0);
            } else {
                key.pop();
            }
            Some(key)
        }
    }
}

fn decrement(integer: &[u8]) -> Option<Vec<u8>> {
    let mut key = integer.to_vec();
    for digit in key[1..].iter_mut().rev() {
        if *digit > 0 {
            *digit -= 1;
            return Some(key);
        }
        *digit = 0xFF;
    }
    match key[0] {
        0x80 => Some(vec![0x7F, 0xFF]),
        0x00 => None,
        head => {
            key[0] = head - 1;
            if key[0] < 0x80 {
                key.push(0xFF);
            } else {
                key.pop();
            }
            Some(key)
        }
    }
}

/// A fraction between `low` and `high` (the end when None). Neither ends in a zero byte.
fn midpoint(low: &[u8], high: Option<&[u8]>) -> Vec<u8> {
    if let Some(high) = high {
        let shared = high
            .iter()
            .enumerate()
            .take_while(|(index, digit)| low.get(*index).copied().unwrap_or(0) == **digit)
            .count();
        if shared > 0 {
            let mut key = high[..shared].to_vec();
            key.extend(midpoint(
                low.get(shared..).unwrap_or_default(),
                Some(&high[shared..]),
            ));
            return key;
        }
    }
    let low_digit = u16::from(low.first().copied().unwrap_or(0));
    let high_digit = high.map_or(0x100, |high| u16::from(high[0]));
    if high_digit - low_digit > 1 {
        return vec![((low_digit + high_digit) / 2) as u8];
    }
    if let Some(high) = high.filter(|high| high.len() > 1) {
        return vec![high[0]];
    }
    let mut key = vec![low_digit as u8];
    key.extend(midpoint(low.get(1..).unwrap_or_default(), None));
    key
}

/// A key after `low` and before `high`; None is the start or the end of the list. `low` must
/// sort before `high`.
pub fn between(low: Option<&[u8]>, high: Option<&[u8]>) -> Vec<u8> {
    match (low, high) {
        (None, None) => ZERO.to_vec(),
        (None, Some(high)) => {
            let whole = integer(high);
            if whole.len() < high.len() {
                return whole.to_vec();
            }
            decrement(whole).unwrap_or_else(|| {
                let mut key = whole.to_vec();
                key.extend(midpoint(&[], Some(&high[whole.len()..])));
                key
            })
        }
        (Some(low), None) => {
            let whole = integer(low);
            increment(whole).unwrap_or_else(|| {
                let mut key = whole.to_vec();
                key.extend(midpoint(&low[whole.len()..], None));
                key
            })
        }
        (Some(low), Some(high)) => {
            let (low_whole, high_whole) = (integer(low), integer(high));
            let mut key = low_whole.to_vec();
            if low_whole == high_whole {
                key.extend(midpoint(
                    &low[low_whole.len()..],
                    Some(&high[high_whole.len()..]),
                ));
                return key;
            }
            if let Some(next) = increment(low_whole).filter(|next| next.as_slice() < high) {
                return next;
            }
            key.extend(midpoint(&low[low_whole.len()..], None));
            key
        }
    }
}

/// `count` keys in order between `low` and `high`, spread so later inserts stay short.
pub fn spread(low: Option<&[u8]>, high: Option<&[u8]>, count: usize) -> Vec<Vec<u8>> {
    let mut keys = Vec::with_capacity(count);
    match (low, high) {
        _ if count == 0 => {}
        (_, None) => {
            let mut last = low.map(<[u8]>::to_vec);
            for _ in 0..count {
                let key = between(last.as_deref(), None);
                last = Some(key.clone());
                keys.push(key);
            }
        }
        (None, Some(_)) => {
            let mut first = high.map(<[u8]>::to_vec);
            for _ in 0..count {
                let key = between(None, first.as_deref());
                first = Some(key.clone());
                keys.push(key);
            }
            keys.reverse();
        }
        (Some(_), Some(_)) => {
            let middle = between(low, high);
            let before = count / 2;
            keys.extend(spread(low, Some(&middle), before));
            keys.push(middle.clone());
            keys.extend(spread(Some(&middle), high, count - before - 1));
        }
    }
    keys
}

/// `key` followed by random bytes, still before `high`. Devices that place playlist entries in
/// the same gap while apart then give them different keys, so every device orders them alike.
pub(crate) fn distinct(mut key: Vec<u8>, high: Option<&[u8]>) -> Vec<u8> {
    // A key that `high` starts with continues with a fraction below the rest of `high`.
    let mut rest = high
        .and_then(|high| high.strip_prefix(key.as_slice()))
        .map(<[u8]>::to_vec);
    while let Some(remaining) = rest {
        let fraction = midpoint(&[], Some(&remaining));
        rest = remaining
            .strip_prefix(fraction.as_slice())
            .map(<[u8]>::to_vec);
        key.extend(fraction);
    }
    let random =
        std::hash::BuildHasher::hash_one(&std::collections::hash_map::RandomState::new(), ());
    key.extend_from_slice(&(random as u32 | 1).to_be_bytes());
    key
}

/// The key at `index` in a list built by appending from the empty list.
pub(crate) fn nth(index: u64) -> Vec<u8> {
    let (mut start, mut width) = (0_u64, 1_u32);
    while width < 8 && index - start >= 1 << (8 * width) {
        start += 1 << (8 * width);
        width += 1;
    }
    let mut key = vec![0x7F + width as u8];
    key.extend_from_slice(&(index - start).to_be_bytes()[8 - width as usize..]);
    key
}

/// A key as hex, for SQL that reads keys out of JSON with unhex().
pub(crate) fn hex(key: &[u8]) -> String {
    key.iter().map(|byte| format!("{byte:02X}")).collect()
}

/// SQL for `nth` of a 0-based integer expression, for lists of up to 16.8 million entries.
pub(crate) fn nth_sql(index: &str) -> String {
    format!(
        "CASE WHEN ({index})<256 THEN unhex(printf('80%02X',{index})) WHEN ({index})<65792 THEN unhex(printf('81%04X',({index})-256)) ELSE unhex(printf('82%06X',({index})-65792)) END"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_order_and_stay_short() {
        let appended = spread(None, None, 70_000);
        assert!(appended.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(appended.iter().all(|key| key.len() <= 4));
        for (index, key) in appended.iter().enumerate().step_by(997) {
            assert_eq!(&nth(index as u64), key);
        }
        let mut low = appended[10].clone();
        let high = appended[11].clone();
        for _ in 0..500 {
            let key = between(Some(&low), Some(&high));
            assert!(low < key && key < high);
            low = key;
        }
        let mut high = appended[0].clone();
        for _ in 0..300 {
            let key = between(None, Some(&high));
            assert!(key < high);
            high = key;
        }
        let inserted = spread(Some(&appended[3]), Some(&appended[4]), 1000);
        assert!(inserted.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(appended[3] < inserted[0] && inserted[999] < appended[4]);
        for (low, high) in [(&appended[3], &inserted[0]), (&inserted[0], &inserted[1])] {
            let middle = between(Some(low), Some(high));
            let (first, second) = (
                distinct(middle.clone(), Some(high)),
                distinct(middle, Some(high)),
            );
            assert!(first != second && *low < first && first < *high && second < *high);
        }
        let high = between(Some(&appended[5]), Some(&appended[6]));
        let key = distinct(appended[5].clone(), Some(&high));
        assert!(high.starts_with(&appended[5]) && appended[5] < key && key < high);
    }
}
