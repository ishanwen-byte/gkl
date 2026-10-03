//! 树级 diff:文件变更列表与同 oid 改名发现。
//!
//! 基于 gix tree diff(等价 git diff-tree),变更事件自带 blob oid,
//! 纯改名(同 oid 搬家)表现为 Deletion+Addition 对,直接配对即可,
//! 无需内容散列。

use crate::repo::Repo;
use gix::bstr::ByteSlice;

/// 单个文件的变更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// 变更后路径;Deletion 时为删除前的路径。
    pub path: String,
    /// 纯改名时的旧路径。
    pub old_path: Option<String>,
    pub kind: ChangeKind,
    /// 变更前 blob oid(hex;Addition 为 None)。
    pub old_oid: Option<String>,
    /// 变更后 blob oid(hex;Deletion 为 None)。
    pub new_oid: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Addition,
    Deletion,
    Modification,
    /// 同 oid 搬家(git mv / 纯改名)。
    Rename,
}

/// diff lhs_rev -> rhs_rev 两棵树,返回文件级变更(已把同 oid 的
/// Deletion+Addition 合并为 Rename)。
pub fn tree_diff(repo: &Repo, lhs_rev: &str, rhs_rev: &str) -> anyhow::Result<Vec<FileChange>> {
    let lhs = tree_of(repo, lhs_rev)?;
    let rhs = tree_of(repo, rhs_rev)?;

    let mut adds: Vec<(String, String)> = Vec::new(); // (path, oid)
    let mut dels: Vec<(String, String)> = Vec::new();
    let mut out: Vec<FileChange> = Vec::new();

    let mut platform = lhs.changes()?;
    platform.track_path();
    // 关闭重写跟踪:我们自己做同 oid 配对,且避免 partial clone 触发 blob 相似度读取。
    platform.track_rewrites(None);
    platform.for_each_to_obtain_tree(&rhs, |change| {
        use gix::object::tree::diff::change::Event;
        // 只统计 blob(文件);目录/子模块/符号链接不算文件 churn。
        let is_file = match &change.event {
            Event::Addition { entry_mode, .. }
            | Event::Deletion { entry_mode, .. }
            | Event::Modification { entry_mode, .. } => entry_mode.is_blob(),
            _ => false,
        };
        if !is_file {
            return Ok::<gix::object::tree::diff::Action, std::convert::Infallible>(
                gix::object::tree::diff::Action::Continue,
            );
        }
        let path = change.location.to_str_lossy().into_owned();
        match change.event {
            Event::Addition { id, .. } => adds.push((path, id.to_hex().to_string())),
            Event::Deletion { id, .. } => dels.push((path, id.to_hex().to_string())),
            Event::Modification { previous_id, id, .. } => out.push(FileChange {
                path,
                old_path: None,
                kind: ChangeKind::Modification,
                old_oid: Some(previous_id.to_hex().to_string()),
                new_oid: Some(id.to_hex().to_string()),
            }),
            _ => {}
        }
        Ok::<gix::object::tree::diff::Action, std::convert::Infallible>(
            gix::object::tree::diff::Action::Continue,
        )
    })?;

    // 同 oid 的 Deletion+Addition 配对为 Rename;未配对的按原样输出。
    let mut used_del: Vec<bool> = vec![false; dels.len()];
    for (apath, aoid) in adds {
        let mut paired = None;
        for (i, (dpath, doid)) in dels.iter().enumerate() {
            if !used_del[i] && doid == &aoid {
                used_del[i] = true;
                paired = Some(dpath.clone());
                break;
            }
        }
        match paired {
            Some(dpath) => out.push(FileChange {
                path: apath,
                old_path: Some(dpath),
                kind: ChangeKind::Rename,
                old_oid: Some(aoid.clone()),
                new_oid: Some(aoid),
            }),
            None => out.push(FileChange {
                path: apath,
                old_path: None,
                kind: ChangeKind::Addition,
                old_oid: None,
                new_oid: Some(aoid),
            }),
        }
    }
    for (i, (dpath, doid)) in dels.into_iter().enumerate() {
        if !used_del[i] {
            out.push(FileChange {
                path: dpath,
                old_path: None,
                kind: ChangeKind::Deletion,
                old_oid: Some(doid),
                new_oid: None,
            });
        }
    }
    Ok(out)
}

