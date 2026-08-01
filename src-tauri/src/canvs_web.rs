use crate::canvs::{CanvsSnapshot, CanvsTask, CanvsTaskList};

pub fn task_list_page(workspace_name: &str, list: &CanvsTaskList) -> String {
    let cards = if list.tasks.is_empty() {
        "<section class='empty'><h2>暂无任务</h2><p>该工作区还没有 Harness 当前任务或历史任务。</p></section>"
            .to_string()
    } else {
        list.tasks
            .iter()
            .map(task_card)
            .collect::<Vec<_>>()
            .join("")
    };
    page(
        &format!("{} · Canvs", escape_html(workspace_name)),
        &format!(
            "<header class='hero'><div><p class='kicker'>Anchor Canvs</p><h1>{}</h1><p class='subtitle'>当前任务与历史任务按更新时间倒序排列。每个入口只读取当前工作区的 Harness 数据。</p></div><div class='hero-meta'><span>{} 个任务</span><time data-time='{}'>{}</time></div></header><main class='cards'>{cards}</main>",
            escape_html(workspace_name),
            list.tasks.len(),
            escape_attr(&list.refreshed_at),
            escape_html(&list.refreshed_at),
        ),
        10_000,
    )
}

pub fn task_detail_page(workspace_name: &str, snapshot: &CanvsSnapshot) -> String {
    let Some(task) = snapshot.task.as_ref() else {
        return error_page(
            workspace_name,
            "任务不存在",
            "没有找到对应的 Harness 任务。",
        );
    };
    let completed = string_list(&task.completed_steps, "尚无已完成步骤", true);
    let pending = string_list(&task.pending_steps, "没有待处理步骤", false);
    let operations = if snapshot.recent_operations.is_empty() {
        empty_line("当前任务还没有操作记录")
    } else {
        snapshot
            .recent_operations
            .iter()
            .map(|operation| {
                format!(
                    "<article class='row'><div><strong>{}</strong><p>{} · <time data-time='{}'>{}</time></p></div><div class='row-end {}'><span>{}</span><small>{}</small></div></article>",
                    escape_html(&operation.tool),
                    escape_html(&operation.kind),
                    escape_attr(&operation.created_at),
                    escape_html(&operation.created_at),
                    outcome_class(operation.ok),
                    escape_html(&operation.status),
                    operation_meta(operation.affected_files, operation.duration_ms),
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let verifications = if snapshot.verifications.is_empty() {
        empty_line("当前任务还没有验证记录")
    } else {
        snapshot
            .verifications
            .iter()
            .map(|verification| {
                format!(
                    "<article class='row'><div class='grow'><code>{}</code><p>{} · {} · <time data-time='{}'>{}</time></p></div><div class='row-end {}'><span>{}</span><small>{}</small></div></article>",
                    escape_html(&verification.command),
                    escape_html(&verification.kind),
                    escape_html(&verification.level),
                    escape_attr(&verification.created_at),
                    escape_html(&verification.created_at),
                    if verification.passed { "ok" } else { "bad" },
                    escape_html(&disposition_label(&verification.disposition)),
                    verification_meta(verification.exit_code, verification.duration_ms),
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let changes = if snapshot.changes.is_empty() {
        empty_line("当前任务还没有分段提交")
    } else {
        snapshot
            .changes
            .iter()
            .map(|change| {
                let hash = change.commit_sha.as_deref().unwrap_or(&change.id);
                let files = if change.committed_files.is_empty() {
                    "没有提交文件".to_string()
                } else {
                    change
                        .committed_files
                        .iter()
                        .take(4)
                        .map(|file| escape_html(file))
                        .collect::<Vec<_>>()
                        .join(" · ")
                };
                format!(
                    "<article class='row'><div class='grow'><code>{}</code><p>{files}</p></div><div class='row-end'><time data-time='{}'>{}</time><small>{} 文件 · {} 验证</small></div></article>",
                    escape_html(&short_hash(hash)),
                    escape_attr(&change.created_at),
                    escape_html(&change.created_at),
                    change.committed_files.len(),
                    change.verification_count,
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let events = if snapshot.recent_events.is_empty() {
        empty_line("当前任务还没有事件记录")
    } else {
        snapshot
            .recent_events
            .iter()
            .map(|event| {
                format!(
                    "<article class='row'><div><strong>{}</strong><p>{} · <time data-time='{}'>{}</time></p></div><div class='row-end {}'><span>{}</span></div></article>",
                    escape_html(&event_kind_label(&event.kind)),
                    escape_html(event.tool_name.as_deref().unwrap_or("Harness")),
                    escape_attr(&event.created_at),
                    escape_html(&event.created_at),
                    outcome_class(event.ok),
                    if event.affected_files > 0 {
                        format!("{} 文件", event.affected_files)
                    } else if event.ok == Some(false) {
                        "失败".into()
                    } else {
                        String::new()
                    },
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let current_badge = if task.current {
        "<span class='badge current'>当前任务</span>"
    } else {
        "<span class='badge history'>历史任务</span>"
    };
    let total_steps = task.completed_steps.len() + task.pending_steps.len();
    let body = format!(
        "<nav><a href='../../canvs'>← 返回任务列表</a><span>{}</span></nav><header class='detail-hero'><div class='badges'>{current_badge}<span class='badge {}'>{}</span></div><h1>{}</h1><p class='task-id'>{}</p><div class='progress-line'><div><span style='width:{}%'></span></div><strong>{}%</strong><small>{}/{} 步骤</small></div><p class='subtitle'>更新于 <time data-time='{}'>{}</time> · 分支 {} · HEAD {}</p></header><main class='detail-grid'><section class='panel wide'><h2>步骤</h2><div class='columns'><div><h3>已完成</h3>{completed}</div><div><h3>待处理</h3>{pending}</div></div></section><section class='panel'><h2>任务基线</h2><dl class='facts'><div><dt>初始 HEAD</dt><dd><code>{}</code></dd></div><div><dt>当前预期 HEAD</dt><dd><code>{}</code></dd></div><div><dt>最新变更</dt><dd><code>{}</code></dd></div><div><dt>最新验证</dt><dd><code>{}</code></dd></div></dl></section><section class='panel'><h2>最近操作</h2>{operations}</section><section class='panel'><h2>有效验证</h2>{verifications}</section><section class='panel'><h2>分段提交</h2>{changes}</section><section class='panel'><h2>任务事件</h2>{events}</section></main>",
        escape_html(workspace_name),
        status_class(&task.status),
        escape_html(&status_label(&task.status)),
        escape_html(&task.objective),
        escape_html(&task.id),
        task.progress_percent,
        task.progress_percent,
        task.completed_steps.len(),
        total_steps,
        escape_attr(&task.updated_at),
        escape_html(&task.updated_at),
        escape_html(task.branch.as_deref().unwrap_or("—")),
        escape_html(&short_hash(task.expected_head.as_deref().unwrap_or(""))),
        escape_html(&short_hash(task.head.as_deref().unwrap_or(""))),
        escape_html(&short_hash(task.expected_head.as_deref().unwrap_or(""))),
        escape_html(task.latest_change_id.as_deref().unwrap_or("—")),
        escape_html(task.latest_verification_id.as_deref().unwrap_or("—")),
    );
    page(
        &format!("{} · Canvs", escape_html(&task.objective)),
        &body,
        if task.current { 5_000 } else { 0 },
    )
}

pub fn unauthorized_page(workspace_name: &str) -> String {
    page(
        "Canvs 需要认证",
        &format!(
            "<main class='center-card'><p class='kicker'>Anchor Canvs</p><h1>需要认证</h1><p>请输入工作区“{}”的 MCP 授权口令。Bearer 模式下请输入 Bearer Token。</p><p class='muted'>浏览器会使用 HTTP Basic Authentication；公网入口必须通过 HTTPS 访问。</p></main>",
            escape_html(workspace_name),
        ),
        0,
    )
}

pub fn error_page(workspace_name: &str, title: &str, message: &str) -> String {
    page(
        title,
        &format!(
            "<nav><a href='../../canvs'>← 返回任务列表</a><span>{}</span></nav><main class='center-card'><p class='kicker'>Anchor Canvs</p><h1>{}</h1><p>{}</p></main>",
            escape_html(workspace_name),
            escape_html(title),
            escape_html(message),
        ),
        0,
    )
}

fn task_card(task: &CanvsTask) -> String {
    let current = if task.current {
        "<span class='badge current'>当前任务</span>"
    } else {
        "<span class='badge history'>历史任务</span>"
    };
    let total = task.completed_steps.len() + task.pending_steps.len();
    format!(
        "<a class='task-card' href='./canvs/tasks/{}'><div class='badges'>{current}<span class='badge {}'>{}</span></div><h2>{}</h2><p class='task-id'>{}</p><div class='progress-line'><div><span style='width:{}%'></span></div><strong>{}%</strong><small>{}/{} 步骤</small></div><footer><span>更新于 <time data-time='{}'>{}</time></span><span>{} · {}</span></footer></a>",
        escape_attr(&task.id),
        status_class(&task.status),
        escape_html(&status_label(&task.status)),
        escape_html(&task.objective),
        escape_html(&task.id),
        task.progress_percent,
        task.progress_percent,
        task.completed_steps.len(),
        total,
        escape_attr(&task.updated_at),
        escape_html(&task.updated_at),
        escape_html(task.branch.as_deref().unwrap_or("—")),
        escape_html(&short_hash(task.expected_head.as_deref().unwrap_or(""))),
    )
}

fn string_list(values: &[String], empty: &str, completed: bool) -> String {
    if values.is_empty() {
        return empty_line(empty);
    }
    format!(
        "<ol class='steps'>{}</ol>",
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                format!(
                    "<li><span>{}</span><p>{}</p></li>",
                    if completed {
                        "✓".to_string()
                    } else {
                        (index + 1).to_string()
                    },
                    escape_html(value),
                )
            })
            .collect::<Vec<_>>()
            .join("")
    )
}

fn page(title: &str, body: &str, refresh_ms: u64) -> String {
    let refresh_script = if refresh_ms > 0 {
        format!(
            "const key='anchor-canvs-scroll:'+location.pathname;addEventListener('beforeunload',()=>sessionStorage.setItem(key,String(scrollY)));const saved=sessionStorage.getItem(key);if(saved)requestAnimationFrame(()=>scrollTo(0,Number(saved)));setInterval(()=>{{if(!document.hidden)location.reload();}},{refresh_ms});"
        )
    } else {
        String::new()
    };
    format!(
        "<!doctype html><html lang='zh-CN'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'><meta name='color-scheme' content='light dark'><title>{}</title><style>{}</style></head><body>{body}<script>{}document.querySelectorAll('time[data-time]').forEach((node)=>{{const raw=node.dataset.time||'';let date;if(raw.startsWith('unix:'))date=new Date(Number(raw.slice(5))*1000);else if(/^\\d{{13}}$/.test(raw))date=new Date(Number(raw));else if(/^\\d{{10}}$/.test(raw))date=new Date(Number(raw)*1000);else date=new Date(raw);if(!Number.isNaN(date.getTime()))node.textContent=date.toLocaleString();}});</script></body></html>",
        escape_html(title),
        STYLES,
        refresh_script,
    )
}

fn status_label(status: &str) -> String {
    match status {
        "active" => "进行中",
        "paused" => "已暂停",
        "verifying" => "验证中",
        "failed" => "失败",
        "completed" => "已完成",
        "completed_unverified" => "完成未验证",
        "rolled_back" => "已回滚",
        _ => "未知",
    }
    .into()
}

fn status_class(status: &str) -> &'static str {
    match status {
        "active" | "completed" => "ok",
        "verifying" => "warn",
        "failed" => "bad",
        _ => "neutral",
    }
}

fn outcome_class(ok: Option<bool>) -> &'static str {
    match ok {
        Some(true) => "ok",
        Some(false) => "bad",
        None => "neutral",
    }
}

fn disposition_label(disposition: &str) -> String {
    match disposition {
        "passed" => "通过",
        "active_failure" | "failed" => "失败",
        "diagnostic_only" => "诊断",
        "expected_failure" => "预期失败",
        "superseded" => "已取代",
        "waived" => "已豁免",
        other => other,
    }
    .into()
}

fn event_kind_label(kind: &str) -> String {
    match kind {
        "task_auto_resumed" => "任务自动恢复",
        "task_status_changed" => "任务状态变更",
        "operation_started" => "操作开始",
        "operation_finished" => "操作完成",
        "proxy_operation_finished" => "代理操作完成",
        other => other,
    }
    .into()
}

fn operation_meta(files: usize, duration_ms: Option<u64>) -> String {
    let mut values = Vec::new();
    if files > 0 {
        values.push(format!("{files} 文件"));
    }
    if let Some(duration_ms) = duration_ms {
        values.push(format_duration(duration_ms));
    }
    escape_html(&values.join(" · "))
}

fn verification_meta(exit_code: Option<i32>, duration_ms: Option<u64>) -> String {
    let mut values = Vec::new();
    if let Some(exit_code) = exit_code {
        values.push(format!("退出码 {exit_code}"));
    }
    if let Some(duration_ms) = duration_ms {
        values.push(format_duration(duration_ms));
    }
    escape_html(&values.join(" · "))
}

fn format_duration(duration_ms: u64) -> String {
    if duration_ms < 1_000 {
        format!("{duration_ms} ms")
    } else if duration_ms < 10_000 {
        format!("{:.1} s", duration_ms as f64 / 1_000.0)
    } else {
        format!("{} s", duration_ms / 1_000)
    }
}

fn short_hash(value: &str) -> String {
    if value.is_empty() {
        "—".into()
    } else {
        value.chars().take(10).collect()
    }
}

fn empty_line(message: &str) -> String {
    format!("<p class='muted'>{}</p>", escape_html(message))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn escape_attr(value: &str) -> String {
    escape_html(value)
}

const STYLES: &str = r#"
:root{font-family:Inter,ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;color:#172033;background:#f3f6fb;font-synthesis:none}*{box-sizing:border-box}body{margin:0;min-height:100vh;background:radial-gradient(circle at top left,#e9f4ff 0,transparent 34rem),#f3f6fb;color:#172033}a{color:inherit}nav,.hero,.detail-hero,.cards,.detail-grid,.center-card{width:min(1180px,calc(100% - 32px));margin-inline:auto}nav{display:flex;justify-content:space-between;gap:16px;padding:24px 0 8px;color:#64748b;font-size:14px}nav a{text-decoration:none;font-weight:700;color:#2563eb}.hero,.detail-hero,.center-card{margin-top:24px;border:1px solid #dbe4f0;border-radius:24px;background:rgba(255,255,255,.88);box-shadow:0 18px 60px rgba(30,64,175,.08);padding:28px}.hero{display:flex;justify-content:space-between;gap:28px;align-items:flex-start}.hero h1,.detail-hero h1,.center-card h1{margin:6px 0 8px;font-size:clamp(28px,4vw,44px);line-height:1.08}.kicker{margin:0;color:#2563eb;font-weight:800;letter-spacing:.14em;text-transform:uppercase;font-size:12px}.subtitle,.muted,.row p,.task-card footer,.task-id{color:#64748b}.subtitle{margin:0;line-height:1.7}.hero-meta{display:grid;justify-items:end;gap:8px;white-space:nowrap;color:#64748b;font-size:13px}.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(310px,1fr));gap:16px;padding:20px 0 40px}.task-card{display:block;text-decoration:none;border:1px solid #dbe4f0;border-radius:20px;background:#fff;padding:20px;box-shadow:0 10px 30px rgba(15,23,42,.05);transition:transform .16s ease,border-color .16s ease,box-shadow .16s ease}.task-card:hover{transform:translateY(-3px);border-color:#93b4f8;box-shadow:0 18px 36px rgba(37,99,235,.12)}.task-card h2{margin:14px 0 8px;font-size:17px;line-height:1.55}.task-id{margin:0;font:12px ui-monospace,SFMono-Regular,Menlo,monospace;overflow-wrap:anywhere}.badges{display:flex;flex-wrap:wrap;gap:8px}.badge{display:inline-flex;border:1px solid #dbe4f0;border-radius:999px;padding:5px 9px;font-size:12px;font-weight:750}.badge.current{color:#1d4ed8;background:#eff6ff;border-color:#bfdbfe}.badge.history,.badge.neutral{color:#475569;background:#f8fafc}.badge.ok,.row-end.ok{color:#15803d}.badge.ok{background:#f0fdf4;border-color:#bbf7d0}.badge.warn,.row-end.warn{color:#a16207}.badge.warn{background:#fffbeb;border-color:#fde68a}.badge.bad,.row-end.bad{color:#b91c1c}.badge.bad{background:#fef2f2;border-color:#fecaca}.progress-line{display:grid;grid-template-columns:minmax(100px,1fr) auto auto;align-items:center;gap:10px;margin-top:18px}.progress-line>div{height:8px;overflow:hidden;border-radius:999px;background:#e8eef7}.progress-line>div>span{display:block;height:100%;border-radius:inherit;background:linear-gradient(90deg,#2563eb,#06b6d4)}.progress-line strong{font-size:13px}.progress-line small{color:#64748b}.task-card footer{display:flex;justify-content:space-between;gap:12px;margin-top:18px;padding-top:14px;border-top:1px solid #edf1f7;font-size:12px}.detail-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:16px;padding:16px 0 40px}.panel{min-width:0;border:1px solid #dbe4f0;border-radius:20px;background:#fff;padding:20px;box-shadow:0 10px 30px rgba(15,23,42,.04)}.panel.wide{grid-column:1/-1}.panel h2{margin:0 0 16px;font-size:17px}.panel h3{margin:0 0 12px;font-size:13px;color:#64748b;text-transform:uppercase;letter-spacing:.08em}.columns{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:24px}.steps{display:grid;gap:10px;margin:0;padding:0;list-style:none}.steps li{display:flex;gap:10px;line-height:1.55}.steps li span{display:grid;place-items:center;flex:0 0 22px;height:22px;border-radius:999px;background:#eef4ff;color:#2563eb;font-size:12px;font-weight:800}.steps li p{margin:0}.facts{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:10px}.facts div{border:1px solid #e7edf5;border-radius:14px;background:#f8fafc;padding:12px}.facts dt{color:#64748b;font-size:12px}.facts dd{margin:6px 0 0;overflow-wrap:anywhere}.row{display:flex;justify-content:space-between;gap:16px;padding:12px 0;border-top:1px solid #edf1f7}.row:first-of-type{border-top:0}.row strong{font-size:14px}.row p{margin:5px 0 0;font-size:12px}.row code{display:block;white-space:pre-wrap;overflow-wrap:anywhere;font-size:12px}.grow{min-width:0;flex:1}.row-end{display:grid;justify-items:end;align-content:start;gap:5px;flex:0 0 auto;font-size:12px}.row-end small{color:#64748b}.empty,.center-card{text-align:center}.empty{grid-column:1/-1;border:1px dashed #cbd5e1;border-radius:20px;padding:48px;background:rgba(255,255,255,.68)}.center-card{max-width:620px;margin-top:12vh}.center-card p{line-height:1.7}@media(max-width:760px){.hero{display:grid}.hero-meta{justify-items:start}.detail-grid,.columns{grid-template-columns:1fr}.panel.wide{grid-column:auto}.task-card footer{display:grid}.facts{grid-template-columns:1fr}.row{display:grid}.row-end{justify-items:start}.progress-line{grid-template-columns:1fr auto}.progress-line small{grid-column:1/-1}}
@media(prefers-color-scheme:dark){:root{color:#e5edf8;background:#0b1220}body{background:radial-gradient(circle at top left,#172a46 0,transparent 34rem),#0b1220;color:#e5edf8}.hero,.detail-hero,.center-card,.task-card,.panel{background:rgba(15,23,42,.94);border-color:#263449}.empty{background:rgba(15,23,42,.65);border-color:#334155}.subtitle,.muted,.row p,.task-card footer,.task-id,.hero-meta,.row-end small,nav{color:#94a3b8}.task-card:hover{border-color:#3b82f6}.badge.history,.badge.neutral,.facts div{background:#111c2f;border-color:#2b3a52;color:#cbd5e1}.progress-line>div{background:#243248}.task-card footer,.row{border-color:#243248}.panel h3,.facts dt{color:#94a3b8}.facts div{background:#101a2d}}
"#;

#[cfg(test)]
mod tests {
    use super::{escape_html, task_list_page};
    use crate::canvs::{CanvsTask, CanvsTaskList};

    #[test]
    fn html_escaping_blocks_task_content_injection() {
        assert_eq!(
            escape_html("<script>'x'</script>"),
            "&lt;script&gt;&#39;x&#39;&lt;/script&gt;"
        );
    }

    #[test]
    fn task_list_renders_workspace_scoped_card_links() {
        let html = task_list_page(
            "workspace",
            &CanvsTaskList {
                workspace_id: "workspace-id".into(),
                tasks: vec![CanvsTask {
                    id: "task-id".into(),
                    objective: "objective".into(),
                    status: "active".into(),
                    current: true,
                    completed_steps: Vec::new(),
                    pending_steps: Vec::new(),
                    progress_percent: 0,
                    branch: None,
                    head: None,
                    expected_head: None,
                    latest_change_id: None,
                    latest_verification_id: None,
                    created_at: "1".into(),
                    updated_at: "2".into(),
                }],
                refreshed_at: "2".into(),
            },
        );
        assert!(html.contains("href='./canvs/tasks/task-id'"));
        assert!(html.contains("当前任务"));
    }
}
