//! clap 命令定义与执行。

use anyhow::Context;
use clap::{Parser, Subcommand};
use gkl_core::deeplink::{self, DeepLinkTarget};
use gkl_db::Db;
use gkl_git::Repo;

#[derive(Parser)]
#[command(
    name = "gkl",
    version,
    about = "git 历史考古:索引、查询、AI 问答、深链接"
)]
struct Cli {
    /// 仓库路径(默认当前目录)。
    #[arg(long, global = true)]
    repo: Option<String>,
    /// 索引文件路径(默认 <repo>/.git/gkl/index.redb)。
    #[arg(long, global = true)]
    db: Option<String>,
    /// 输出 JSON(供脚本/agent 消费)。
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 建立/更新索引(增量)。
    Scan {
        /// 忽略增量,全量重建。
        #[arg(long)]
        force: bool,
        /// 全历史拓扑(含侧链;顺带重建文件 churn 统计)。
        #[arg(long)]
        all: bool,
    },
    /// 提交日志(走索引,支持过滤)。
    Log {
        #[arg(long)]
        author: Option<String>,
        #[arg(long)]
        grep: Option<String>,
        #[arg(long, default_value = "30")]
        limit: usize,
    },
    /// 文件级 churn 热点(需 scan --all 过)。
    Churn {
        #[arg(long, default_value = "20")]
        limit: usize,
        /// 只看新增+删除行数(而不是变更次数)。
        #[arg(long)]
        by_lines: bool,
    },
    /// 作者统计。
    Authors {
        #[arg(long, default_value = "15")]
        limit: usize,
    },
    /// 月度活动分布。
    Activity,
    /// 查看提交详情。
    Show { id: String },
    /// 文件 blame(轻量,第一父链回溯)。
    Blame { path: String, line: usize },
    /// AI 问答(有 GKL_OPENAI_API_KEY 走 LLM,否则启发式)。
    Ask { question: String },
    /// 生成 gkl:// 深链接。
    Link {
        #[command(subcommand)]
        target: LinkTarget,
    },
    /// 解析 gkl:// 链接为可执行查询。
    Resolve { link: String },
}

#[derive(Subcommand)]
enum LinkTarget {
    /// 链接到提交。
    Commit { id: String },
    /// 链接到文件。
    File {
        path: String,
        #[arg(long)]
        line: Option<u32>,
    },
    /// 链接到 blame 视图。
    Blame {
        path: String,
        #[arg(long)]
        line: Option<u32>,
    },
    /// 链接到搜索。
    Search { query: String },
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let repo = match &cli.repo {
        Some(p) => Repo::open(p)?,
        None => Repo::open(".")?,
    };
    let db_path = cli.db.clone().unwrap_or_else(|| {
        // 索引放 .git/ 下,天然随仓库走、不污染工作区。
        format!("{}/.git/gkl/index.redb", repo_workdir(&repo))
    });
    let db = Db::open(&db_path).context("打开索引数据库")?;

