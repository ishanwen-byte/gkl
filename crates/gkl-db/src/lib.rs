//! gkl-db: redb 索引引擎。
//!
//! 持久化 commit 元数据与文件 churn 统计,支持增量扫描:
//! 记录已扫描的 tip,下次只处理新提交。

pub mod scan;
pub mod store;

pub use store::{Db, SCHEMA_VERSION};
