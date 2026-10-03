//! redb 存储层:表定义、打开、读写。

use gkl_git::repo::CommitMeta;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use std::path::Path;

/// 索引结构版本。结构变化时递增,旧库自动重建。
pub const SCHEMA_VERSION: u64 = 1;

const T_META: TableDefinition<&str, &[u8]> = TableDefinition::new("commit_meta");
const T_STATE: TableDefinition<&str, &[u8]> = TableDefinition::new("scan_state");

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
        }
        txn.commit()?;
        Ok(Self { db, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 是否已有有效索引且 schema 匹配。
    pub fn is_fresh(&self) -> bool {
        let Ok(txn) = self.db.begin_read() else { return false };
        let Ok(t) = txn.open_table(T_STATE) else { return false };
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
                _ => ScanState { schema_version: 0, tip: String::new(), indexed: 0 },
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

    /// 清空全部索引(重建用)。
    pub fn reset(&self) -> anyhow::Result<()> {
        let txn = self.db.begin_write()?;
        {
            txn.delete_table(T_META)?;
            let _t = txn.open_table(T_META)?;
        }
        txn.commit()?;
        Ok(())
    }
}
