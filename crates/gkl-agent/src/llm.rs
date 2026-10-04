//! LLM 引擎:OpenAI 兼容 chat/completions。
//!
//! 上下文构建策略:索引统计摘要 + 最近提交列表 + 问题,
//! 控制在 ~4K token,保证跨模型可用性与成本。

use crate::{Answer, Engine, LlmConfig};
use gkl_db::Db;
use gkl_git::repo::CommitMeta;

/// 用 LLM 回答考古问题。
pub fn ask(cfg: &LlmConfig, db: &Db, question: &str) -> anyhow::Result<Answer> {
    let context = build_context(db, 60)?;
    let system = "你是 git 历史考古助手。基于提供的仓库索引上下文回答问题,\
                  引用提交时给出前 7 位短 hash 与日期。上下文之外的回答要明确标注推测。";
    let user = format!("## 仓库索引上下文\n{}\n\n## 问题\n{}", context, question);

    let body = serde_json::json!({
        "model": cfg.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "temperature": 0.2
    });
    let resp: serde_json::Value = ureq::post(&format!("{}/chat/completions", cfg.base_url))
        .set("Authorization", &format!("Bearer {}", cfg.api_key))
        .send_json(body)?
        .into_json()?;

    let text = resp["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("(LLM 返回了空回答)")
        .to_string();
    Ok(Answer {
        text,
        evidence: vec![],
        engine: Engine::Llm,
    })
}

/// 组装给 LLM 的上下文摘要。
fn build_context(db: &Db, recent: usize) -> anyhow::Result<String> {
    let total = db.len()?;
    let mut s = format!("已索引提交总数: {}\n\n", total);

    let authors = gkl_core::query::authors(db, 8)?;
    s.push_str("作者排行(前 8):\n");
    for a in &authors {
        s.push_str(&format!(
            "- {} <{}>: {} commits\n",
            a.name, a.email, a.commits
        ));
    }

    let months = gkl_core::query::activity_by_month(db)?;
    if !months.is_empty() {
        s.push_str(&format!(
            "\n活动跨度: {} ~ {}\n",
            months.first().unwrap().0,
            months.last().unwrap().0
        ));
    }

    s.push_str("\n最近提交(倒序):\n");
    for m in recent_commits(db, recent)? {
        s.push_str(&format!(
            "- {} {} {}\n",
            &m.id[..7],
            date_str(m.author_time),
            m.message_subject
        ));
    }
    Ok(s)
}

fn recent_commits(db: &Db, n: usize) -> anyhow::Result<Vec<CommitMeta>> {
    gkl_core::query::log(
        db,
        &gkl_core::query::LogFilter {
            limit: n,
            ..Default::default()
        },
    )
}

/// Unix 秒 -> "YYYY-MM-DD"(UTC),无依赖日期格式化。
fn date_str(t: i64) -> String {
    let days = t.div_euclid(86400);
    // 复用 gkl-core 里已测试的 civil 算法(重复 3 行,避免跨 crate 暴露内部)。
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}
