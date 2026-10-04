//! redb 存储层:表定义、打开、读写。

use gkl_git::repo::CommitMeta;
use rayon::prelude::*;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use std::path::Path;

/// 索引结构版本。结构变化时递增,旧库自动重建。
/// v1: 初始。v2: author_tz_offset 从秒修正为分钟。
/// v3: mailmap 归一字段 + churn 表 + 全历史模式。
pub const SCHEMA_VERSION: u64 = 3;

const T_META: TableDefinition<&str, &[u8]> = TableDefinition::new("commit_meta");
const T_STATE: TableDefinition<&str, &[u8]> = TableDefinition::new("scan_state");
const T_CHURN: TableDefinition<&str, &[u8]> = TableDefinition::new("file_churn");

/// 单文件 churn 累计。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ChurnEntry {
    /// 变更次数(含增/删/改/改名)。
    pub commits: u64,
    /// 累计新增行。
    pub added: u64,
    /// 累计删除行。
    pub deleted: u64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ScanState {
    schema_version: u64,
    /// 扫描起点 tip 的完整 id。
    tip: String,
    /// 已索引提交数。
    indexed: u64,
}

/// 打开的索引数据库。
pub struct Db {
    db: Database,
    path: std::path::PathBuf,
}

impl Db {
    /// 打开(不存在则创建)索引文件。仓库 id 用于隔离多仓库索引。
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let db = Database::create(&path)?;
        // 建表(幂等)。
        let txn = db.begin_write()?;
        {
            let _t = txn.open_table(T_META)?;
            let _t = txn.open_table(T_STATE)?;
            let _t = txn.open_table(T_CHURN)?;
        }
        txn.commit()?;
        Ok(Self { db, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 是否已有有效索引且 schema 匹配。
    pub fn is_fresh(&self) -> bool {
        let Ok(txn) = self.db.begin_read() else {
            return false;
        };
        let Ok(t) = txn.open_table(T_STATE) else {
            return false;
        };
        match t.get("state") {
            Ok(Some(v)) => match serde_json::from_slice::<ScanState>(v.value()) {
                Ok(s) => s.schema_version == SCHEMA_VERSION,
                Err(_) => false,
            },
            _ => false,
        }
    }

    /// 批量写入提交元数据并更新扫描状态(一个事务)。
    pub fn write_commits(&self, metas: &[CommitMeta], tip: &str) -> anyhow::Result<u64> {
        let txn = self.db.begin_write()?;
        let mut total;
        {
            let mut t = txn.open_table(T_META)?;
            for m in metas {
                let key: &str = &m.id;
                let val = serde_json::to_vec(m)?;
                t.insert(key, val.as_slice())?;
            }
            let mut s = txn.open_table(T_STATE)?;
            total = metas.len() as u64;
            let prev: ScanState = match s.get("state") {
                Ok(Some(v)) => serde_json::from_slice(v.value()).unwrap_or_else(|_| ScanState {
                    schema_version: 0,
                    tip: String::new(),
                    indexed: 0,
                }),
                _ => ScanState {
                    schema_version: 0,
                    tip: String::new(),
                    indexed: 0,
                },
            };
            total += prev.indexed;
            let state = ScanState {
                schema_version: SCHEMA_VERSION,
                tip: tip.to_string(),
                indexed: total,
            };
            let encoded = serde_json::to_vec(&state)?;
            s.insert("state", encoded.as_slice())?;
        }
        txn.commit()?;
        Ok(total)
    }

    /// 读取单条提交。
    pub fn commit(&self, id: &str) -> anyhow::Result<Option<CommitMeta>> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(T_META)?;
        match t.get(id)? {
            Some(v) => Ok(Some(serde_json::from_slice::<CommitMeta>(v.value())?)),
            None => Ok(None),
        }
    }

    /// 遍历全部已索引提交(用于统计)。
    pub fn all_commits(&self) -> anyhow::Result<Vec<CommitMeta>> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(T_META)?;
        let mut out = Vec::new();
        for row in t.iter()? {
            let (_, v) = row?;
            out.push(serde_json::from_slice::<CommitMeta>(v.value())?);
        }
        Ok(out)
    }

