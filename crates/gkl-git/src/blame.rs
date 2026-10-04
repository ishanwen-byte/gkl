//! blame:回答"这一行最后被谁改的"。
//!
//! 算法(逐提交回溯 + 行映射):
//! 1. 从 HEAD 出发,目标 (path, line)。
//! 2. 对每个提交 C 与其父 P:
//!    - 单父:diff P->C 的 blob,similar 把行号映射回 P;行未变则继续,变了则 C 是答案。
//!    - merge:逐父尝试,P1 行未变则走 P1;否则试 P2(穿透到侧支,git 默认语义)。
//!    - 父无该路径:先试同 oid 纯改名;再试相似度改名(≥0.5,git -M 简化版),
//!      旧内容行映射决定继续回溯还是终止。
//! 3. 到根提交即答案(该行自初始就存在)。
//!
//! 已知偏差:相似度匹配是逐提交局部判断,极小文件(几行)可能因阈值
//! 产生误配;git -M 还有文件大小下限与全仓扫描优化,这里只看本提交的
//! Deletion 候选(足够覆盖典型 refactor 场景)。

use crate::repo::Repo;
use similar::{DiffOp, TextDiff};

/// 一行的 blame 结果。
#[derive(Debug, Clone)]
pub struct LineBlame {
    /// 命中的提交 id。
    pub commit: String,
    /// 该行在命中提交时的行号(1-based)。
    pub line_no: usize,
    /// blame 跟随过的路径链(含起点);rename 时 >1 项。
    pub path_chain: Vec<String>,
}

/// 找 HEAD 文件第 line_no(1-based)行最后修改它的提交。
pub fn blame_line(repo: &Repo, path: &str, line_no: usize) -> anyhow::Result<Option<LineBlame>> {
    blame_from(repo, "HEAD", path, line_no, 0)
}

/// 从任意 rev 开始 blame。
pub fn blame_from(
    repo: &Repo,
    rev: &str,
    path: &str,
    line_no: usize,
    max_hops: usize,
) -> anyhow::Result<Option<LineBlame>> {
    let head = repo.rev(rev)?;
    let Some(blob) = repo.blob_at(&head, path)? else {
        anyhow::bail!("{} 上不存在文件: {}", rev, path);
    };
    let lines = split_lines(&blob);
    if line_no == 0 || line_no > lines.len() {
        anyhow::bail!("行号越界: 文件共 {} 行,请求 {}", lines.len(), line_no);
    }

    let mut cur_path = path.to_string();
    let mut cur_line = line_no;
    let mut path_chain = vec![cur_path.clone()];
    let mut cur = repo.commit_meta(&head)?;

    for hop in 0.. {
        if max_hops > 0 && hop >= max_hops {
            break;
        }
        let Some(next) = (match step_back(repo, &cur, &cur_path, cur_line, &mut path_chain) {
            Ok(v) => v,
            // 浅克隆/partial clone 缺父对象:停止回溯,把当前提交作为答案。
            // 与 README 声明的 partial clone 降级语义一致。
            Err(e) if is_missing_object(&e) => break,
            Err(e) => return Err(e),
        }) else {
            break;
        };
        let (pid, ppath, pline) = next;
        cur = repo.commit_meta(&pid)?;
        cur_path = ppath;
        cur_line = pline;
    }
    Ok(Some(LineBlame {
        commit: cur.id.clone(),
        line_no: cur_line,
        path_chain,
    }))
}

