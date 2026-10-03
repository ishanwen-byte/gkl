//! 仓库打开与对象模型。

use gix::bstr::ByteSlice;
use gix::objs::CommitRef;
use std::path::{Path, PathBuf};

/// 只读仓库句柄。包一层,限制在 gkl 内部统一 API 面。
pub struct Repo {
    pub(crate) repo: gix::Repository,
}

/// 提交摘要,索引与展示共用。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommitMeta {
    /// 40 位十六进制 object id。
    pub id: String,
    pub author_name: String,
    pub author_email: String,
    /// mailmap 归一后的作者名(无 mailmap 时与 author_name 相同)。
    pub author_name_mapped: String,
    /// mailmap 归一后的作者邮箱。
    pub author_email_mapped: String,
    /// Unix 秒(作者时区无关的绝对时间)。
    pub author_time: i64,
    /// 作者时区偏移分钟,如 480 表示 UTC+8。
    pub author_tz_offset: i32,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_time: i64,
    pub message_subject: String,
    /// 父提交 id,通常 0-2 个。
    pub parents: Vec<String>,
    /// 是否 merge commit(父提交 >= 2)。
    pub is_merge: bool,
}

/// 打开失败的用户可读信息。
#[derive(Debug, thiserror::Error)]
#[error("无法打开 git 仓库 {path}: {source}")]
pub struct OpenError {
    path: PathBuf,
    #[source]
    source: gix::discover::Error,
}

impl Repo {
    /// 从给定路径打开仓库。支持三种情况:仓库根、仓库内子目录(向上发现 .git)、
    /// bare 仓库目录。与 git 命令行行为一致。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, OpenError> {
        let path = path.as_ref().to_path_buf();
        let repo = gix::discover(&path).map_err(|source| OpenError { path, source })?;
        Ok(Self { repo })
    }

    /// 仓库的 mailmap 快照(.mailmap 文件解析结果)。
    pub fn mailmap(&self) -> gix::mailmap::Snapshot {
        self.repo.open_mailmap()
    }

    /// 仓库工作目录(仓库为 bare 时返回 None)。
    pub fn workdir(&self) -> Option<std::path::PathBuf> {
        self.repo.work_dir().map(|p: &std::path::Path| p.to_path_buf())
    }

    /// HEAD 指向的提交 id。
    pub fn head_id(&self) -> anyhow::Result<String> {
        let mut head = self.repo.head()?;
        let id = head
            .peel_to_commit_in_place()?
            .id()
            .to_hex()
            .to_string();
        Ok(id)
    }

    /// 解析任意 rev(分支名、短 hash、HEAD~3 等)到完整提交 id。
    pub fn rev(&self, rev: &str) -> anyhow::Result<String> {
        let id = self.repo.rev_parse_single(rev.as_bytes())?;
        Ok(id.to_hex().to_string())
    }

    /// 读取提交元数据。接受完整或短 hash、分支名等任意 rev。
    pub fn commit_meta(&self, id: &str) -> anyhow::Result<CommitMeta> {
        let oid = self.repo.rev_parse_single(id.as_bytes())?.detach();
        let obj = self.repo.find_object(oid)?;
        let commit = obj.try_into_commit()?;
        let cid = commit.id();
        let c: CommitRef = commit.decode()?;
        Ok(self.meta_of(&cid.to_hex().to_string(), &c))
    }

    /// CommitRef -> CommitMeta,应用 mailmap 归一。
    fn meta_of(&self, id: &str, c: &CommitRef) -> CommitMeta {
        let a = &c.author;
        let cm = &c.committer;
        let parent_count = c.parents.iter().count();
        let mailmap = self.mailmap();
        let (an, ae) = resolve_signature(&mailmap, &a.name.to_str_lossy(), &a.email.to_str_lossy());
        CommitMeta {
            id: id.to_string(),
            author_name: a.name.to_str_lossy().into_owned(),
            author_email: a.email.to_str_lossy().into_owned(),
            author_name_mapped: an,
            author_email_mapped: ae,
            author_time: a.time.seconds,
            author_tz_offset: a.time.offset / 60,
            committer_name: cm.name.to_str_lossy().into_owned(),
            committer_email: cm.email.to_str_lossy().into_owned(),
            committer_time: cm.time.seconds,
            message_subject: c
                .message()
                .summary()
                .to_str_lossy()
                .into_owned(),
            parents: c
                .parents
                .iter()
                .map(|p| p.to_str_lossy().to_lowercase())
                .collect(),
            is_merge: parent_count > 1,
        }
    }

    /// 按 hex oid 读对象原始字节(仅 blob 有意义)。
    pub fn blob_by_oid(&self, hex: &str) -> Option<Vec<u8>> {
        let oid = gix::hash::ObjectId::from_hex(hex.as_bytes()).ok()?;
        self.repo.find_object(oid).ok().map(|o| o.data.to_vec())
    }

    /// 读取某提交下指定路径的 blob oid(不读内容;树存在但 blob 缺失时仍返回 oid,
    /// 这是 partial clone 的正常状态)。
    pub fn blob_oid_at(&self, commit_id: &str, path: &str) -> anyhow::Result<Option<String>> {
        let oid = self.repo.rev_parse_single(commit_id.as_bytes())?.detach();
        let commit = self.repo.find_object(oid)?.try_into_commit()?;
        let tree = commit.tree()?;
        let mut buf = Vec::new();
        let entry = tree.lookup_entry_by_path(std::path::Path::new(path), &mut buf)?;
        Ok(entry
            .filter(|e| e.mode().is_blob())
            .map(|e| e.oid().to_hex().to_string()))
    }

    /// 读取某提交下指定路径的 blob 内容。文件不存在或不是 blob 返回 None;
    /// partial clone 缺 blob 时返回友好错误(提示补拉取)而非原始 IO 错误。
    pub fn blob_at(&self, commit_id: &str, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let oid = self.repo.rev_parse_single(commit_id.as_bytes())?.detach();
        let commit = self.repo.find_object(oid)?.try_into_commit()?;
        let tree = commit.tree()?;
        let mut buf = Vec::new();
        let entry = tree.lookup_entry_by_path(std::path::Path::new(path), &mut buf)?;
        match entry {
            Some(e) if e.mode().is_blob() => {
                match self.repo.find_object(e.oid()) {
                    Ok(obj) => Ok(Some(obj.data.to_vec())),
                    Err(err) => {
                        // partial clone(--filter=blob=none)下常见:本地无 blob 内容。
                        anyhow::bail!(
                            "blob {} 未在本地(疑似 partial clone)。\n\
                             解决: git fetch --refetch --no-filter 或重新完整 clone;\n\
                             或对该文件跳过内容级分析: {}",
                            e.oid().to_hex(),
                            err
                        );
                    }
                }
            }
            _ => Ok(None),
        }
    }
}

/// 用 mailmap 快照归一签名;无匹配时返回原值。
fn resolve_signature(
    mailmap: &gix::mailmap::Snapshot,
    name: &str,
    email: &str,
) -> (String, String) {
    use gix::bstr::ByteSlice;
    let sig = gix::actor::SignatureRef {
        name: name.as_bytes().as_bstr(),
        email: email.as_bytes().as_bstr(),
        time: gix::date::Time::new(0, 0),
    };
    let resolved = mailmap.resolve(sig);
    (
        resolved.name.to_str_lossy().into_owned(),
        resolved.email.to_str_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_self() {
        // 本仓库自身就是测试夹具:能打开、能取 HEAD。
        let r = Repo::open(env!("CARGO_MANIFEST_DIR")).unwrap();
        assert_eq!(r.head_id().unwrap().len(), 40);
    }
}
