//! 提交遍历(first-parent 主线 + 全历史拓扑)。

use crate::repo::{CommitMeta, Repo};

/// 遍历模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkMode {
    /// 只走第一父链,线性历史视角。
    FirstParent,
    /// 全历史拓扑序(git log 默认语义)。
    All,
}

/// 从 start 出发按模式遍历,遇到 end(不含)停止。
/// All 模式下 start 为 "--all" 时从全部 refs 出发(git log --all 语义)。
pub fn walk(
    repo: &Repo,
    start: &str,
    end: Option<&str>,
    mode: WalkMode,
    limit: usize,
) -> anyhow::Result<Vec<CommitMeta>> {
    let mut out = Vec::new();
    let stop_oid = match end {
        Some(e) => Some(repo.repo.rev_parse_single(e.as_bytes())?.detach()),
        None => None,
    };

    let mut platform = if start == "--all" {
        // 全部 refs 的 tips(本地/远程分支、标签)。annotated tag 要 peel 到提交
        // (与 git rev-list --all 一致),否则 tag 对象指向的提交会被漏掉。
        let mut tips: Vec<gix::hash::ObjectId> = Vec::new();
        if let Ok(platform) = repo.repo.references() {
            if let Ok(iter) = platform.all() {
                for rref in iter.flatten() {
                    // 跳过 symbolic ref(如 HEAD):id() 会 panic;
                    // 它指向的目标分支本身会被枚举到。
                    if let Some(oid) = rref.target().try_id() {
                        if let Some(c) = peel_to_commit(repo, oid.to_owned()) {
                            tips.push(c);
                        }
                    }
                }
            }
        }
        repo.repo.rev_walk(tips)
    } else {
        let start_oid = repo.repo.rev_parse_single(start.as_bytes())?.detach();
        repo.repo.rev_walk([start_oid])
    };
    if matches!(mode, WalkMode::FirstParent) {
        platform = platform.first_parent_only();
    }

    let stop = stop_oid;
    let iter = platform.selected(move |id| Some(id) != stop.as_deref())?;
    for info in iter {
        let id = info?.id;
        // oid 直达,免逐提交 rev-parse。
        let meta = repo.commit_meta_by_oid(&id)?;
        out.push(meta);
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// 递归 peel:oid 可能是 commit(直接返回)或 tag 对象(取 target 再 peel,
/// 支持嵌套 tag)。不是二者则 None。
fn peel_to_commit(repo: &Repo, oid: gix::hash::ObjectId) -> Option<gix::hash::ObjectId> {
    let mut cur = oid;
    for _ in 0..10 {
        // 防御嵌套 tag 死循环。
        let obj = repo.repo.find_object(cur).ok()?;
        match obj.try_into_tag() {
            Ok(t) => match t.target_id() {
                Ok(id) => cur = id.detach(),
                Err(_) => return None,
            },
            Err(_not_tag) => {
                // 非 tag:是 commit 就返回,否则(树/blob)None。
                let is_commit = repo
                    .repo
                    .find_header(cur)
                    .map(|h| h.kind() == gix::object::Kind::Commit)
                    .unwrap_or(false);
                return if is_commit { Some(cur) } else { None };
            }
        }
    }
    None
}
