//! 考古查询:log 过滤、churn 热点、作者统计。

use gkl_db::Db;
use gkl_git::repo::CommitMeta;
use std::collections::HashMap;

/// log 查询过滤器。
#[derive(Debug, Default, Clone)]
pub struct LogFilter {
    pub author: Option<String>,
    /// 子串匹配,大小写不敏感(同时匹配原始与 mailmap 归一身份)。
    pub grep: Option<String>,
    /// 把 grep 当正则(大小写不敏感);与 git log --grep 的 basic 正则不同,
    /// 这里用 Rust regex 方言(支持 SAD|satd 这类 OR)。
    pub grep_regex: bool,
    /// author_time 下界(Unix 秒)。
    pub since: Option<i64>,
    /// author_time 上界(Unix 秒)。
    pub until: Option<i64>,
    pub limit: usize,
}

/// 按过滤器匹配单条提交(grep 正则/子串,author 子串)。
fn matches(m: &CommitMeta, f: &LogFilter, re: Option<&regex::Regex>) -> bool {
    if let Some(a) = &f.author {
        let a = a.to_lowercase();
        if !(m.author_name.to_lowercase().contains(&a)
            || m.author_email.to_lowercase().contains(&a)
            || m.author_name_mapped.to_lowercase().contains(&a)
            || m.author_email_mapped.to_lowercase().contains(&a))
        {
            return false;
        }
    }
    if let Some(g) = &f.grep {
        let subject = m.message_subject.to_lowercase();
        let hit = match re {
            Some(re) => re.is_match(&subject),
            None => subject.contains(&g.to_lowercase()),
        };
        if !hit {
            return false;
        }
    }
    if let Some(s) = f.since {
        if m.author_time < s {
            return false;
        }
    }
    if let Some(u) = f.until {
        if m.author_time > u {
            return false;
        }
    }
    true
}

/// 按(倒序)过滤已索引提交。
pub fn log(db: &Db, f: &LogFilter) -> anyhow::Result<Vec<CommitMeta>> {
    // 正则只编译一次,158k 提交过滤仍在亚秒级。
    let re = match (&f.grep, f.grep_regex) {
        (Some(g), true) => Some(
            regex::RegexBuilder::new(g)
                .case_insensitive(true)
                .build()
                .map_err(|e| anyhow::anyhow!("--grep 正则无效: {e}"))?,
        ),
        _ => None,
    };
    let mut all = gkl_db::scan::commits_desc(db)?;
    all.retain(|m| matches(m, f, re.as_ref()));
    all.truncate(f.limit);
    Ok(all)
}

/// 单个作者的提交统计。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuthorStat {
    pub name: String,
    pub email: String,
    pub commits: u64,
    /// 首次提交时间(Unix 秒)。
    pub first: i64,
    /// 最近提交时间。
    pub last: i64,
}

/// 作者维度统计,按提交数降序。优先 mailmap 归一身份。
pub fn authors(db: &Db, limit: usize) -> anyhow::Result<Vec<AuthorStat>> {
    let all = db.all_commits()?;
    let mut by: HashMap<(String, String), AuthorStat> = HashMap::new();
    for m in all {
        // 归一身份优先;无 mailmap 时与原始一致。
        let key = (m.author_name_mapped.clone(), m.author_email_mapped.clone());
        let e = by.entry(key).or_insert(AuthorStat {
            name: m.author_name_mapped.clone(),
            email: m.author_email_mapped.clone(),
            commits: 0,
            first: m.author_time,
            last: m.author_time,
        });
        e.commits += 1;
        e.first = e.first.min(m.author_time);
        e.last = e.last.max(m.author_time);
    }
    let mut list: Vec<AuthorStat> = by.into_values().collect();
    list.sort_by_key(|a| std::cmp::Reverse(a.commits));
    list.truncate(limit);
    Ok(list)
}

/// 时间分布(按月),用于活动趋势。
pub fn activity_by_month(db: &Db) -> anyhow::Result<Vec<(String, u64)>> {
    let all = db.all_commits()?;
    let mut by: HashMap<String, u64> = HashMap::new();
    for m in all {
        // 从 Unix 秒直接取 UTC 年月,避免引入日期库的 API 变动风险。
        let days = m.author_time.div_euclid(86400);
        let ymd = civil_from_days(days);
        let key = format!("{:04}-{:02}", ymd.0, ymd.1);
        *by.entry(key).or_default() += 1;
    }
    let mut list: Vec<(String, u64)> = by.into_iter().collect();
    list.sort();
    Ok(list)
}

/// 天数 -> (年, 月, 日),Howard Hinnant 的 civil_from_days 算法。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1)); // 2024-01-01
    }
}
