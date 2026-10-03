//! 离线启发式引擎:无 API 时的问题路由。
//!
//! 识别常见考古问题模式(谁写的/什么时候/为什么改/热点在哪),
//! 调用 gkl-core 查询给出带证据的确定性回答。

use crate::{Answer, Engine};
use gkl_core::deeplink::DeepLinkTarget;
use gkl_db::Db;
use gkl_git::Repo;

/// 尝试回答。无法识别的问题返回 None(CLI 层提示用户)。
pub fn answer(repo: &Repo, db: &Db, q: &str) -> anyhow::Result<Option<Answer>> {
    let q_lower = q.to_lowercase();

    // 模式:谁写的 <path>
    if q_lower.starts_with("谁") || q_lower.contains("who wrote") || q_lower.contains("作者") {
        if let Some(path) = extract_path(q) {
            if let Some(a) = who_wrote(repo, db, &path)? {
                return Ok(Some(a));
            }
        }
    }

    // 模式:热点/churn
    if q_lower.contains("热点") || q_lower.contains("churn") || q_lower.contains("最多改动") {
        return Ok(Some(hotspots(repo, db)?));
    }

    // 模式:作者排行
    if q_lower.contains("排行") || q_lower.contains("贡献") || q_lower.contains("统计作者") {
        return Ok(Some(top_authors(db)?));
    }

    Ok(None)
}

/// 从问题里抽取一个疑似文件路径的 token。
/// 规则:含扩展名形态(点后跟字母数字)即可,有无目录斜杠都行
/// (README.md 与 a/b.rs 都认);裁剪常见包裹/句尾标点。
fn extract_path(q: &str) -> Option<String> {
    q.split_whitespace()
        .map(|t| {
            t.trim_matches(|c: char| {
                ['<', '>', '"', '`', '?', '.', ',', ';', ':', '。', '，'].contains(&c)
            })
        })
        .find(|t| match t.rfind('.') {
            Some(i) => t[i + 1..].chars().any(|c| c.is_alphanumeric()),
            None => false,
        })
        .map(|s| s.to_string())
}

fn who_wrote(repo: &Repo, db: &Db, path: &str) -> anyhow::Result<Option<Answer>> {
    let head = repo.head_id()?;
    // 文件在 HEAD 的大小,判断存在性。
    let Some(blob) = repo.blob_at(&head, &path)? else {
        return Ok(None);
    };
    let lines = blob.iter().filter(|&&b| b == b'\n').count().max(1);
    let meta = repo.commit_meta(&head)?;
    let in_index = db.commit(&meta.id)?.is_some();
    let _ = in_index;
    let evidence = vec![
        DeepLinkTarget::File { path: path.into(), line: Some(1) }.to_link(),
        DeepLinkTarget::Blame { path: path.into(), line: Some(1) }.to_link(),
    ];
    Ok(Some(Answer {
        text: format!(
            "{} 共 {} 行。查看完整来历: gkl blame {} 1",
            path, lines, path
        ),
        evidence,
        engine: Engine::Heuristic,
    }))
}

fn hotspots(repo: &Repo, db: &Db) -> anyhow::Result<Answer> {
    if db.has_churn()? {
        let list = db.all_churn()?;
        let mut text = String::from("churn 热点(按变更次数,前 5):\n");
        let mut evidence = Vec::new();
        for (p, e) in list.iter().take(5) {
            text.push_str(&format!("  {}c +{} -{} {}\n", e.commits, e.added, e.deleted, p));
            evidence.push(DeepLinkTarget::Blame { path: p.clone(), line: Some(1) }.to_link());
        }
        Ok(Answer { text, evidence, engine: Engine::Heuristic })
    } else {
        let _ = repo;
        let authors = gkl_core::query::authors(db, 3)?;
        let mut text = String::from("churn 索引为空(运行 gkl scan --all 可得文件级热点)。当前提交数最多的作者:\n");
        for a in authors {
            text.push_str(&format!("  {} <{}>: {} 次提交\n", a.name, a.email, a.commits));
        }
        Ok(Answer { text, evidence: vec![], engine: Engine::Heuristic })
    }
}

fn top_authors(db: &Db) -> anyhow::Result<Answer> {
    let list = gkl_core::query::authors(db, 10)?;
    let mut text = String::from("作者排行(按提交数):\n");
    for (i, a) in list.iter().enumerate() {
        text.push_str(&format!("{}. {} <{}>: {}\n", i + 1, a.name, a.email, a.commits));
    }
    Ok(Answer { text, evidence: vec![], engine: Engine::Heuristic })
}
