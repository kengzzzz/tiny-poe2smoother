use anyhow::{bail, Result};
use regex::Regex;
use std::collections::HashSet;

pub(super) fn replace_utf16(bytes: &[u8], replacements: &[(&str, &str)]) -> Result<Vec<u8>> {
    let mut text = decode_utf16(bytes)?;
    for (from, to) in replacements {
        text = text.replace(from, to);
    }
    Ok(encode_utf16_bom(&text))
}

pub(super) fn regex_utf16(bytes: &[u8], regex: &Regex, replacement: &str) -> Result<Vec<u8>> {
    let text = decode_utf16(bytes)?;
    Ok(encode_utf16_bom(&regex.replace_all(&text, replacement)))
}

pub(crate) fn decode_utf16(bytes: &[u8]) -> Result<String> {
    if !bytes.len().is_multiple_of(2) {
        bail!("UTF-16 file has odd byte length");
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    let text = String::from_utf16(&units)?;
    Ok(text.trim_start_matches('\u{feff}').to_string())
}

pub(super) fn decode_utf16_lossless(bytes: &[u8]) -> Result<Option<String>> {
    if !bytes.len().is_multiple_of(2) {
        return Ok(None);
    }
    match decode_utf16(bytes) {
        Ok(text) => Ok(Some(text)),
        Err(_) => Ok(None),
    }
}

pub(super) fn encode_utf16_bom(text: &str) -> Vec<u8> {
    let mut out = vec![0xff, 0xfe];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

pub(super) fn replace_array_property(mut data: String, property: &str) -> String {
    let pattern = format!("\"{property}\":");
    let Some(index) = data.find(&pattern) else {
        return data;
    };
    let Some(bracket_start_rel) = data[index..].find('[') else {
        return data;
    };
    let bracket_start = index + bracket_start_rel;
    let mut depth = 1;
    let mut end = bracket_start + 1;
    let chars: Vec<char> = data.chars().collect();
    while end < chars.len() && depth > 0 {
        match chars[end] {
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
        end += 1;
    }
    if depth == 0 {
        if let Some(comma_rel) = data[end - 1..].find(',') {
            let comma = end - 1 + comma_rel;
            if comma < end + 5 {
                data.replace_range(index..=comma, &format!("\"{property}\": [],"));
            }
        }
    }
    data
}

pub(super) fn remove_function_calls(data: &str, func: &str) -> String {
    let mut data = data.to_string();
    let mut pos = 0;
    while let Some(found) = data[pos..].find(func) {
        let found = pos + found;
        let mut start = found;
        while start > 0 {
            let ch = data.as_bytes()[start - 1] as char;
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
                start -= 1;
            } else {
                break;
            }
        }
        let mut paren = found + func.len();
        while paren < data.len() && data.as_bytes()[paren].is_ascii_whitespace() {
            paren += 1;
        }
        if paren >= data.len() || data.as_bytes()[paren] != b'(' {
            pos = found + 1;
            continue;
        }
        let mut depth = 1;
        let mut end = paren + 1;
        while end < data.len() && depth > 0 {
            match data.as_bytes()[end] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            end += 1;
        }
        while end < data.len() && data.as_bytes()[end].is_ascii_whitespace() {
            end += 1;
        }
        if depth == 0 && end < data.len() && data.as_bytes()[end] == b';' {
            data.replace_range(start..end + 1, "");
            pos = start;
        } else {
            pos = found + 1;
        }
    }
    data
}

fn skip_syntax(text: &str, i: usize) -> usize {
    let bytes = text.as_bytes();
    match bytes[i] {
        b'"' => {
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'\\' && j + 1 < bytes.len() {
                    j += 2;
                    continue;
                }
                if bytes[j] == b'"' {
                    return j + 1;
                }
                j += 1;
            }
            bytes.len()
        }
        b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => text[i + 2..]
            .find('\n')
            .map_or(bytes.len(), |p| i + 2 + p + 1),
        b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => text[i + 2..]
            .find("*/")
            .map_or(bytes.len(), |p| i + 2 + p + 2),
        b'[' => {
            let mut depth = 1;
            let mut j = i + 1;
            while j < bytes.len() && depth > 0 {
                if bytes[j] == b'"'
                    || (bytes[j] == b'/'
                        && j + 1 < bytes.len()
                        && (bytes[j + 1] == b'/' || bytes[j + 1] == b'*'))
                {
                    j = skip_syntax(text, j);
                    continue;
                }
                if bytes[j] == b'[' {
                    depth += 1;
                } else if bytes[j] == b']' {
                    depth -= 1;
                }
                j += 1;
            }
            j
        }
        _ => i + 1,
    }
}

fn find_matching_brace(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 1;
    let mut i = open + 1;
    while i < bytes.len() {
        if bytes[i] == b'"'
            || bytes[i] == b'['
            || (bytes[i] == b'/'
                && i + 1 < bytes.len()
                && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*'))
        {
            i = skip_syntax(text, i);
            continue;
        }
        if bytes[i] == b'{' {
            depth += 1;
        } else if bytes[i] == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn find_next_sub_block(text: &str, from: usize) -> Option<(usize, String, usize)> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'"'
            || bytes[i] == b'['
            || (bytes[i] == b'/'
                && i + 1 < bytes.len()
                && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*'))
        {
            i = skip_syntax(text, i);
            continue;
        }
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let name_end = i;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'{' {
                return Some((start, text[start..name_end].to_string(), i));
            }
            continue;
        }
        i += 1;
    }
    None
}

fn find_top_level_block(text: &str, name: &str) -> Option<usize> {
    let mut pos = 0;
    while let Some((_, block_name, open)) = find_next_sub_block(text, pos) {
        if block_name == name {
            return Some(open);
        }
        let close = find_matching_brace(text, open)?;
        pos = close + 1;
    }
    None
}

pub(super) fn strip_client_blocks(data: &str, keep: &HashSet<&str>) -> String {
    let Some(client_open) = find_top_level_block(data, "client") else {
        return data.to_string();
    };
    let Some(client_close) = find_matching_brace(data, client_open) else {
        return data.to_string();
    };
    let body = &data[client_open + 1..client_close];
    let mut result = String::new();
    let mut pos = 0;
    while let Some((name_start, name, open)) = find_next_sub_block(body, pos) {
        let Some(close) = find_matching_brace(body, open) else {
            result.push_str(&body[pos..]);
            break;
        };
        result.push_str(&body[pos..name_start]);
        if keep.contains(name.as_str()) {
            result.push_str(&body[name_start..close + 1]);
        }
        pos = close + 1;
    }
    result.push_str(&body[pos..]);
    format!(
        "{}{}{}",
        &data[..client_open + 1],
        result,
        &data[client_close..]
    )
}

/// Replace the body of every block whose name is in `names` with `{}`, in place,
/// at any nesting depth, leaving all surrounding bytes untouched. Used for sound
/// removal: `SoundEvents { ... }` -> `SoundEvents {}`.
pub(super) fn empty_named_blocks(data: &str, names: &HashSet<&str>) -> String {
    let mut out = data.to_string();
    let mut pos = 0;
    while let Some((name_start, name, open)) = find_next_sub_block(&out, pos) {
        let Some(close) = find_matching_brace(&out, open) else {
            break;
        };
        // Only collapse blocks that actually carry content; leave already-empty
        // (whitespace-only) bodies verbatim, matching the captured output.
        if names.contains(name.as_str()) && !out[open + 1..close].trim().is_empty() {
            let replacement = format!("{name} {{}}");
            let advance = name_start + replacement.len();
            out.replace_range(name_start..close + 1, &replacement);
            // Resume past the emptied block so we never re-match it (infinite loop).
            pos = advance;
        } else {
            pos = open + 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_keeps_selected_client_blocks() {
        let input =
            "version 3\nclient\n{\n  AnimatedRender\n  {\n  }\n  ParticleEffects\n  {\n  }\n}";
        let keep = ["AnimatedRender"].into_iter().collect();
        let out = strip_client_blocks(input, &keep);
        assert!(out.contains("AnimatedRender"));
        assert!(!out.contains("ParticleEffects"));
    }
}