    match cli.cmd {
        Cmd::Scan { force, all } => {
            let r = gkl_db::scan::scan(&repo, &db, force, all)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({"new": r.new_commits, "total": r.total, "tip": r.tip, "all": r.all_history})
                );
            } else {
                let mode = if r.all_history { "全历史" } else { "主线" };
                println!(
                    "已索引 {} 个提交({}模式,新增 {},tip {})",
                    r.total,
                    mode,
                    r.new_commits,
                    &r.tip[..7]
                );
            }
        }
        Cmd::Log {
            author,
            grep,
            limit,
        } => {
            ensure_scanned(&db)?;
            let list = gkl_core::query::log(
                &db,
                &gkl_core::query::LogFilter {
                    author,
                    grep,
                    limit,
                    ..Default::default()
                },
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else {
                for m in list {
                    println!(
                        "{} {} {:<12} {}",
                        &m.id[..7],
                        date_str(m.author_time),
                        m.author_name,
                        m.message_subject
                    );
                }
            }
        }
        Cmd::Churn { limit, by_lines } => {
            if !db.has_churn()? {
                anyhow::bail!("churn 索引为空,先运行: gkl scan --all");
            }
            let mut list = db.all_churn()?;
            if by_lines {
                list.sort_by_key(|(_, e)| std::cmp::Reverse(e.added + e.deleted));
            }
            list.truncate(limit);
            if cli.json {
                let out: Vec<serde_json::Value> = list
                    .iter()
                    .map(|(p, e)| {
                        serde_json::json!({"path": p, "commits": e.commits, "added": e.added, "deleted": e.deleted})
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                for (p, e) in list {
                    println!("{:>6}c +{:<6} -{:<6} {}", e.commits, e.added, e.deleted, p);
                }
            }
        }
        Cmd::Authors { limit } => {
            ensure_scanned(&db)?;
            let list = gkl_core::query::authors(&db, limit)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else {
                for a in list {
                    println!("{:>6}  {} <{}>", a.commits, a.name, a.email);
                }
            }
        }
        Cmd::Activity => {
            ensure_scanned(&db)?;
            let months = gkl_core::query::activity_by_month(&db)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&months)?);
            } else {
                let max = months.iter().map(|(_, c)| *c).max().unwrap_or(1);
                for (k, v) in months {
                    let bar = "#".repeat(((v as f64 / max as f64) * 40.0) as usize);
                    println!("{} {:>5} {}", k, v, bar);
                }
            }
        }
        Cmd::Show { id } => {
            let m = repo.commit_meta(&id)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&m)?);
            } else {
                println!("commit {}", m.id);
                println!("Author: {} <{}>", m.author_name, m.author_email);
                println!(
                    "Date:   {} ({:+03}:{:02})",
                    date_str(m.author_time),
                    m.author_tz_offset / 60,
                    m.author_tz_offset.abs() % 60
                );
                debug_assert!((-720..=840).contains(&m.author_tz_offset), "tz 应为分钟制");
                if m.is_merge {
                    println!("Merge: {}", m.parents.join(" "));
                }
                println!();
                println!("    {}", m.message_subject);
                println!();
                println!(
                    "深链接: {}",
                    DeepLinkTarget::Commit { id: m.id.clone() }.to_link()
                );
            }
        }
        Cmd::Blame { path, line } => {
            let hit = gkl_git::blame::blame_line(&repo, &path, line)?;
            match hit {
                Some(h) => {
                    let m = repo.commit_meta(&h.commit)?;
                    let chain = if h.path_chain.len() > 1 {
                        format!("\n路径链: {}", h.path_chain.join(" <- "))
                    } else {
                        String::new()
                    };
                    if cli.json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "path": path, "line": line,
                                "commit": h.commit, "author": m.author_name,
                                "date": date_str(m.author_time), "subject": m.message_subject,
                                "path_chain": h.path_chain,
                                "link": DeepLinkTarget::Blame { path: path.clone(), line: Some(line as u32) }.to_link(),
                            })
                        );
                    } else {
                        println!(
                            "{} {} ({} {})",
                            &h.commit[..7],
                            m.message_subject,
                            m.author_name,
                            date_str(m.author_time)
                        );
                        println!("{}", chain.trim_end_matches('\n'));
                        println!(
                            "深链接: {}",
                            DeepLinkTarget::Blame {
                                path: path.clone(),
                                line: Some(line as u32)
                            }
                            .to_link()
                        );
                    }
                }
                None => println!("未找到 blame"),
            }
        }
        Cmd::Ask { question } => {
            ensure_scanned(&db)?;
            let answer = match gkl_agent::LlmConfig::from_env() {
                Some(cfg) => gkl_agent::llm::ask(&cfg, &db, &question)?,
                None => gkl_agent::offline::answer(&repo, &db, &question)?.unwrap_or_else(|| {
                    gkl_agent::Answer {
                        text: "无法识别的问题。离线模式支持: 谁写的 <path> / 热点 / 作者排行。\
                               设置 GKL_OPENAI_API_KEY 启用 LLM 问答。"
                            .into(),
                        evidence: vec![],
                        engine: gkl_agent::Engine::Heuristic,
                    }
                }),
            };
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&answer)?);
            } else {
                println!("{}", answer.text);
                for e in &answer.evidence {
                    println!("证据: {}", e);
                }
            }
        }
        Cmd::Link { target } => {
            let t = match target {
                LinkTarget::Commit { id } => DeepLinkTarget::Commit { id: repo.rev(&id)? },
                LinkTarget::File { path, line } => DeepLinkTarget::File { path, line },
                LinkTarget::Blame { path, line } => DeepLinkTarget::Blame { path, line },
                LinkTarget::Search { query } => DeepLinkTarget::Search { query },
            };
            println!("{}", t.to_link());
        }
        Cmd::Resolve { link } => {
            let t = deeplink::parse(&link)?;
            if cli.json {
                println!("{}", serde_json::json!({"target": format!("{:?}", t)}));
            } else {
                match t {
                    DeepLinkTarget::Commit { id } => {
                        println!("指向提交,运行查看: gkl show {}", id);
                    }
                    DeepLinkTarget::File { path, line } => {
                        println!("指向文件 {} 第 {:?} 行", path, line);
                    }
                    DeepLinkTarget::Blame { path, line } => {
                        let line = line.unwrap_or(1);
                        println!("解析为 blame,执行中...");
                        let hit = gkl_git::blame::blame_line(&repo, &path, line as usize)?;
                        if let Some(h) = hit {
                            let m = repo.commit_meta(&h.commit)?;
                            println!(
                                "{} {} ({} {})",
                                &h.commit[..7],
                                m.message_subject,
                                m.author_name,
                                date_str(m.author_time)
                            );
                        } else {
                            println!("未找到");
                        }
                    }
                    DeepLinkTarget::Search { query } => {
                        println!("指向搜索,运行: gkl log --grep {}", query);
                    }
                }
            }
        }
    }
    Ok(())
}

fn ensure_scanned(db: &Db) -> anyhow::Result<()> {
    if db.is_empty()? {
        anyhow::bail!("索引为空,先运行: gkl scan");
    }
    Ok(())
}

fn repo_workdir(repo: &Repo) -> String {
    // 索引放 .git/ 下,天然随仓库走、不污染工作区。bare 仓库则退到当前目录。
    match repo.workdir() {
        Some(p) => p
            .to_string_lossy()
            .trim_end_matches('\\')
            .trim_end_matches('/')
            .to_string(),
        None => ".".into(),
    }
}

/// Unix 秒 -> YYYY-MM-DD(UTC)。
fn date_str(t: i64) -> String {
    let days = t.div_euclid(86400);
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