/// 在 parent_rev -> cur_rev 的 diff 里,为 cur_path 找同 oid 的旧路径。
/// 返回 Some(old_path) 表示该文件是从 old_path 纯改名而来。
pub fn find_rename(
    repo: &Repo,
    parent_rev: &str,
    cur_rev: &str,
    cur_path: &str,
) -> anyhow::Result<Option<String>> {
    // 快路径:目标 oid 直接取自 cur 树。
    if repo.blob_oid_at(cur_rev, cur_path)?.is_none() {
        return Ok(None);
    }
    if repo.blob_oid_at(parent_rev, cur_path)?.is_some() {
        return Ok(None); // 路径在父已存在,不是改名
    }
    let changes = tree_diff(repo, parent_rev, cur_rev)?;
    Ok(changes
        .into_iter()
        .find(|c| c.kind == ChangeKind::Rename && c.path == cur_path)
        .and_then(|c| c.old_path))
}

/// 快路径 tree diff:跳过同 oid 配对;lhs_rev 也可以是 "empty" 表示空树(根提交)。
pub fn tree_diff_fast(
    repo: &Repo,
    lhs_rev: &str,
    rhs_rev: &str,
) -> anyhow::Result<Vec<FileChange>> {
    let rhs = tree_of(repo, rhs_rev)?;
    let mut out = Vec::new();
    let empty = if lhs_rev == "empty" { Some(empty_tree(repo)?) } else { None };
    let lhs = if lhs_rev == "empty" { None } else { Some(tree_of(repo, lhs_rev)?) };
    let mut platform = match (&empty, &lhs) {
        (Some(e), _) => e.changes()?,
        (_, Some(l)) => l.changes()?,
        _ => unreachable!(),
    };
    platform.track_path();
    platform.track_rewrites(None);
    platform.for_each_to_obtain_tree(&rhs, |change| {
        use gix::object::tree::diff::change::Event;
        // 只统计 blob(文件);目录/子模块/符号链接不算。
        let is_file = match &change.event {
            Event::Addition { entry_mode, .. }
            | Event::Deletion { entry_mode, .. }
            | Event::Modification { entry_mode, .. } => entry_mode.is_blob(),
            _ => false,
        };
        if !is_file {
            return Ok::<gix::object::tree::diff::Action, std::convert::Infallible>(
                gix::object::tree::diff::Action::Continue,
            );
        }
        let path = change.location.to_str_lossy().into_owned();
        match change.event {
            Event::Addition { id, .. } => out.push(FileChange {
                path,
                old_path: None,
                kind: ChangeKind::Addition,
                old_oid: None,
                new_oid: Some(id.to_hex().to_string()),
            }),
            Event::Deletion { id, .. } => out.push(FileChange {
                path,
                old_path: None,
                kind: ChangeKind::Deletion,
                old_oid: Some(id.to_hex().to_string()),
                new_oid: None,
            }),
            Event::Modification { previous_id, id, .. } => out.push(FileChange {
                path,
                old_path: None,
                kind: ChangeKind::Modification,
                old_oid: Some(previous_id.to_hex().to_string()),
                new_oid: Some(id.to_hex().to_string()),
            }),
            _ => {}
        }
        Ok::<gix::object::tree::diff::Action, std::convert::Infallible>(
            gix::object::tree::diff::Action::Continue,
        )
    })?;
    Ok(out)
}

/// 空树对象(git 约定 oid 4b825dc...,每个仓库都有)。
fn empty_tree<'a>(repo: &'a Repo) -> anyhow::Result<gix::Tree<'a>> {
    let oid = gix::hash::ObjectId::from_hex(
        b"4b825dc642cb6eb9a060e54bf8d69288fbee4904",
    )
    .unwrap();
    let obj = repo.repo.find_object(oid)?;
    Ok(obj.try_into_tree()?)
}

fn tree_of<'a>(repo: &'a Repo, rev: &str) -> anyhow::Result<gix::Tree<'a>> {
    let oid = repo.repo.rev_parse_single(rev.as_bytes())?.detach();
    let commit = repo.repo.find_object(oid)?.try_into_commit()?;
    let tree = commit.tree()?;
    // Tree 借用 repo(非函数参数 rev),生命周期由 repo 决定。
    Ok(tree)
}
