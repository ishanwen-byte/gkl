//! gkl-git: gitoxide 只读访问层。
//!
//! 只做读取:提交遍历、树路径解析、blob 内容读取、轻量 blame。
//! 永不写入仓库,不执行任何 hook 或仓库内代码。

pub mod blame;
pub mod diff;
pub mod repo;
pub mod walk;

pub use repo::Repo;
