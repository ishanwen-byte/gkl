//! gkl-core: 查询引擎。
//!
//! 组合 gkl-git(读仓库)与 gkl-db(索引),提供考古查询:
//! log、churn 热点、作者统计、blame、深链接。

pub mod query;
pub mod deeplink;

pub use deeplink::DeepLinkTarget;
