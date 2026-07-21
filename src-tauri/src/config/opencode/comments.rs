// ===================== JSONC 注释剥离 =====================

/// 剥离 JSONC 注释（`//` 行注释与 `/* */` 块注释），尊重字符串字面量。
/// 按 Unicode code point 处理（不会切坏中文 / emoji）。
/// 返回 `(无注释 JSON 文本, 是否曾出现注释)`。
pub fn strip_comments(input: &str) -> (String, bool) {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut had_comment = false;
    let mut i = 0;
    let n = chars.len();
    while i < n {
        let c = chars[i];
        // 字符串字面量：原样拷贝到闭合的未转义 "
        if c == '"' {
            out.push('"');
            i += 1;
            while i < n {
                let ch = chars[i];
                out.push(ch);
                if ch == '\\' && i + 1 < n {
                    i += 1;
                    out.push(chars[i]);
                } else if ch == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // 行注释 //
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            had_comment = true;
            i += 2;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            // 保留换行符
            if i < n && chars[i] == '\n' {
                out.push('\n');
                i += 1;
            }
            continue;
        }
        // 块注释 /* */
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            had_comment = true;
            i += 2;
            while i + 1 < n && !(chars[i] == '*' && chars[i + 1] == '/') {
                // 在块注释中保留换行
                if chars[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            if i + 1 < n {
                i += 2; // 跳过 */
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    (out, had_comment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_line_comment() {
        let (out, had) = strip_comments("{\"a\":1 // comment\n}");
        assert!(had);
        assert_eq!(out, "{\"a\":1 \n}");
    }

    #[test]
    fn strip_block_comment() {
        let (out, had) = strip_comments("{\"a\":1 /* block */, \"b\":2}");
        assert!(had);
        assert_eq!(out, "{\"a\":1 , \"b\":2}");
    }

    #[test]
    fn string_comment_like_preserved() {
        let (out, _) = strip_comments("{\"url\":\"http://example.com\"}");
        assert_eq!(out, "{\"url\":\"http://example.com\"}");
    }

    #[test]
    fn string_with_escaped_quotes() {
        let (out, _) = strip_comments("{\"msg\":\"he said \\\"hello\\\"\"}");
        assert_eq!(out, "{\"msg\":\"he said \\\"hello\\\"\"}");
    }

    #[test]
    fn no_comment() {
        let (out, had) = strip_comments("{\"a\":1}");
        assert!(!had);
        assert_eq!(out, "{\"a\":1}");
    }
}
