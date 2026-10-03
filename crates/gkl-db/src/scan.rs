//! 增量扫描:git 仓库 -> redb 索引。
//!
//! 策略:first-parent 主线全量,侧链提交待需要时再索引(保持首版简单)。
//! 后续可换 commit-graph 驱动的拓扑遍历 + 批量并行。

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
}

/// 执行一次增量扫描。
///
/// 增量逻辑:上次 tip 仍在新历史的第一父链上,则只索引 (新tip, 旧tip] 区间;
/// 不在(历史被改写/rebase/force push),全量重建。
pub fn scan(repo: &Repo, db: &Db, force: bool) -> anyhow::Result<ScanReport> {
    let tip = repo.head_id()?;
    if force || !db.is_fresh() {
        db.reset()?;
        let metas = gkl_git::walk::walk(repo, "HEAD", None, WalkMode::FirstParent, usize::MAX)?;
        let total = db.write_commits(&metas, &tip)?;
        return Ok(ScanReport { new_commits: metas.len(), total, tip });
    }

    let last_tip = db.last_tip()?.unwrap_or_default();
    if last_tip == tip {
        return Ok(ScanReport { new_commits: 0, total: db.len()?, tip });
    }

    // 旧 tip 是否仍在新历史第一父链上?
    let on_mainline = gkl_git::walk::walk(
        repo,
        "HEAD",
        None,
        WalkMode::FirstParent,
        usize::MAX,
    )?
    .iter()
    .any(|m| m.id == last_tip);

    if on_mainline {
        // 增量:只索引新区间。
        let metas = gkl_git::walk::walk(
            repo,
            "HEAD",
            Some(&last_tip),
            WalkMode::FirstParent,
            usize::MAX,
        )?;
        let total = db.write_commits(&metas, &tip)?;
        Ok(ScanReport { new_commits: metas.len(), total, tip })
    } else {
        // 改写检测:全量重建。
        db.reset()?;
        let metas = gkl_git::walk::walk(repo, "HEAD", None, WalkMode::FirstParent, usize::MAX)?;
        let total = db.write_commits(&metas, &tip)?;
        Ok(ScanReport { new_commits: metas.len(), total, tip })
    }
}

/// 读出全部提交,按时间倒序。
pub fn commits_desc(db: &Db) -> anyhow::Result<Vec<CommitMeta>> {
    let mut all = db.all_commits()?;
    all.sort_by(|a, b| b.author_time.cmp(&a.author_time));
    Ok(all)
}
