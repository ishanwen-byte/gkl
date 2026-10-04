//! 增量扫描:git 仓库 -> redb 索引。
//!
//! 默认 first-parent 主线(快);`scan --all` 全历史拓扑(git log 语义)。
//! 增量:跳过已索引提交;若上次 tip 已不在新历史中(改写)则全量重建。
//! churn:全历史模式下顺带累计每文件增删行数,供 `gkl churn` 查询。

use crate::store::Db;
use gkl_git::repo::CommitMeta;
use gkl_git::walk::WalkMode;
use gkl_git::Repo;

/// 扫描结果统计。
#[derive(Debug)]
pub struct ScanReport {
    /// 本次新索引的提交数。
    pub new_commits: usize,
    /// 索引内提交总数。
    pub total: u64,
    /// 本次的 tip。
    pub tip: String,
    /// 全历史模式(包含侧链)。
    pub all_history: bool,
}

/// 遍历起点:全历史模式用 --all(全部 refs),否则 HEAD。
fn start_ref(all: bool) -> &'static str {
    if all {
        "--all"
    } else {
        "HEAD"
    }
}

/// 执行一次扫描。
///
/// 增量逻辑:遍历新历史,已索引的提交跳过写入;计数以表中实际条数为准。
/// 改写检测:上次 tip 不在新历史里 -> 全量重建。
pub fn scan(repo: &Repo, db: &Db, force: bool, all: bool) -> anyhow::Result<ScanReport> {
    let tip = repo.head_id()?;
    let mode = if all {
        WalkMode::All
    } else {
        WalkMode::FirstParent
    };

    if force || !db.is_fresh() {
        db.reset()?;
        let metas = gkl_git::walk::walk(repo, start_ref(all), None, mode, usize::MAX)?;
        let total = db.write_commits(&metas, &tip)?;
        if all {
            db.rebuild_churn(repo, &metas)?;
        }
        return Ok(ScanReport {
            new_commits: metas.len(),
            total,
            tip,
            all_history: all,
        });
    }

    let last_tip = db.last_tip()?.unwrap_or_default();
    if last_tip == tip {
        // tip 未变;但若用户刚切 --all 且 churn 为空,补建 churn。
        if all && !db.has_churn()? {
            let metas = gkl_git::walk::walk(repo, "HEAD", None, WalkMode::All, usize::MAX)?;
            db.rebuild_churn(repo, &metas)?;
        }
        return Ok(ScanReport {
            new_commits: 0,
            total: db.len()?,
            tip,
            all_history: all,
        });
    }

    // 改写检测:旧 tip 是否仍可达。
    let reachable = gkl_git::walk::walk(repo, start_ref(all), None, mode, usize::MAX)?
        .iter()
        .any(|m| m.id == last_tip);

    if reachable {
        // 增量:遍历全部但只写未索引的(write_commits 内部跳过已有 key)。
        let metas = gkl_git::walk::walk(repo, start_ref(all), None, mode, usize::MAX)?;
        let before = db.len()?;
        db.write_commits_skip_existing(&metas, &tip)?;
        if all {
            // churn 只重算新提交区间(旧 tip 起),已有累计不重复计。
            let new_range = gkl_git::walk::walk(
                repo,
                "HEAD",
                Some(&last_tip),
                WalkMode::FirstParent,
                usize::MAX,
            )?;
            db.add_churn(repo, &new_range)?;
        }
        let total = db.len()?;
        return Ok(ScanReport {
            new_commits: (total - before) as usize,
            total,
            tip,
            all_history: all,
        });
    }

    // 改写:全量重建。
    db.reset()?;
    let metas = gkl_git::walk::walk(repo, start_ref(all), None, mode, usize::MAX)?;
    let total = db.write_commits(&metas, &tip)?;
    if all {
        db.rebuild_churn(repo, &metas)?;
    }
    Ok(ScanReport {
        new_commits: metas.len(),
        total,
        tip,
        all_history: all,
    })
}

/// 读出全部提交,按时间倒序。
pub fn commits_desc(db: &Db) -> anyhow::Result<Vec<CommitMeta>> {
    let mut all = db.all_commits()?;
    all.sort_by_key(|m| std::cmp::Reverse(m.author_time));
    Ok(all)
}
