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
        // 全部 refs 的 tips(本地/远程分支、标签,peel 到提交)。
        let mut tips: Vec<gix::hash::ObjectId> = Vec::new();
        match repo.repo.references() {
            Ok(platform) => match platform.all() {
                Ok(iter) => {
                    for rref in iter.flatten() {
                        // 跳过 symbolic ref(如 HEAD):id() 会 panic;
                        // 它指向的目标分支本身会被枚举到。
                        if rref.target().try_id().is_none() {
                            continue;
                        }
                        if let Ok(obj) = rref.id().object() {
                            let oid = obj.id;
                            if matches!(obj.try_into_commit(), Ok(_)) {
                                tips.push(oid);
                            }
                        }
                    }
                }
                Err(_) => {}
            },
            Err(_) => {}
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
    let iter = platform.selected(move |id| Some(id) != stop.as_ref().map(|v| &**v))?;
    for info in iter {
        let id = info?.id;
        let meta = repo.commit_meta(&id.to_hex().to_string())?;
        out.push(meta);
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}
