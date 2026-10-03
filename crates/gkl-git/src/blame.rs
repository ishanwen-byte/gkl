//! 轻量 blame:回答"这一行最后一次被谁改的"。
//!
//! 不是完整 git blame(不做跨 rename 追溯、不处理合并启发式),
//! 而是历史二分 + 行内容匹配,足够回答考古问题:
//! "HEAD 上文件 F 的第 N 行,最早出现在哪个提交"。

use crate::repo::Repo;

/// 一行的 blame 结果。
#[derive(Debug, Clone)]
pub struct LineBlame {
    /// 命中的提交 id。
    pub commit: String,
    /// 该行在命中提交时的行号(1-based)。
    pub line_no: usize,
}

/// 找 HEAD 文件第 line_no(1-based)行最后修改它的提交。
///
/// 沿第一父链回溯:行内容相同则继续向前,内容变化(或文件消失)的前一个提交即答案。
/// 简单可靠,大仓库下行数少时性能足够;后续可换 imit::blame 优化。
pub fn blame_line(repo: &Repo, path: &str, line_no: usize) -> anyhow::Result<Option<LineBlame>> {
    let head = repo.head_id()?;
    let head_blob = repo.blob_at(&head, path)?;
    let Some(blob) = head_blob else {
        anyhow::bail!("HEAD 上不存在文件: {}", path);
    };
    let lines: Vec<&[u8]> = split_lines(&blob);
    if line_no == 0 || line_no > lines.len() {
        anyhow::bail!("行号越界: 文件共 {} 行,请求 {}", lines.len(), line_no);
    }
    let target = lines[line_no - 1].to_vec();

    let mut cur = repo.commit_meta(&head)?;
    loop {
        let Some(parent) = cur.parents.first().cloned() else {
            // 到根提交:该行自仓库初始就存在。
            return Ok(Some(LineBlame { commit: cur.id.clone(), line_no }));
        };
        let parent_blob = repo.blob_at(&parent, path)?;
        match parent_blob {
            Some(pb) => {
                let plines = split_lines(&pb);
                if plines.get(line_no - 1) == Some(&target.as_slice()) {
                    // 行未变,继续回溯。
                    cur = repo.commit_meta(&parent)?;
                } else {
                    return Ok(Some(LineBlame { commit: cur.id.clone(), line_no }));
                }
            }
            None => {
                // 文件在父提交不存在:当前提交引入了它。
                return Ok(Some(LineBlame { commit: cur.id.clone(), line_no }));
            }
        }
    }
}

/// 按 \n 切分,保留每行内容(不含换行符)。\r 保留以便 CRLF 比较一致。
fn split_lines(b: &[u8]) -> Vec<&[u8]> {
    b.split(|&c| c == b'\n').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_blame() {
        let r = Repo::open(env!("CARGO_MANIFEST_DIR")).unwrap();
        // 本文件第 1 行是文档注释,blame 必须命中一个真实提交。
        let hit = blame_line(&r, "crates/gkl-git/src/blame.rs", 1).unwrap().unwrap();
        assert_eq!(hit.commit.len(), 40);
    }
}