/// 单步回溯。Some((父, 父路径, 父行号)) = 行未变可继续;None = 当前提交是答案。
fn step_back(
    repo: &Repo,
    cur: &crate::repo::CommitMeta,
    path: &str,
    line: usize,
    path_chain: &mut Vec<String>,
) -> anyhow::Result<Option<(String, String, usize)>> {
    let Some(cur_blob) = repo.blob_at(&cur.id, path)? else {
        anyhow::bail!("提交 {} 缺文件 {}", &cur.id[..7], path);
    };

    for (i, parent_id) in cur.parents.iter().enumerate() {
        match repo.blob_at(parent_id, path)? {
            Some(pb) => {
                if let Some(parent_line) = map_line(&pb, &cur_blob, line) {
                    return Ok(Some((parent_id.clone(), path.to_string(), parent_line)));
                }
                // 行变了;还有父可试则继续。
                if i == cur.parents.len() - 1 {
                    return Ok(None);
                }
            }
            None => {
                // 父无该路径:同 oid 纯改名跟随。
                if let Some(old_path) = crate::diff::find_rename(repo, parent_id, &cur.id, path)? {
                    path_chain.push(old_path.clone());
                    // 纯改名行号不变。
                    return Ok(Some((parent_id.clone(), old_path, line)));
                }
                // 相似度改名跟随(git -M 简化版):改名同时改内容的场景。
                if let Some((old_path, old_blob)) =
                    crate::diff::find_rename_similar(repo, parent_id, &cur.id, &cur_blob)?
                {
                    // 旧内容里的行号映射:目标行未变则继续回溯,变了则本提交是答案。
                    if let Some(parent_line) = map_line(&old_blob, &cur_blob, line) {
                        path_chain.push(old_path.clone());
                        return Ok(Some((parent_id.clone(), old_path, parent_line)));
                    }
                    // 行在本次改名提交中新建:当前提交即答案。
                    return Ok(None);
                }
                if i == cur.parents.len() - 1 {
                    return Ok(None);
                }
            }
        }
    }
    Ok(None)
}

/// 把 cur_blob 的第 line(1-based)行映射回 parent_blob 的行号。
/// 返回 Some(parent_line) 表示该行在 Equal 段(未变);
/// None 表示行处于 Insert/Delete/Replace 段(本提交改了它)。
fn map_line(parent_blob: &[u8], cur_blob: &[u8], line: usize) -> Option<usize> {
    let diff = TextDiff::from_lines(parent_blob, cur_blob);
    for op in diff.ops() {
        if let DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            // new 段覆盖 0-based [new_index, new_index+len);目标行 line-1 落在内则映射。
            let n0 = line - 1;
            if n0 >= *new_index && n0 < *new_index + len {
                return Some(old_index + (n0 - new_index) + 1);
            }
        }
    }
    None
}

/// 按 \n 切分(保留 \r 以便 CRLF 一致比较)。
fn split_lines(b: &[u8]) -> Vec<&[u8]> {
    b.split(|&c| c == b'\n').collect()
}

/// 判断错误链里是否是“对象缺失”(浅克隆/partial clone 回溯到边界)。
/// gitoxide 的消��形如 "An object prefixed <hex> could not be found"
/// 或 "The ref partially named \"<hex>\" could not be found"。
fn is_missing_object(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        let msg = cause.to_string();
        msg.contains("could not be found")
            && (msg.contains("An object prefixed") || msg.contains("The ref partially named"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_blame() {
        let r = Repo::open(env!("CARGO_MANIFEST_DIR")).unwrap();
        // 浅克隆(depth 1,如 CI checkout)下父对象缺失,blame 应回退到 HEAD 而非报错。
        let hit = blame_line(&r, "crates/gkl-git/src/blame.rs", 1)
            .unwrap()
            .unwrap();
        assert_eq!(hit.commit.len(), 40);
    }

    #[test]
    fn missing_object_message_match() {
        // gitoxide 缺对象错误的两种真实消息形态,降级判定必须命中。
        let cases = [
            "An object prefixed e71dfe47b2a20bb97 could not be found",
            "The ref partially named \"e71dfe47\" could not be found",
        ];
        for c in cases {
            let e = anyhow::anyhow!(c);
            assert!(is_missing_object(&e), "should match: {c}");
        }
        // 非缺对象错误不得误判。
        let other = anyhow::anyhow!("corrupt loose object file");
        assert!(!is_missing_object(&other));
    }

    #[test]
    fn map_line_identity() {
        // 全等文件:行号原样映射。
        assert_eq!(map_line(b"a\nb\nc\n", b"a\nb\nc\n", 2), Some(2));
    }

    #[test]
    fn map_line_insert_before() {
        // 父:a b c;子:在头部插入 x -> a b c。子行 2(a)应映射回父行 1。
        assert_eq!(map_line(b"a\nb\nc\n", b"x\na\nb\nc\n", 2), Some(1));
        // 子行 1(x)是新增 -> None。
        assert_eq!(map_line(b"a\nb\nc\n", b"x\na\nb\nc\n", 1), None);
    }

    #[test]
    fn map_line_delete_before() {
        // 父:x a b;子删掉 x -> a b。子行 1(a)映射回父行 2。
        assert_eq!(map_line(b"x\na\nb\n", b"a\nb\n", 1), Some(2));
    }
}