    /// 已索引提交数。
    pub fn len(&self) -> anyhow::Result<u64> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(T_META)?;
        Ok(t.len()?)
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn is_empty(&self) -> anyhow::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// 上次扫描的 tip(增量起点;历史改写检测也用它)。
    pub fn last_tip(&self) -> anyhow::Result<Option<String>> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(T_STATE)?;
        match t.get("state")? {
            Some(v) => {
                let s: ScanState = serde_json::from_slice(v.value())?;
                Ok(Some(s.tip))
            }
            None => Ok(None),
        }
    }

    /// 增量写入:已存在的 key 跳过(避免覆盖旧 schema 数据/重复计数)。
    pub fn write_commits_skip_existing(
        &self,
        metas: &[CommitMeta],
        tip: &str,
    ) -> anyhow::Result<u64> {
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(T_META)?;
            for m in metas {
                let key: &str = &m.id;
                if t.get(key)?.is_some() {
                    continue;
                }
                let val = serde_json::to_vec(m)?;
                t.insert(key, val.as_slice())?;
            }
            let mut s = txn.open_table(T_STATE)?;
            let state = ScanState {
                schema_version: SCHEMA_VERSION,
                tip: tip.to_string(),
                indexed: t.len()?,
            };
            let encoded = serde_json::to_vec(&state)?;
            s.insert("state", encoded.as_slice())?;
        }
        txn.commit()?;
        self.len()
    }

    /// 全量重建 churn:清空后累计每个提交(对第一父)的文件级变更。
    /// 性能:单事务批量写;blob 用 oid 直读(避免逐文件 rev-parse)。
    pub fn rebuild_churn(&self, repo: &gkl_git::Repo, metas: &[CommitMeta]) -> anyhow::Result<()> {
        self.churn_inner(repo, metas, true)
    }

    /// 增量累计 churn(只传入新提交区间)。
    pub fn add_churn(&self, repo: &gkl_git::Repo, metas: &[CommitMeta]) -> anyhow::Result<()> {
        self.churn_inner(repo, metas, false)
    }

    fn churn_inner(
        &self,
        repo: &gkl_git::Repo,
        metas: &[CommitMeta],
        clear_first: bool,
    ) -> anyhow::Result<()> {
        // 并行:每线程独立开 Repo(gix Repository 非 Sync),互不共享缓存;
        // 结果按文件路径 reduce 合并。万提交仓库从串行约 30min 降到分钟级。
        let workdir = repo.workdir().unwrap_or_else(|| ".".into());
        let per_file: std::collections::HashMap<String, ChurnEntry> = metas
            .par_iter()
            .map_init(
                || gkl_git::Repo::open(&workdir).ok(),
                |repo_opt, m| {
                    let Some(r) = repo_opt else { return Vec::new() };
                    // merge 跳过(与 git numstat 默认语义一致);无父对空树。
                    if m.parents.len() > 1 {
                        return Vec::new();
                    }
                    let changes = if m.parents.is_empty() {
                        gkl_git::diff::tree_diff_fast(r, "empty", &m.id).unwrap_or_default()
                    } else {
                        gkl_git::diff::tree_diff_fast(r, &m.parents[0], &m.id).unwrap_or_default()
                    };
                    let mut local: Vec<(String, ChurnEntry)> = Vec::new();
                    for ch in changes {
                        let mut e = ChurnEntry {
                            commits: 1,
                            ..Default::default()
                        };
                        match ch.kind {
                            gkl_git::diff::ChangeKind::Addition => {
                                if let Some(hex) = &ch.new_oid {
                                    if let Some(blob) = r.blob_by_oid(hex) {
                                        e.added = count_lines(&blob);
                                    }
                                }
                            }
                            gkl_git::diff::ChangeKind::Modification => {
                                if let (Some(o), Some(n)) = (&ch.old_oid, &ch.new_oid) {
                                    if let (Some(ob), Some(nb)) =
                                        (r.blob_by_oid(o), r.blob_by_oid(n))
                                    {
                                        let (a, d) = line_delta_raw(&ob, &nb);
                                        e.added = a;
                                        e.deleted = d;
                                    }
                                }
                            }
                            gkl_git::diff::ChangeKind::Deletion => {
                                if let Some(hex) = &ch.old_oid {
                                    if let Some(blob) = r.blob_by_oid(hex) {
                                        e.deleted = count_lines(&blob);
                                    }
                                }
                            }
                            _ => {}
                        }
                        local.push((ch.path, e));
                    }
                    local
                },
            )
            .flatten()
            .fold(
                std::collections::HashMap::<String, ChurnEntry>::new,
                |mut acc: std::collections::HashMap<String, ChurnEntry>, (path, e)| {
                    let slot = acc.entry(path).or_default();
                    slot.commits += e.commits;
                    slot.added += e.added;
                    slot.deleted += e.deleted;
                    acc
                },
            )
            .reduce(
                std::collections::HashMap::<String, ChurnEntry>::new,
                |mut a: std::collections::HashMap<String, ChurnEntry>, b| {
                    for (path, e) in b {
                        let slot = a.entry(path).or_default();
                        slot.commits += e.commits;
                        slot.added += e.added;
                        slot.deleted += e.deleted;
                    }
                    a
                },
            );

        let txn = self.db.begin_write()?;
        {
            if clear_first {
                txn.delete_table(T_CHURN)?;
            }
            let mut t = txn.open_table(T_CHURN)?;
            for (path, e) in per_file {
                let val = serde_json::to_vec(&e)?;
                t.insert(path.as_str(), val.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// churn 表非空?
    pub fn has_churn(&self) -> anyhow::Result<bool> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(T_CHURN)?;
        Ok(t.len()? > 0)
    }

    /// 读全部 churn,按变更次数降序。
    pub fn all_churn(&self) -> anyhow::Result<Vec<(String, ChurnEntry)>> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(T_CHURN)?;
        let mut out = Vec::new();
        for row in t.iter()? {
            let (k, v) = row?;
            out.push((
                k.value().to_string(),
                serde_json::from_slice::<ChurnEntry>(v.value())?,
            ));
        }
        out.sort_by_key(|(_, e)| std::cmp::Reverse(e.commits));
        Ok(out)
    }

    /// 清空全部索引(重建用)。commit 表与扫描状态一并清零。
    pub fn reset(&self) -> anyhow::Result<()> {
        let txn = self.db.begin_write()?;
        {
            txn.delete_table(T_META)?;
            txn.delete_table(T_STATE)?;
            txn.delete_table(T_CHURN)?;
            let _t = txn.open_table(T_META)?;
            let _t = txn.open_table(T_STATE)?;
            let _t = txn.open_table(T_CHURN)?;
        }
        txn.commit()?;
        Ok(())
    }
}

/// 行数统计(\n 计数,末行无换行也计 1)。
fn count_lines(b: &[u8]) -> u64 {
    if b.is_empty() {
        return 0;
    }
    (b.iter().filter(|&&c| c == b'\n').count() as u64)
        + if b.last() != Some(&b'\n') { 1 } else { 0 }
}

/// 两版内容的行级 (added, deleted)。similar 行 diff 的 Insert/Delete/Replace 计数。
fn line_delta_raw(old: &[u8], new: &[u8]) -> (u64, u64) {
    use similar::{DiffOp, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    let (mut a, mut d) = (0u64, 0u64);
    for op in diff.ops() {
        match op {
            DiffOp::Insert { new_len, .. } => a += *new_len as u64,
            DiffOp::Delete { old_len, .. } => d += *old_len as u64,
            DiffOp::Replace {
                old_len, new_len, ..
            } => {
                d += *old_len as u64;
                a += *new_len as u64;
            }
            DiffOp::Equal { .. } => {}
        }
    }
    (a, d)
}
