//! gkl:// 深链接:生成与解析。
//!
//! 形态:
//!   gkl://commit/<40-hex>
//!   gkl://file/<path>#L<line>
//!   gkl://blame/<path>#L<line>
//!   gkl://search/<query>
//!
//! 设计原则:纯字符串编解码,不碰仓库。解析与生成在同一处,
//! 保证往返一致;非法输入报错而不是静默纠偏。

/// 深链接指向的目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepLinkTarget {
    Commit { id: String },
    File { path: String, line: Option<u32> },
    Blame { path: String, line: Option<u32> },
    Search { query: String },
}

impl DeepLinkTarget {
    /// 序列化为 gkl:// URL。路径做百分号编码,行号 #L<line> 片段。
    pub fn to_link(&self) -> String {
        match self {
            DeepLinkTarget::Commit { id } => format!("gkl://commit/{}", id),
            DeepLinkTarget::File { path, line } => {
                format!("gkl://file/{}{}", encode_path(path), frag(line))
            }
            DeepLinkTarget::Blame { path, line } => {
                format!("gkl://blame/{}{}", encode_path(path), frag(line))
            }
            DeepLinkTarget::Search { query } => {
                format!("gkl://search/{}", encode_path(query))
            }
        }
    }
}

fn frag(line: &Option<u32>) -> String {
    match line {
        Some(l) => format!("#L{}", l),
        None => String::new(),
    }
}

/// 只编码 URL 路径段里必须编码的字符,保持可读性。
fn encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 解析错误。
#[derive(Debug, thiserror::Error)]
#[error("非法深链接: {0}")]
pub struct DeepLinkError(String);

/// 解析 gkl:// 链接。
pub fn parse(input: &str) -> Result<DeepLinkTarget, DeepLinkError> {
    let rest = input
        .strip_prefix("gkl://")
        .ok_or_else(|| DeepLinkError(format!("必须以 gkl:// 开头: {}", input)))?;
    let (path, frag) = match rest.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (rest, None),
    };
    let line = match frag {
        Some(f) => Some(
            f.strip_prefix("L")
                .and_then(|n| n.parse::<u32>().ok())
                .ok_or_else(|| DeepLinkError(format!("片段必须是 #L<数字>: #{}", f)))?,
        ),
        None => None,
    };
    let (kind, arg) = path
        .split_once('/')
        .ok_or_else(|| DeepLinkError(format!("缺少分类段: {}", path)))?;
    match kind {
        "commit" => {
            if arg.len() != 40 || !arg.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(DeepLinkError(format!(
                    "commit id 必须是 40 位 hex: {}",
                    arg
                )));
            }
            Ok(DeepLinkTarget::Commit {
                id: arg.to_lowercase(),
            })
        }
        "file" => Ok(DeepLinkTarget::File {
            path: percent_decode(arg)?,
            line,
        }),
        "blame" => Ok(DeepLinkTarget::Blame {
            path: percent_decode(arg)?,
            line,
        }),
        "search" => Ok(DeepLinkTarget::Search {
            query: percent_decode(arg)?,
        }),
        other => Err(DeepLinkError(format!("未知分类: {}", other))),
    }
}

/// 简单百分号解码;%XX 不合法时报错。
fn percent_decode(s: &str) -> Result<String, DeepLinkError> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() + 1 {
                return Err(DeepLinkError("截断的百分号转义".into()));
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .map_err(|_| DeepLinkError("非法 UTF-8".into()))?;
            let v = u8::from_str_radix(hex, 16)
                .map_err(|_| DeepLinkError(format!("非法转义: %{}", hex)))?;
            out.push(v);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| DeepLinkError("路径不是合法 UTF-8".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let cases = vec![
            DeepLinkTarget::Commit { id: "a".repeat(40) },
            DeepLinkTarget::File {
                path: "src/主 文件.rs".into(),
                line: Some(42),
            },
            DeepLinkTarget::Blame {
                path: "a/b.rs".into(),
                line: None,
            },
            DeepLinkTarget::Search {
                query: "fix: 泄漏".into(),
            },
        ];
        for t in cases {
            let link = t.to_link();
            assert_eq!(parse(&link).unwrap(), t, "roundtrip failed: {}", link);
        }
    }

    #[test]
    fn rejects_bad() {
        assert!(parse("https://x.com").is_err());
        assert!(parse("gkl://commit/abc").is_err());
        assert!(parse("gkl://file/a#Lx").is_err());
    }
}
