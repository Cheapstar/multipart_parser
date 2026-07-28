use std::collections::HashMap;

pub fn find_double_newline(chunk: &[u8], index: usize) -> Option<usize> {
    chunk[index..]
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|pos| index + pos)
}

pub fn substring_search(haystack: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    let needle_end: u8 = needle.len() as u8 - 1;
    let mut skip_table = vec![needle.len(); 256];

    for i in 0..needle_end {
        skip_table[needle[i as usize] as usize] = (needle_end - i) as usize;
    }

    let haystack_length = haystack.len();
    let mut i = start + needle_end as usize;

    while (i as usize) < haystack_length {
        let mut j = needle_end;
        let mut k = i;

        while (j >= 0) && haystack[k as usize] == needle[j as usize] {
            if j == 0 {
                return Some(k as usize);
            }
            j -= 1;
            k -= 1;
        }

        i += skip_table[haystack[i as usize] as usize] as usize;
    }

    return None;
}

pub fn substring_partial_search(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if haystack.is_empty() || needle.is_empty() {
        return None;
    }

    let mut byte_indexes = HashMap::<u8, Vec<usize>>::new();
    for (i, &byte) in needle.iter().enumerate() {
        byte_indexes.entry(byte).or_insert_with(Vec::new).push(i);
    }

    let haystack_end = haystack.len() - 1;

    if let Some(indexes) = byte_indexes.get(&haystack[haystack_end]) {
        for &i in indexes.iter().rev() {
            let mut j = i;
            let mut k = haystack_end;

            loop {
                if haystack[k] != needle[j] {
                    break;
                }
                if j == 0 {
                    return Some(k);
                }
                j -= 1;
                k -= 1;
            }
        }
    }

    None
}
